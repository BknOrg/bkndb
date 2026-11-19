package bkndb

import (
	"context"
	"errors"
	"runtime"
)

// WASMConfig configures one isolated runtime. MemoryLimitMiB defaults to 256.
// Cancelling Context invalidates the instance, rolls back uncommitted work and
// releases its host resources; reopen and reconcile any interrupted commit.
type WASMConfig struct {
	Context        context.Context
	MemoryLimitMiB uint32
}

// OpenLSMWithWASMConfig opens LSM using explicit WASM runtime settings.
// Build with -tags=bkndb_wasm. OpenLSM uses the default settings.
func OpenLSMWithWASMConfig(path string, config WASMConfig) (*DB, error) {
	if err := validateText(path); err != nil {
		return nil, err
	}
	d, ok := defaultDriver.(interface {
		openLSMConfig(string, WASMConfig) (uintptr, error)
	})
	if !ok {
		return nil, errors.New("WASMConfig requires -tags=bkndb_wasm")
	}
	h, e := d.openLSMConfig(path, config)
	if e != nil {
		return nil, e
	}
	db := &DB{handle: h}
	runtime.SetFinalizer(db, (*DB).free)
	return db, nil
}
