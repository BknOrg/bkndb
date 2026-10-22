package bkndb

import (
	"encoding/json"
	"errors"
	"runtime"
	"sync"
)

// DB represents an open embedded BknDb database instance.
type DB struct {
	mu     sync.RWMutex
	handle uintptr
	closed bool
}

// Open opens or creates an embedded single-file BknDb database at path.
func Open(path string) (*DB, error) {
	h, err := defaultDriver.open(path)
	if err != nil {
		return nil, err
	}
	db := &DB{handle: h}
	runtime.SetFinalizer(db, (*DB).free)
	return db, nil
}

// InMemory creates an ephemeral in-memory BknDb database instance.
func InMemory() (*DB, error) {
	h, err := defaultDriver.inMemory()
	if err != nil {
		return nil, err
	}
	db := &DB{handle: h}
	runtime.SetFinalizer(db, (*DB).free)
	return db, nil
}

func (db *DB) free() {
	db.mu.Lock()
	defer db.mu.Unlock()
	if db.handle != 0 {
		defaultDriver.freeEngine(db.handle)
		db.handle = 0
		db.closed = true
	}
}

// Close closes the database handle and releases any file locks.
func (db *DB) Close() error {
	db.mu.Lock()
	defer db.mu.Unlock()
	if db.closed || db.handle == 0 {
		return nil
	}
	err := defaultDriver.close(db.handle)
	defaultDriver.freeEngine(db.handle)
	db.handle = 0
	db.closed = true
	return err
}

// IsClosed returns true if the database handle is closed.
func (db *DB) IsClosed() bool {
	db.mu.RLock()
	defer db.mu.RUnlock()
	if db.closed || db.handle == 0 {
		return true
	}
	return defaultDriver.isClosed(db.handle)
}

// Compact reclaims unused disk space by compacting the database file.
func (db *DB) Compact() error {
	db.mu.RLock()
	defer db.mu.RUnlock()
	if db.closed || db.handle == 0 {
		return errors.New("database is closed")
	}
	return defaultDriver.compact(db.handle)
}

// Backup creates a point-in-time consistent backup copy at dest.
func (db *DB) Backup(dest string) error {
	db.mu.RLock()
	defer db.mu.RUnlock()
	if db.closed || db.handle == 0 {
		return errors.New("database is closed")
	}
	return defaultDriver.backup(db.handle, dest)
}

// Stats returns logical and storage statistics for the database.
func (db *DB) Stats() (*DbStats, error) {
	db.mu.RLock()
	defer db.mu.RUnlock()
	if db.closed || db.handle == 0 {
		return nil, errors.New("database is closed")
	}
	raw, err := defaultDriver.stats(db.handle)
	if err != nil {
		return nil, err
	}
	var stats DbStats
	if err := json.Unmarshal([]byte(raw), &stats); err != nil {
		return nil, err
	}
	return &stats, nil
}

// SQL executes a SQL statement. Params can be passed as positional arguments
// (? or $1) or a map[string]any for named arguments (:name).
func (db *DB) SQL(query string, params ...any) (*QueryResult, error) {
	db.mu.RLock()
	defer db.mu.RUnlock()
	if db.closed || db.handle == 0 {
		return nil, errors.New("database is closed")
	}

	var paramsJSON string
	if len(params) == 1 {
		if m, ok := params[0].(map[string]any); ok {
			b, err := json.Marshal(m)
			if err != nil {
				return nil, err
			}
			paramsJSON = string(b)
		} else {
			b, err := json.Marshal(params)
			if err != nil {
				return nil, err
			}
			paramsJSON = string(b)
		}
	} else if len(params) > 1 {
		b, err := json.Marshal(params)
		if err != nil {
			return nil, err
		}
		paramsJSON = string(b)
	}

	raw, err := defaultDriver.sql(db.handle, query, paramsJSON)
	if err != nil {
		return nil, err
	}
	var res QueryResult
	if err := json.Unmarshal([]byte(raw), &res); err != nil {
		return nil, err
	}
	return &res, nil
}

// Exec executes a SQL command (e.g., CREATE TABLE, INSERT, UPDATE, DELETE)
// and returns the number of affected rows.
func (db *DB) Exec(query string, params ...any) (uint64, error) {
	res, err := db.SQL(query, params...)
	if err != nil {
		return 0, err
	}
	return res.Affected, nil
}
