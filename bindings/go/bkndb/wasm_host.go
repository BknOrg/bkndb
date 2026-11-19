//go:build bkndb_wasm && ((linux && !android && (amd64 || arm64)) || (darwin && !ios && (amd64 || arm64)) || (windows && amd64))

package bkndb

import (
	"context"
	"errors"
	"fmt"
	"io"
	"math"
	"os"
	"path/filepath"

	"github.com/tetratelabs/wazero/api"
)

type hostFile interface {
	ReadAt([]byte, int64) (int, error)
	WriteAt([]byte, int64) (int, error)
	Truncate(int64) error
	Sync() error
	Close() error
	size() (int64, error)
	lock() error
}
type diskHostFile struct{ *os.File }

func (f *diskHostFile) size() (int64, error) {
	s, e := f.Stat()
	if e != nil {
		return 0, e
	}
	return s.Size(), nil
}
func (f *diskHostFile) lock() error { return lockHostFile(f.File) }

type wasmHost struct {
	module      api.Module
	paths       map[uint32]string
	files       map[uint32]hostFile
	next        uint32
	memory      bool
	nodes       map[uint32]*memoryNode
	memoryLimit uint64
}

func newWASMHost(path string, memory bool, limit uint64) (*wasmHost, error) {
	h := &wasmHost{paths: map[uint32]string{}, files: map[uint32]hostFile{}, memory: memory, nodes: map[uint32]*memoryNode{}, memoryLimit: limit}
	if !memory {
		if path == "" {
			return nil, errors.New("database path is empty")
		}
		p, e := filepath.Abs(path)
		if e != nil {
			return nil, e
		}
		if e = makeHostDirs(filepath.Dir(p)); e != nil {
			return nil, e
		}
		p, e = canonicalHostPath(p)
		if e != nil {
			return nil, e
		}
		h.paths[1] = p
		h.paths[2] = p + ".lock"
		h.paths[3] = p + ".compact.tmp"
	}
	return h, nil
}
func (h *wasmHost) setBackup(dest string) error {
	if h.memory {
		return errors.New("backup needs an on-disk database")
	}
	p, e := filepath.Abs(dest)
	if e != nil {
		return e
	}
	if dest == "" {
		return errors.New("backup path is empty")
	}
	for role := uint32(1); role <= 3; role++ {
		if p == h.paths[role] || p+".backup.tmp" == h.paths[role] {
			return errors.New("backup conflicts with database operational files")
		}
	}
	if e = makeHostDirs(filepath.Dir(p)); e != nil {
		return e
	}
	h.paths[4] = p
	h.paths[5] = p + ".backup.tmp"
	return nil
}
func (h *wasmHost) closeAll() {
	for id, f := range h.files {
		_ = f.Close()
		delete(h.files, id)
	}
	h.nodes = nil
}
func errno(e error) int64 {
	if e == nil {
		return 0
	}
	if errors.Is(e, errHostLocked) {
		return -2
	}
	if errors.Is(e, os.ErrExist) {
		return -3
	}
	if errors.Is(e, os.ErrNotExist) {
		return -4
	}
	if errors.Is(e, os.ErrInvalid) {
		return -5
	}
	return -1
}

var errHostLocked = errors.New("database already locked")

func (h *wasmHost) open(role uint32, mode uint64) (uint32, error) {
	if role < 1 || role > 5 || mode > 2 {
		return 0, os.ErrInvalid
	}
	if h.next == ^uint32(0) {
		return 0, errors.New("file handle space exhausted")
	}
	var f hostFile
	var e error
	if h.memory {
		n := h.nodes[role]
		if mode == 2 && n != nil {
			return 0, os.ErrExist
		}
		if mode == 1 && n == nil {
			return 0, os.ErrNotExist
		}
		if n == nil {
			n = &memoryNode{}
			h.nodes[role] = n
		}
		f = &memoryHostFile{node: n, host: h}
	} else {
		p := h.paths[role]
		if p == "" {
			return 0, os.ErrInvalid
		}
		f, e = openHostFile(p, mode)
		if e != nil {
			return 0, e
		}
	}
	h.next++
	if h.next == 0 {
		_ = f.Close()
		return 0, errors.New("file handle space exhausted")
	}
	h.files[h.next] = f
	return h.next, nil
}

// All imports run under the instance mutex. Every guest memory range is checked
// before host I/O. The role table is immutable except for an explicit Backup call.
func (h *wasmHost) ioCall(ctx context.Context, m api.Module, op, handle uint32, offset uint64, ptr, length uint32) int64 {
	if ctx.Err() != nil {
		return -1
	}
	if offset > math.MaxInt64 || length > 64<<20 {
		return -5
	}
	f := h.files[handle]
	switch op {
	case 0:
		return 31
	case 1:
		id, e := h.open(handle, offset)
		if e != nil {
			return errno(e)
		}
		return int64(id)
	case 2:
		if f == nil {
			return -5
		}
		delete(h.files, handle)
		return errno(f.Close())
	case 3, 4:
		if f == nil || offset > math.MaxInt64-uint64(length) {
			return -5
		}
		buf, ok := m.Memory().Read(ptr, length)
		if !ok {
			return -5
		}
		var n int
		var e error
		if op == 3 {
			n, e = f.ReadAt(buf, int64(offset))
			if errors.Is(e, io.EOF) {
				e = nil
			}
		} else {
			n, e = f.WriteAt(buf, int64(offset))
		}
		if n > 0 {
			return int64(n)
		}
		if e != nil {
			return errno(e)
		}
		return 0
	case 5:
		if f == nil {
			return -5
		}
		n, e := f.size()
		if e != nil {
			return errno(e)
		}
		return n
	case 6:
		if f == nil {
			return -5
		}
		return errno(f.Truncate(int64(offset)))
	case 7:
		if f == nil {
			return -5
		}
		return errno(f.Sync())
	case 8:
		if f == nil {
			return -5
		}
		return errno(f.lock())
	case 9:
		if h.memory {
			if h.nodes[handle] != nil {
				return 1
			}
			return 0
		}
		p := h.paths[handle]
		if p == "" {
			return -5
		}
		_, e := os.Stat(p)
		if e == nil {
			return 1
		}
		if errors.Is(e, os.ErrNotExist) {
			return 0
		}
		return errno(e)
	case 10:
		if handle != 3 && handle != 5 {
			return -5
		}
		if h.memory {
			delete(h.nodes, handle)
			return 0
		}
		if h.paths[handle] == "" {
			return -5
		}
		return errno(os.Remove(h.paths[handle]))
	case 11, 12:
		dst := uint32(offset)
		if (op == 11 && (handle != 3 || dst != 1)) || (op == 12 && (handle != 5 || dst != 4)) {
			return -5
		}
		if h.memory {
			n := h.nodes[handle]
			if n == nil {
				return -4
			}
			if op == 12 && h.nodes[dst] != nil {
				return -3
			}
			h.nodes[dst] = n
			delete(h.nodes, handle)
			return 0
		}
		if h.paths[handle] == "" || h.paths[dst] == "" {
			return -5
		}
		if op == 11 {
			return errno(replaceHostFile(h.paths[handle], h.paths[dst], true))
		}
		return errno(publishHostFile(h.paths[handle], h.paths[dst]))
	case 13:
		if h.memory {
			return 0
		}
		p := h.paths[handle]
		if p == "" {
			return -5
		}
		return errno(syncHostParent(p))
	}
	return -5
}

type memoryNode struct {
	bytes  []byte
	locked bool
}
type memoryHostFile struct {
	node   *memoryNode
	host   *wasmHost
	locked bool
}

func (f *memoryHostFile) ReadAt(b []byte, off int64) (int, error) {
	if off < 0 {
		return 0, os.ErrInvalid
	}
	if off >= int64(len(f.node.bytes)) {
		return 0, io.EOF
	}
	n := copy(b, f.node.bytes[off:])
	if n < len(b) {
		return n, io.EOF
	}
	return n, nil
}
func (f *memoryHostFile) WriteAt(b []byte, off int64) (int, error) {
	if off < 0 || off > math.MaxInt64-int64(len(b)) {
		return 0, os.ErrInvalid
	}
	end := off + int64(len(b))
	if end > int64(len(f.node.bytes)) {
		if e := f.Truncate(end); e != nil {
			return 0, e
		}
	}
	return copy(f.node.bytes[off:end], b), nil
}
func (f *memoryHostFile) Truncate(n int64) error {
	if n < 0 || uint64(n) > f.host.memoryLimit {
		return fmt.Errorf("ResourceLimit: in-memory storage limit")
	}
	if n > int64(len(f.node.bytes)) {
		seen := map[*memoryNode]bool{}
		var used uint64
		for _, v := range f.host.files {
			if v, ok := v.(*memoryHostFile); ok && !seen[v.node] {
				seen[v.node] = true
				used += uint64(len(v.node.bytes))
			}
		}
		for _, v := range f.host.nodes {
			if !seen[v] {
				seen[v] = true
				used += uint64(len(v.bytes))
			}
		}
		if used+uint64(n)-uint64(len(f.node.bytes)) > f.host.memoryLimit {
			return errors.New("ResourceLimit: in-memory storage limit")
		}
		f.node.bytes = append(f.node.bytes, make([]byte, int(n)-len(f.node.bytes))...)
	} else {
		f.node.bytes = f.node.bytes[:n]
	}
	return nil
}
func (f *memoryHostFile) size() (int64, error) { return int64(len(f.node.bytes)), nil }
func (f *memoryHostFile) Sync() error          { return nil }
func (f *memoryHostFile) Close() error {
	if f.locked {
		f.node.locked = false
		f.locked = false
	}
	return nil
}
func (f *memoryHostFile) lock() error {
	if f.node.locked {
		return errHostLocked
	}
	f.node.locked = true
	f.locked = true
	return nil
}
