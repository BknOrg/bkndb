//go:build bkndb_wasm && windows && amd64

package bkndb

import (
	"errors"
	"os"
	"path/filepath"
	"syscall"
	"unsafe"
)

var kernel32 = syscall.NewLazyDLL("kernel32.dll")
var lockFileEx = kernel32.NewProc("LockFileEx")
var moveFileEx = kernel32.NewProc("MoveFileExW")

func openHostFile(path string, mode uint64) (hostFile, error) {
	if s, e := os.Lstat(path); e == nil && s.Mode()&os.ModeSymlink != 0 {
		return nil, os.ErrInvalid
	}
	p, e := syscall.UTF16PtrFromString(path)
	if e != nil {
		return nil, e
	}
	var disposition uint32 = syscall.OPEN_EXISTING
	if mode == 0 {
		disposition = syscall.OPEN_ALWAYS
	}
	if mode == 2 {
		disposition = syscall.CREATE_NEW
	}
	h, e := syscall.CreateFile(p, syscall.GENERIC_READ|syscall.GENERIC_WRITE, syscall.FILE_SHARE_READ|syscall.FILE_SHARE_WRITE|syscall.FILE_SHARE_DELETE, nil, disposition, syscall.FILE_ATTRIBUTE_NORMAL, 0)
	if e != nil {
		return nil, e
	}
	return &diskHostFile{os.NewFile(uintptr(h), path)}, nil
}
func lockHostFile(f *os.File) error {
	var overlapped syscall.Overlapped
	r, _, e := lockFileEx.Call(f.Fd(), 3, 0, 1, 0, uintptr(unsafe.Pointer(&overlapped)))
	if r != 0 {
		return nil
	}
	if errors.Is(e, syscall.Errno(33)) {
		return errHostLocked
	}
	return e
}
func replaceHostFile(src, dst string, replace bool) error {
	s, e := syscall.UTF16PtrFromString(src)
	if e != nil {
		return e
	}
	d, e := syscall.UTF16PtrFromString(dst)
	if e != nil {
		return e
	}
	flags := uintptr(8)
	if replace {
		flags |= 1
	}
	r, _, e := moveFileEx.Call(uintptr(unsafe.Pointer(s)), uintptr(unsafe.Pointer(d)), flags)
	if r == 0 {
		return e
	}
	return nil
}
func publishHostFile(src, dst string) error { return replaceHostFile(src, dst, false) }

// MOVEFILE_WRITE_THROUGH provides the Windows replacement barrier.
func syncHostParent(string) error { return nil }

func canonicalHostPath(path string) (string, error) {
	if s, e := os.Stat(path); e == nil {
		if !s.Mode().IsRegular() {
			return "", os.ErrInvalid
		}
		resolved, e := filepath.EvalSymlinks(path)
		if e != nil {
			return "", e
		}
		opened, e := openHostFile(resolved, 1)
		if e != nil {
			return "", e
		}
		f := opened.(*diskHostFile)
		defer f.Close()
		var info syscall.ByHandleFileInformation
		if e = syscall.GetFileInformationByHandle(syscall.Handle(f.Fd()), &info); e != nil {
			return "", e
		}
		if info.NumberOfLinks != 1 {
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

func makeHostDirs(path string) error { return os.MkdirAll(path, 0755) }
