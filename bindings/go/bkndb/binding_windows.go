//go:build windows

package bkndb

import (
	"errors"
	"os"
	"path/filepath"
	"sync"
	"syscall"
	"unsafe"
)

var (
	dllOnce sync.Once
	dll     *syscall.LazyDLL
)

func init() {
	defaultDriver = &windowsDriver{}
}

func getDLL() *syscall.LazyDLL {
	dllOnce.Do(func() {
		dllName := "bkndb_ffi.dll"
		candidates := []string{
			dllName,
			filepath.Join("..", dllName),
			filepath.Join("..", "..", dllName),
			filepath.Join("bindings", "python", "bkndb", "_native", dllName),
			filepath.Join("..", "python", "bkndb", "_native", dllName),
			filepath.Join("..", "..", "bindings", "python", "bkndb", "_native", dllName),
			filepath.Join("target", "debug", dllName),
			filepath.Join("..", "..", "target", "debug", dllName),
			filepath.Join("..", "..", "..", "target", "debug", dllName),
			filepath.Join("target", "release", dllName),
			filepath.Join("..", "..", "target", "release", dllName),
			filepath.Join("..", "..", "..", "target", "release", dllName),
		}
		for _, c := range candidates {
			if _, err := os.Stat(c); err == nil {
				if abs, err := filepath.Abs(c); err == nil {
					dll = syscall.NewLazyDLL(abs)
					return
				}
			}
		}
		dll = syscall.NewLazyDLL(dllName)
	})
	return dll
}

func cString(s string) *byte {
	p, err := syscall.BytePtrFromString(s)
	if err != nil {
		p, _ = syscall.BytePtrFromString("")
	}
	return p
}

func uintptrToPointer(u uintptr) unsafe.Pointer {
	return *(*unsafe.Pointer)(unsafe.Pointer(&u))
}

func ptrToStringAndFree(ptr uintptr) string {
	if ptr == 0 {
		return ""
	}
	p := (*byte)(uintptrToPointer(ptr))
	slice := unsafe.Slice(p, 1<<28)
	var length int
	for length = 0; length < len(slice); length++ {
		if slice[length] == 0 {
			break
		}
	}
	res := string(slice[:length])
	getDLL().NewProc("bkndb_free_string").Call(ptr)
	return res
}

func checkErr(errPtr uintptr) error {
	if errPtr == 0 {
		return nil
	}
	msg := ptrToStringAndFree(errPtr)
	if msg == "" {
		return errors.New("unknown error")
	}
	return errors.New(msg)
}

type windowsDriver struct{}

func (d *windowsDriver) open(path string) (uintptr, error) {
	var errPtr uintptr
	r1, _, _ := getDLL().NewProc("bkndb_open").Call(
		uintptr(unsafe.Pointer(cString(path))),
		uintptr(unsafe.Pointer(&errPtr)),
	)
	if err := checkErr(errPtr); err != nil {
		return 0, err
	}
	if r1 == 0 {
		return 0, errors.New("failed to open database")
	}
	return r1, nil
}

func (d *windowsDriver) inMemory() (uintptr, error) {
	var errPtr uintptr
	r1, _, _ := getDLL().NewProc("bkndb_in_memory").Call(
		uintptr(unsafe.Pointer(&errPtr)),
	)
	if err := checkErr(errPtr); err != nil {
		return 0, err
	}
	if r1 == 0 {
		return 0, errors.New("failed to create in-memory database")
	}
	return r1, nil
}

func (d *windowsDriver) close(handle uintptr) error {
	var errPtr uintptr
	getDLL().NewProc("bkndb_close").Call(
		handle,
		uintptr(unsafe.Pointer(&errPtr)),
	)
	return checkErr(errPtr)
}

func (d *windowsDriver) isClosed(handle uintptr) bool {
	r1, _, _ := getDLL().NewProc("bkndb_is_closed").Call(handle)
	return r1 != 0
}

func (d *windowsDriver) freeEngine(handle uintptr) {
	getDLL().NewProc("bkndb_free_engine").Call(handle)
}

func (d *windowsDriver) compact(handle uintptr) error {
	var errPtr uintptr
	getDLL().NewProc("bkndb_compact").Call(
		handle,
		uintptr(unsafe.Pointer(&errPtr)),
	)
	return checkErr(errPtr)
}

func (d *windowsDriver) backup(handle uintptr, dest string) error {
	var errPtr uintptr
	getDLL().NewProc("bkndb_backup").Call(
		handle,
		uintptr(unsafe.Pointer(cString(dest))),
		uintptr(unsafe.Pointer(&errPtr)),
	)
	return checkErr(errPtr)
}

func (d *windowsDriver) stats(handle uintptr) (string, error) {
	var errPtr uintptr
	r1, _, _ := getDLL().NewProc("bkndb_stats").Call(
		handle,
		uintptr(unsafe.Pointer(&errPtr)),
	)
	if err := checkErr(errPtr); err != nil {
		return "", err
	}
	return ptrToStringAndFree(r1), nil
}

func (d *windowsDriver) sql(handle uintptr, query string, paramsJSON string) (string, error) {
	var errPtr uintptr
	var pParams uintptr
	if paramsJSON != "" {
		pParams = uintptr(unsafe.Pointer(cString(paramsJSON)))
	}
	r1, _, _ := getDLL().NewProc("bkndb_sql").Call(
		handle,
		uintptr(unsafe.Pointer(cString(query))),
		pParams,
		uintptr(unsafe.Pointer(&errPtr)),
	)
	if err := checkErr(errPtr); err != nil {
		return "", err
	}
	return ptrToStringAndFree(r1), nil
}

func (d *windowsDriver) graphQuery(handle uintptr, query string, paramsJSON string) (string, error) {
	var errPtr uintptr
	var pParams uintptr
	if paramsJSON != "" {
		pParams = uintptr(unsafe.Pointer(cString(paramsJSON)))
	}
	r1, _, _ := getDLL().NewProc("bkndb_graph_query").Call(
		handle,
		uintptr(unsafe.Pointer(cString(query))),
		pParams,
		uintptr(unsafe.Pointer(&errPtr)),
	)
	if err := checkErr(errPtr); err != nil {
		return "", err
	}
	return ptrToStringAndFree(r1), nil
}

func (d *windowsDriver) createNode(handle uintptr, label string, propsJSON string) (uint64, error) {
	var errPtr uintptr
	var outID uint64
	var pProps uintptr
	if propsJSON != "" {
		pProps = uintptr(unsafe.Pointer(cString(propsJSON)))
	}
	getDLL().NewProc("bkndb_create_node").Call(
		handle,
		uintptr(unsafe.Pointer(cString(label))),
		pProps,
		uintptr(unsafe.Pointer(&outID)),
		uintptr(unsafe.Pointer(&errPtr)),
	)
	if err := checkErr(errPtr); err != nil {
		return 0, err
	}
	return outID, nil
}

func (d *windowsDriver) getNode(handle uintptr, id uint64) (string, error) {
	var errPtr uintptr
	r1, _, _ := getDLL().NewProc("bkndb_get_node").Call(
		handle,
		uintptr(id),
		uintptr(unsafe.Pointer(&errPtr)),
	)
	if err := checkErr(errPtr); err != nil {
		return "", err
	}
	return ptrToStringAndFree(r1), nil
}

func (d *windowsDriver) deleteNode(handle uintptr, id uint64) error {
	var errPtr uintptr
	getDLL().NewProc("bkndb_delete_node").Call(
		handle,
		uintptr(id),
		uintptr(unsafe.Pointer(&errPtr)),
	)
	return checkErr(errPtr)
}

func (d *windowsDriver) updateNode(handle uintptr, id uint64, setJSON, unsetJSON string) error {
	var errPtr uintptr
	var pSet, pUnset uintptr
	if setJSON != "" {
		pSet = uintptr(unsafe.Pointer(cString(setJSON)))
	}
	if unsetJSON != "" {
		pUnset = uintptr(unsafe.Pointer(cString(unsetJSON)))
	}
	getDLL().NewProc("bkndb_update_node").Call(
		handle,
		uintptr(id),
		pSet,
		pUnset,
		uintptr(unsafe.Pointer(&errPtr)),
	)
	return checkErr(errPtr)
}

func (d *windowsDriver) createEdge(handle uintptr, from uint64, edgeType string, to uint64, propsJSON string) (uint64, error) {
	var errPtr uintptr
	var outID uint64
	var pProps uintptr
	if propsJSON != "" {
		pProps = uintptr(unsafe.Pointer(cString(propsJSON)))
	}
	getDLL().NewProc("bkndb_create_edge").Call(
		handle,
		uintptr(from),
		uintptr(unsafe.Pointer(cString(edgeType))),
		uintptr(to),
		pProps,
		uintptr(unsafe.Pointer(&outID)),
		uintptr(unsafe.Pointer(&errPtr)),
	)
	if err := checkErr(errPtr); err != nil {
		return 0, err
	}
	return outID, nil
}

func (d *windowsDriver) getEdge(handle uintptr, id uint64) (string, error) {
	var errPtr uintptr
	r1, _, _ := getDLL().NewProc("bkndb_get_edge").Call(
		handle,
		uintptr(id),
		uintptr(unsafe.Pointer(&errPtr)),
	)
	if err := checkErr(errPtr); err != nil {
		return "", err
	}
	return ptrToStringAndFree(r1), nil
}

func (d *windowsDriver) deleteEdge(handle uintptr, id uint64) (bool, error) {
	var errPtr uintptr
	var deleted byte
	getDLL().NewProc("bkndb_delete_edge").Call(
		handle,
		uintptr(id),
		uintptr(unsafe.Pointer(&deleted)),
		uintptr(unsafe.Pointer(&errPtr)),
	)
	if err := checkErr(errPtr); err != nil {
		return false, err
	}
	return deleted != 0, nil
}

func (d *windowsDriver) neighbors(handle uintptr, node uint64, direction int, edgeType string) (string, error) {
	var errPtr uintptr
	var pEdgeType uintptr
	if edgeType != "" {
		pEdgeType = uintptr(unsafe.Pointer(cString(edgeType)))
	}
	r1, _, _ := getDLL().NewProc("bkndb_neighbors").Call(
		handle,
		uintptr(node),
		uintptr(direction),
		pEdgeType,
		uintptr(unsafe.Pointer(&errPtr)),
	)
	if err := checkErr(errPtr); err != nil {
		return "", err
	}
	return ptrToStringAndFree(r1), nil
}

func (d *windowsDriver) createFulltextIndex(handle uintptr, table, column string) error {
	var errPtr uintptr
	getDLL().NewProc("bkndb_create_fulltext_index").Call(
		handle,
		uintptr(unsafe.Pointer(cString(table))),
		uintptr(unsafe.Pointer(cString(column))),
		uintptr(unsafe.Pointer(&errPtr)),
	)
	return checkErr(errPtr)
}

func (d *windowsDriver) searchText(handle uintptr, table, column, query string, limit int, matchAll bool) (string, error) {
	var errPtr uintptr
	var matchAllByte byte
	if matchAll {
		matchAllByte = 1
	}
	r1, _, _ := getDLL().NewProc("bkndb_search_text").Call(
		handle,
		uintptr(unsafe.Pointer(cString(table))),
		uintptr(unsafe.Pointer(cString(column))),
		uintptr(unsafe.Pointer(cString(query))),
		uintptr(limit),
		uintptr(matchAllByte),
		uintptr(unsafe.Pointer(&errPtr)),
	)
	if err := checkErr(errPtr); err != nil {
		return "", err
	}
	return ptrToStringAndFree(r1), nil
}

func (d *windowsDriver) createVectorIndex(handle uintptr, table, column, metric string, m, efConstruction int) error {
	var errPtr uintptr
	getDLL().NewProc("bkndb_create_vector_index").Call(
		handle,
		uintptr(unsafe.Pointer(cString(table))),
		uintptr(unsafe.Pointer(cString(column))),
		uintptr(unsafe.Pointer(cString(metric))),
		uintptr(m),
		uintptr(efConstruction),
		uintptr(unsafe.Pointer(&errPtr)),
	)
	return checkErr(errPtr)
}

func (d *windowsDriver) searchVector(handle uintptr, table, column string, vec []float32, limit int, metric string, exact bool) (string, error) {
	var errPtr uintptr
	var exactByte byte
	if exact {
		exactByte = 1
	}
	var pVec uintptr
	if len(vec) > 0 {
		pVec = uintptr(unsafe.Pointer(&vec[0]))
	}
	r1, _, _ := getDLL().NewProc("bkndb_search_vector").Call(
		handle,
		uintptr(unsafe.Pointer(cString(table))),
		uintptr(unsafe.Pointer(cString(column))),
		pVec,
		uintptr(len(vec)),
		uintptr(limit),
		uintptr(unsafe.Pointer(cString(metric))),
		uintptr(exactByte),
		uintptr(unsafe.Pointer(&errPtr)),
	)
	if err := checkErr(errPtr); err != nil {
		return "", err
	}
	return ptrToStringAndFree(r1), nil
}

func (d *windowsDriver) beginTransaction(handle uintptr) (uintptr, error) {
	var errPtr uintptr
	r1, _, _ := getDLL().NewProc("bkndb_begin_transaction").Call(
		handle,
		uintptr(unsafe.Pointer(&errPtr)),
	)
	if err := checkErr(errPtr); err != nil {
		return 0, err
	}
	return r1, nil
}

func (d *windowsDriver) txSQL(txHandle uintptr, query string, paramsJSON string) (string, error) {
	var errPtr uintptr
	var pParams uintptr
	if paramsJSON != "" {
		pParams = uintptr(unsafe.Pointer(cString(paramsJSON)))
	}
	r1, _, _ := getDLL().NewProc("bkndb_tx_sql").Call(
		txHandle,
		uintptr(unsafe.Pointer(cString(query))),
		pParams,
		uintptr(unsafe.Pointer(&errPtr)),
	)
	if err := checkErr(errPtr); err != nil {
		return "", err
	}
	return ptrToStringAndFree(r1), nil
}

func (d *windowsDriver) txCommit(txHandle uintptr) error {
	var errPtr uintptr
	getDLL().NewProc("bkndb_tx_commit").Call(
		txHandle,
		uintptr(unsafe.Pointer(&errPtr)),
	)
	return checkErr(errPtr)
}

func (d *windowsDriver) txRollback(txHandle uintptr) error {
	var errPtr uintptr
	getDLL().NewProc("bkndb_tx_rollback").Call(
		txHandle,
		uintptr(unsafe.Pointer(&errPtr)),
	)
	return checkErr(errPtr)
}

func (d *windowsDriver) freeTransaction(txHandle uintptr) {
	getDLL().NewProc("bkndb_free_transaction").Call(txHandle)
}
