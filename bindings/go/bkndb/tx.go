package bkndb

import (
	"encoding/json"
	"errors"
	"runtime"
	"sync"
)

// Tx represents an explicit ACID write transaction.
type Tx struct {
	mu     sync.Mutex
	handle uintptr
	closed bool
}

// Begin opens an explicit write transaction. Only one write transaction can be
// open at a time on a database handle.
func (db *DB) Begin() (*Tx, error) {
	db.mu.RLock()
	defer db.mu.RUnlock()
	if db.closed || db.handle == 0 {
		return nil, errors.New("database is closed")
	}

	h, err := defaultDriver.beginTransaction(db.handle)
	if err != nil {
		return nil, err
	}
	tx := &Tx{handle: h}
	runtime.SetFinalizer(tx, (*Tx).free)
	return tx, nil
}

func (tx *Tx) free() {
	tx.mu.Lock()
	defer tx.mu.Unlock()
	if tx.handle != 0 {
		defaultDriver.freeTransaction(tx.handle)
		tx.handle = 0
		tx.closed = true
	}
}

// SQL executes a SQL statement inside this transaction (reads see uncommitted writes).
func (tx *Tx) SQL(query string, params ...any) (*QueryResult, error) {
	tx.mu.Lock()
	defer tx.mu.Unlock()
	if tx.closed || tx.handle == 0 {
		return nil, errors.New("transaction is closed")
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

	raw, err := defaultDriver.txSQL(tx.handle, query, paramsJSON)
	if err != nil {
		return nil, err
	}
	var res QueryResult
	if err := json.Unmarshal([]byte(raw), &res); err != nil {
		return nil, err
	}
	return &res, nil
}

// Exec executes a SQL command inside this transaction and returns affected rows.
func (tx *Tx) Exec(query string, params ...any) (uint64, error) {
	res, err := tx.SQL(query, params...)
	if err != nil {
		return 0, err
	}
	return res.Affected, nil
}

// Commit commits the transaction atomically.
func (tx *Tx) Commit() error {
	tx.mu.Lock()
	defer tx.mu.Unlock()
	if tx.closed || tx.handle == 0 {
		return errors.New("transaction is already closed")
	}
	err := defaultDriver.txCommit(tx.handle)
	defaultDriver.freeTransaction(tx.handle)
	tx.handle = 0
	tx.closed = true
	return err
}

// Rollback rolls back any writes made in this transaction.
func (tx *Tx) Rollback() error {
	tx.mu.Lock()
	defer tx.mu.Unlock()
	if tx.closed || tx.handle == 0 {
		return errors.New("transaction is already closed")
	}
	err := defaultDriver.txRollback(tx.handle)
	defaultDriver.freeTransaction(tx.handle)
	tx.handle = 0
	tx.closed = true
	return err
}
