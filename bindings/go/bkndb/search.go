package bkndb

import (
	"errors"
)

// CreateFulltextIndex builds a BM25 inverted index on table(column).
func (db *DB) CreateFulltextIndex(table, column string) error {
	if err := validateText(table, column); err != nil {
		return err
	}
	db.mu.RLock()
	defer db.mu.RUnlock()
	if db.closed || db.handle == 0 {
		return errors.New("database is closed")
	}
	return defaultDriver.createFulltextIndex(db.handle, table, column)
}

// SearchText performs BM25 full-text search against table(column) for query terms.
func (db *DB) SearchText(table, column, query string, limit int, matchAll ...bool) ([]ScoredRow, error) {
	if limit < 0 || uint64(limit) > uint64(^uint32(0)) {
		return nil, errors.New("search limit must fit an unsigned 32-bit integer")
	}
	if err := validateText(table, column, query); err != nil {
		return nil, err
	}
	db.mu.RLock()
	defer db.mu.RUnlock()
	if db.closed || db.handle == 0 {
		return nil, errors.New("database is closed")
	}

	all := false
	if len(matchAll) > 0 {
		all = matchAll[0]
	}
	raw, err := defaultDriver.searchText(db.handle, table, column, query, limit, all)
	if err != nil {
		return nil, err
	}
	var results []ScoredRow
	if err := decodeResult([]byte(raw), &results); err != nil {
		return nil, err
	}
	return results, nil
}

// CreateVectorIndex builds an approximate HNSW vector index on table(column).
// Default parameters are typically m=16, efConstruction=200.
func (db *DB) CreateVectorIndex(table, column string, metric VectorMetric, m, efConstruction int) error {
	if err := validateText(table, column, string(metric)); err != nil {
		return err
	}
	db.mu.RLock()
	defer db.mu.RUnlock()
	if db.closed || db.handle == 0 {
		return errors.New("database is closed")
	}
	if m <= 0 {
		m = 16
	}
	if efConstruction <= 0 {
		efConstruction = 200
	}
	if uint64(m) > uint64(^uint32(0)) || uint64(efConstruction) > uint64(^uint32(0)) {
		return errors.New("vector index parameters exceed unsigned 32-bit range")
	}
	return defaultDriver.createVectorIndex(db.handle, table, column, string(metric), m, efConstruction)
}

// SearchVector performs approximate (or exact) nearest neighbor vector search.
func (db *DB) SearchVector(table, column string, vector []float32, limit int, metric VectorMetric, exact ...bool) ([]ScoredRow, error) {
	if limit < 0 || uint64(limit) > uint64(^uint32(0)) {
		return nil, errors.New("search limit must fit an unsigned 32-bit integer")
	}
	if err := validateText(table, column, string(metric)); err != nil {
		return nil, err
	}
	db.mu.RLock()
	defer db.mu.RUnlock()
	if db.closed || db.handle == 0 {
		return nil, errors.New("database is closed")
	}

	isExact := false
	if len(exact) > 0 {
		isExact = exact[0]
	}
	raw, err := defaultDriver.searchVector(db.handle, table, column, vector, limit, string(metric), isExact)
	if err != nil {
		return nil, err
	}
	var results []ScoredRow
	if err := decodeResult([]byte(raw), &results); err != nil {
		return nil, err
	}
	return results, nil
}
