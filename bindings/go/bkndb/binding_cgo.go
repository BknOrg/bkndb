//go:build !windows

package bkndb

/*
#cgo LDFLAGS: -L. -L../../target/debug -L../../target/release -L../../../target/debug -L../../../target/release -lbkndb_ffi
#include "../../bkndb.h"
#include <stdlib.h>
*/
import "C"
import (
	"errors"
	"unsafe"
)

func init() {
	defaultDriver = &cgoDriver{}
}

type cgoDriver struct{}

func checkCgoErr(errPtr *C.char) error {
	if errPtr == nil {
		return nil
	}
	defer C.bkndb_free_string(errPtr)
	return errors.New(C.GoString(errPtr))
}

func (d *cgoDriver) open(path string) (uintptr, error) {
	cPath := C.CString(path)
	defer C.free(unsafe.Pointer(cPath))
	var cErr *C.char

	engine := C.bkndb_open(cPath, &cErr)
	if err := checkCgoErr(cErr); err != nil {
		return 0, err
	}
	if engine == nil {
		return 0, errors.New("failed to open database")
	}
	return uintptr(unsafe.Pointer(engine)), nil
}

func (d *cgoDriver) inMemory() (uintptr, error) {
	var cErr *C.char
	engine := C.bkndb_in_memory(&cErr)
	if err := checkCgoErr(cErr); err != nil {
		return 0, err
	}
	if engine == nil {
		return 0, errors.New("failed to create in-memory database")
	}
	return uintptr(unsafe.Pointer(engine)), nil
}

func (d *cgoDriver) close(handle uintptr) error {
	var cErr *C.char
	C.bkndb_close((*C.BknDbEngine)(unsafe.Pointer(handle)), &cErr)
	return checkCgoErr(cErr)
}

func (d *cgoDriver) isClosed(handle uintptr) bool {
	return bool(C.bkndb_is_closed((*C.BknDbEngine)(unsafe.Pointer(handle))))
}

func (d *cgoDriver) freeEngine(handle uintptr) {
	C.bkndb_free_engine((*C.BknDbEngine)(unsafe.Pointer(handle)))
}

func (d *cgoDriver) compact(handle uintptr) error {
	var cErr *C.char
	C.bkndb_compact((*C.BknDbEngine)(unsafe.Pointer(handle)), &cErr)
	return checkCgoErr(cErr)
}

func (d *cgoDriver) backup(handle uintptr, dest string) error {
	cDest := C.CString(dest)
	defer C.free(unsafe.Pointer(cDest))
	var cErr *C.char
	C.bkndb_backup((*C.BknDbEngine)(unsafe.Pointer(handle)), cDest, &cErr)
	return checkCgoErr(cErr)
}

func (d *cgoDriver) stats(handle uintptr) (string, error) {
	var cErr *C.char
	cStr := C.bkndb_stats((*C.BknDbEngine)(unsafe.Pointer(handle)), &cErr)
	if err := checkCgoErr(cErr); err != nil {
		return "", err
	}
	if cStr == nil {
		return "", nil
	}
	defer C.bkndb_free_string(cStr)
	return C.GoString(cStr), nil
}

func (d *cgoDriver) sql(handle uintptr, query string, paramsJSON string) (string, error) {
	cQuery := C.CString(query)
	defer C.free(unsafe.Pointer(cQuery))
	var cParams *C.char
	if paramsJSON != "" {
		cParams = C.CString(paramsJSON)
		defer C.free(unsafe.Pointer(cParams))
	}
	var cErr *C.char
	cRes := C.bkndb_sql((*C.BknDbEngine)(unsafe.Pointer(handle)), cQuery, cParams, &cErr)
	if err := checkCgoErr(cErr); err != nil {
		return "", err
	}
	if cRes == nil {
		return "", nil
	}
	defer C.bkndb_free_string(cRes)
	return C.GoString(cRes), nil
}

func (d *cgoDriver) graphQuery(handle uintptr, query string, paramsJSON string) (string, error) {
	cQuery := C.CString(query)
	defer C.free(unsafe.Pointer(cQuery))
	var cParams *C.char
	if paramsJSON != "" {
		cParams = C.CString(paramsJSON)
		defer C.free(unsafe.Pointer(cParams))
	}
	var cErr *C.char
	cRes := C.bkndb_graph_query((*C.BknDbEngine)(unsafe.Pointer(handle)), cQuery, cParams, &cErr)
	if err := checkCgoErr(cErr); err != nil {
		return "", err
	}
	if cRes == nil {
		return "", nil
	}
	defer C.bkndb_free_string(cRes)
	return C.GoString(cRes), nil
}

func (d *cgoDriver) createNode(handle uintptr, label string, propsJSON string) (uint64, error) {
	cLabel := C.CString(label)
	defer C.free(unsafe.Pointer(cLabel))
	var cProps *C.char
	if propsJSON != "" {
		cProps = C.CString(propsJSON)
		defer C.free(unsafe.Pointer(cProps))
	}
	var outID C.uint64_t
	var cErr *C.char
	C.bkndb_create_node((*C.BknDbEngine)(unsafe.Pointer(handle)), cLabel, cProps, &outID, &cErr)
	if err := checkCgoErr(cErr); err != nil {
		return 0, err
	}
	return uint64(outID), nil
}

func (d *cgoDriver) getNode(handle uintptr, id uint64) (string, error) {
	var cErr *C.char
	cRes := C.bkndb_get_node((*C.BknDbEngine)(unsafe.Pointer(handle)), C.uint64_t(id), &cErr)
	if err := checkCgoErr(cErr); err != nil {
		return "", err
	}
	if cRes == nil {
		return "", nil
	}
	defer C.bkndb_free_string(cRes)
	return C.GoString(cRes), nil
}

func (d *cgoDriver) deleteNode(handle uintptr, id uint64) error {
	var cErr *C.char
	C.bkndb_delete_node((*C.BknDbEngine)(unsafe.Pointer(handle)), C.uint64_t(id), &cErr)
	return checkCgoErr(cErr)
}

func (d *cgoDriver) updateNode(handle uintptr, id uint64, setJSON, unsetJSON string) error {
	var cSet, cUnset *C.char
	if setJSON != "" {
		cSet = C.CString(setJSON)
		defer C.free(unsafe.Pointer(cSet))
	}
	if unsetJSON != "" {
		cUnset = C.CString(unsetJSON)
		defer C.free(unsafe.Pointer(cUnset))
	}
	var cErr *C.char
	C.bkndb_update_node((*C.BknDbEngine)(unsafe.Pointer(handle)), C.uint64_t(id), cSet, cUnset, &cErr)
	return checkCgoErr(cErr)
}

func (d *cgoDriver) createEdge(handle uintptr, from uint64, edgeType string, to uint64, propsJSON string) (uint64, error) {
	cType := C.CString(edgeType)
	defer C.free(unsafe.Pointer(cType))
	var cProps *C.char
	if propsJSON != "" {
		cProps = C.CString(propsJSON)
		defer C.free(unsafe.Pointer(cProps))
	}
	var outID C.uint64_t
	var cErr *C.char
	C.bkndb_create_edge((*C.BknDbEngine)(unsafe.Pointer(handle)), C.uint64_t(from), cType, C.uint64_t(to), cProps, &outID, &cErr)
	if err := checkCgoErr(cErr); err != nil {
		return 0, err
	}
	return uint64(outID), nil
}

func (d *cgoDriver) getEdge(handle uintptr, id uint64) (string, error) {
	var cErr *C.char
	cRes := C.bkndb_get_edge((*C.BknDbEngine)(unsafe.Pointer(handle)), C.uint64_t(id), &cErr)
	if err := checkCgoErr(cErr); err != nil {
		return "", err
	}
	if cRes == nil {
		return "", nil
	}
	defer C.bkndb_free_string(cRes)
	return C.GoString(cRes), nil
}

func (d *cgoDriver) deleteEdge(handle uintptr, id uint64) (bool, error) {
	var deleted C.bool
	var cErr *C.char
	C.bkndb_delete_edge((*C.BknDbEngine)(unsafe.Pointer(handle)), C.uint64_t(id), &deleted, &cErr)
	if err := checkCgoErr(cErr); err != nil {
		return false, err
	}
	return bool(deleted), nil
}

func (d *cgoDriver) neighbors(handle uintptr, node uint64, direction int, edgeType string) (string, error) {
	var cType *C.char
	if edgeType != "" {
		cType = C.CString(edgeType)
		defer C.free(unsafe.Pointer(cType))
	}
	var cErr *C.char
	cRes := C.bkndb_neighbors((*C.BknDbEngine)(unsafe.Pointer(handle)), C.uint64_t(node), C.int32_t(direction), cType, &cErr)
	if err := checkCgoErr(cErr); err != nil {
		return "", err
	}
	if cRes == nil {
		return "", nil
	}
	defer C.bkndb_free_string(cRes)
	return C.GoString(cRes), nil
}

func (d *cgoDriver) createFulltextIndex(handle uintptr, table, column string) error {
	cTable := C.CString(table)
	defer C.free(unsafe.Pointer(cTable))
	cCol := C.CString(column)
	defer C.free(unsafe.Pointer(cCol))
	var cErr *C.char
	C.bkndb_create_fulltext_index((*C.BknDbEngine)(unsafe.Pointer(handle)), cTable, cCol, &cErr)
	return checkCgoErr(cErr)
}

func (d *cgoDriver) searchText(handle uintptr, table, column, query string, limit int, matchAll bool) (string, error) {
	cTable := C.CString(table)
	defer C.free(unsafe.Pointer(cTable))
	cCol := C.CString(column)
	defer C.free(unsafe.Pointer(cCol))
	cQuery := C.CString(query)
	defer C.free(unsafe.Pointer(cQuery))
	var cErr *C.char
	cRes := C.bkndb_search_text((*C.BknDbEngine)(unsafe.Pointer(handle)), cTable, cCol, cQuery, C.uint32_t(limit), C.bool(matchAll), &cErr)
	if err := checkCgoErr(cErr); err != nil {
		return "", err
	}
	if cRes == nil {
		return "", nil
	}
	defer C.bkndb_free_string(cRes)
	return C.GoString(cRes), nil
}

func (d *cgoDriver) createVectorIndex(handle uintptr, table, column, metric string, m, efConstruction int) error {
	cTable := C.CString(table)
	defer C.free(unsafe.Pointer(cTable))
	cCol := C.CString(column)
	defer C.free(unsafe.Pointer(cCol))
	cMetric := C.CString(metric)
	defer C.free(unsafe.Pointer(cMetric))
	var cErr *C.char
	C.bkndb_create_vector_index((*C.BknDbEngine)(unsafe.Pointer(handle)), cTable, cCol, cMetric, C.uint32_t(m), C.uint32_t(efConstruction), &cErr)
	return checkCgoErr(cErr)
}

func (d *cgoDriver) searchVector(handle uintptr, table, column string, vec []float32, limit int, metric string, exact bool) (string, error) {
	cTable := C.CString(table)
	defer C.free(unsafe.Pointer(cTable))
	cCol := C.CString(column)
	defer C.free(unsafe.Pointer(cCol))
	cMetric := C.CString(metric)
	defer C.free(unsafe.Pointer(cMetric))
	var pVec *C.float
	if len(vec) > 0 {
		pVec = (*C.float)(&vec[0])
	}
	var cErr *C.char
	cRes := C.bkndb_search_vector((*C.BknDbEngine)(unsafe.Pointer(handle)), cTable, cCol, pVec, C.uint32_t(len(vec)), C.uint32_t(limit), cMetric, C.bool(exact), &cErr)
	if err := checkCgoErr(cErr); err != nil {
		return "", err
	}
	if cRes == nil {
		return "", nil
	}
	defer C.bkndb_free_string(cRes)
	return C.GoString(cRes), nil
}

func (d *cgoDriver) beginTransaction(handle uintptr) (uintptr, error) {
	var cErr *C.char
	tx := C.bkndb_begin_transaction((*C.BknDbEngine)(unsafe.Pointer(handle)), &cErr)
	if err := checkCgoErr(cErr); err != nil {
		return 0, err
	}
	return uintptr(unsafe.Pointer(tx)), nil
}

func (d *cgoDriver) txSQL(txHandle uintptr, query string, paramsJSON string) (string, error) {
	cQuery := C.CString(query)
	defer C.free(unsafe.Pointer(cQuery))
	var cParams *C.char
	if paramsJSON != "" {
		cParams = C.CString(paramsJSON)
		defer C.free(unsafe.Pointer(cParams))
	}
	var cErr *C.char
	cRes := C.bkndb_tx_sql((*C.BknDbTransaction)(unsafe.Pointer(txHandle)), cQuery, cParams, &cErr)
	if err := checkCgoErr(cErr); err != nil {
		return "", err
	}
	if cRes == nil {
		return "", nil
	}
	defer C.bkndb_free_string(cRes)
	return C.GoString(cRes), nil
}

func (d *cgoDriver) txCommit(txHandle uintptr) error {
	var cErr *C.char
	C.bkndb_tx_commit((*C.BknDbTransaction)(unsafe.Pointer(txHandle)), &cErr)
	return checkCgoErr(cErr)
}

func (d *cgoDriver) txRollback(txHandle uintptr) error {
	var cErr *C.char
	C.bkndb_tx_rollback((*C.BknDbTransaction)(unsafe.Pointer(txHandle)), &cErr)
	return checkCgoErr(cErr)
}

func (d *cgoDriver) freeTransaction(txHandle uintptr) {
	C.bkndb_free_transaction((*C.BknDbTransaction)(unsafe.Pointer(txHandle)))
}
