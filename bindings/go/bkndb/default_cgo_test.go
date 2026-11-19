//go:build !bkndb_wasm && cgo

package bkndb_test

import (
	b "github.com/BknOrg/bkndb/bindings/go/bkndb"
	"path/filepath"
	"testing"
)

func TestDefaultStillUsesRedb(t *testing.T) {
	p := filepath.Join(t.TempDir(), "redb.bkndb")
	db, e := b.Open(p)
	if e != nil {
		t.Fatal(e)
	}
	if e = db.Close(); e != nil {
		t.Fatal(e)
	}
	if db, e = b.OpenLSM(p); e == nil {
		db.Close()
		t.Fatal("LSM accepted Redb format")
	}
}
