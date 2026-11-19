//go:build bkndb_wasm && (linux || darwin) && !android && !ios && (amd64 || arm64)

package bkndb

import (
	"errors"
	"os"
	"path/filepath"
	"syscall"
)

func openHostFile(path string, mode uint64) (hostFile, error) {
	if s, e := os.Lstat(path); e == nil && s.Mode()&os.ModeSymlink != 0 {
		return nil, os.ErrInvalid
	}
	flags := os.O_RDWR
	if mode == 0 {
		flags |= os.O_CREATE
	}
	if mode == 2 {
		flags |= os.O_CREATE | os.O_EXCL
	}
	f, e := os.OpenFile(path, flags, 0600)
	if e != nil {
		return nil, e
	}
	return &diskHostFile{f}, nil
}
func lockHostFile(f *os.File) error {
	e := syscall.Flock(int(f.Fd()), syscall.LOCK_EX|syscall.LOCK_NB)
	if errors.Is(e, syscall.EWOULDBLOCK) {
		return errHostLocked
	}
	return e
}
func replaceHostFile(src, dst string, _ bool) error { return os.Rename(src, dst) }
func publishHostFile(src, dst string) error {
	if e := os.Link(src, dst); e != nil {
		return e
	}
	return os.Remove(src)
}
func syncHostParent(path string) error {
	f, e := os.Open(filepath.Dir(path))
	if e != nil {
		return e
	}
	defer f.Close()
	return f.Sync()
}

func canonicalHostPath(path string) (string, error) {
	if s, e := os.Stat(path); e == nil {
		if !s.Mode().IsRegular() {
			return "", os.ErrInvalid
		}
		if s.Sys().(*syscall.Stat_t).Nlink != 1 {
			return "", errors.New("hard-linked databases are unsupported")
		}
		return filepath.EvalSymlinks(path)
	} else if !errors.Is(e, os.ErrNotExist) {
		return "", e
	}
	parent, e := filepath.EvalSymlinks(filepath.Dir(path))
	if e != nil {
		return "", e
	}
	return filepath.Join(parent, filepath.Base(path)), nil
}

func makeHostDirs(path string) error {
	var created []string
	for current := path; ; current = filepath.Dir(current) {
		if _, e := os.Stat(current); e == nil {
			break
		} else if !errors.Is(e, os.ErrNotExist) {
			return e
		}
		created = append(created, current)
		parent := filepath.Dir(current)
		if parent == current {
			break
		}
	}
	if e := os.MkdirAll(path, 0755); e != nil {
		return e
	}
	for _, dir := range created {
		if e := syncHostParent(dir); e != nil {
			return e
		}
	}
	return nil
}
