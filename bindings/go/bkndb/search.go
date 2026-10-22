package bkndb

import (
	"encoding/json"
	"errors"
)

// CreateFulltextIndex builds a BM25 inverted index on table(column).
func (db *DB) CreateFulltextIndex(table, column string) error {
	db.mu.RLock()
	defer db.mu.RUnlock()
	if db.closed || db.handle == 0 {
		return errors.New("database is closed")
	}
	return defaultDriver.createFulltextIndex(db.handle, table, column)
}

// SearchText performs BM25 full-text search against table(column) for query terms.
func (db *DB) SearchText(table, column, query string, limit int, matchAll ...bool) ([]ScoredRow, error) {
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
	if err := json.Unmarshal([]byte(raw), &results); err != nil {
		return nil, err
	}
	return results, nil
}

// CreateVectorIndex builds an approximate HNSW vector index on table(column).
// Default parameters are typically m=16, efConstruction=200.
func (db *DB) CreateVectorIndex(table, column string, metric VectorMetric, m, efConstruction int) error {
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
	return defaultDriver.createVectorIndex(db.handle, table, column, string(metric), m, efConstruction)
}

// SearchVector performs approximate (or exact) nearest neighbor vector search.
func (db *DB) SearchVector(table, column string, vector []float32, limit int, metric VectorMetric, exact ...bool) ([]ScoredRow, error) {
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
	if err := json.Unmarshal([]byte(raw), &results); err != nil {
		return nil, err
	}
	return results, nil
}
