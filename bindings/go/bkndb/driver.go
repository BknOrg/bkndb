package bkndb

// defaultDriver holds the active platform driver instance (initialized in platform-specific files).
var defaultDriver driver

// driver is the internal interface implemented by platform-specific FFI loaders
// (e.g., Windows syscall.NewLazyDLL or standard cgo for POSIX systems).
type driver interface {
	open(path string) (uintptr, error)
	inMemory() (uintptr, error)
	close(handle uintptr) error
	isClosed(handle uintptr) bool
	freeEngine(handle uintptr)
	compact(handle uintptr) error
	backup(handle uintptr, dest string) error
	stats(handle uintptr) (string, error)
	sql(handle uintptr, query string, paramsJSON string) (string, error)
	graphQuery(handle uintptr, query string, paramsJSON string) (string, error)
	createNode(handle uintptr, label string, propsJSON string) (uint64, error)
	getNode(handle uintptr, id uint64) (string, error)
	deleteNode(handle uintptr, id uint64) error
	updateNode(handle uintptr, id uint64, setJSON, unsetJSON string) error
	createEdge(handle uintptr, from uint64, edgeType string, to uint64, propsJSON string) (uint64, error)
	getEdge(handle uintptr, id uint64) (string, error)
	deleteEdge(handle uintptr, id uint64) (bool, error)
	neighbors(handle uintptr, node uint64, direction int, edgeType string) (string, error)
	createFulltextIndex(handle uintptr, table, column string) error
	searchText(handle uintptr, table, column, query string, limit int, matchAll bool) (string, error)
	createVectorIndex(handle uintptr, table, column, metric string, m, efConstruction int) error
	searchVector(handle uintptr, table, column string, vec []float32, limit int, metric string, exact bool) (string, error)
	beginTransaction(handle uintptr) (uintptr, error)
	txSQL(txHandle uintptr, query string, paramsJSON string) (string, error)
	txCommit(txHandle uintptr) error
	txRollback(txHandle uintptr) error
	freeTransaction(txHandle uintptr)
}
