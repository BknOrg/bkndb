//go:build bkndb_wasm

package bkndb

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestWASMRequiresExplicitLSM(t *testing.T) {
	if _, e := Open(filepath.Join(t.TempDir(), "db")); e == nil || !strings.Contains(e.Error(), "OpenLSM") {
		t.Fatal(e)
	}
}
func TestWASMPersistenceLockBackupAndCancellation(t *testing.T) {
	path := filepath.Join(t.TempDir(), "db.bkndb")
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	db, e := OpenLSMWithWASMConfig(path, WASMConfig{Context: ctx, MemoryLimitMiB: 64})
	if e != nil {
		t.Fatal(e)
	}
	if _, e = OpenLSM(path); e == nil {
		t.Fatal("second writer accepted")
	}
	if _, e = db.Exec("CREATE TABLE t (id INT PRIMARY KEY, v TEXT)"); e != nil {
		t.Fatal(e)
	}
	if _, e = db.Exec("INSERT INTO t VALUES (9007199254740993, 'persisted')"); e != nil {
		t.Fatal(e)
	}
	backup := path + ".backup"
	if e = db.Backup(backup); e != nil {
		t.Fatal(e)
	}
	if e = db.Backup(backup); e == nil {
		t.Fatal("backup overwrote destination")
	}
	tx, e := db.Begin()
	if e != nil {
		t.Fatal(e)
	}
	if _, e = tx.Exec("INSERT INTO t VALUES (2,'rollback')"); e != nil {
		t.Fatal(e)
	}
	cancel()
	deadline := time.Now().Add(3 * time.Second)
	for !db.IsClosed() && time.Now().Before(deadline) {
		time.Sleep(time.Millisecond)
	}
	if !db.IsClosed() {
		t.Fatal("cancelled runtime stayed open")
	}
	_ = db.Close()
	_ = tx.Rollback()
	db, e = OpenLSM(path)
	if e != nil {
		t.Fatal(e)
	}
	defer db.Close()
	r, e := db.SQL("SELECT v FROM t ORDER BY id")
	if e != nil || len(r.Rows) != 1 || r.Rows[0][0] != "persisted" {
		t.Fatalf("%+v %v", r, e)
	}
	copy, e := OpenLSM(backup)
	if e != nil {
		t.Fatal(e)
	}
	defer copy.Close()
	r, e = copy.SQL("SELECT v FROM t")
	if e != nil || len(r.Rows) != 1 {
		t.Fatal(r, e)
	}
}
func TestWASMRejectsRedbAndForeignFilesWithoutChangingThem(t *testing.T) {
	path := filepath.Join(t.TempDir(), "db")
	original := []byte("foreign database")
	if e := os.WriteFile(path, original, 0600); e != nil {
		t.Fatal(e)
	}
	if _, e := OpenLSM(path); e == nil {
		t.Fatal("accepted foreign file")
	}
	got, _ := os.ReadFile(path)
	if string(got) != string(original) {
		t.Fatal("source changed")
	}
}
func TestWASMStaleTransactionCannotRollbackNewTransaction(t *testing.T) {
	d := defaultDriver.(*wasmDriver)
	h, e := d.inMemory()
	if e != nil {
		t.Fatal(e)
	}
	defer d.freeEngine(h)
	a, e := d.beginTransaction(h)
	if e != nil {
		t.Fatal(e)
	}
	if e = d.txCommit(a); e != nil {
		t.Fatal(e)
	}
	b, e := d.beginTransaction(h)
	if e != nil {
		t.Fatal(e)
	}
	d.freeTransaction(a)
	if e = d.txCommit(b); e != nil {
		t.Fatal("stale handle affected new transaction:", e)
	}
	d.freeTransaction(b)
}
func TestWASMHostRejectsInvalidRangesAndRoles(t *testing.T) {
	h, e := newWASMHost("", true, 1<<20)
	if e != nil {
		t.Fatal(e)
	}
	if r := h.ioCall(context.Background(), nil, 1, 99, 0, 0, 0); r >= 0 {
		t.Fatal("unknown role accepted")
	}
	if r := h.ioCall(context.Background(), nil, 6, 1, ^uint64(0), 0, 0); r >= 0 {
		t.Fatal("overflow accepted")
	}
	if r := h.ioCall(context.Background(), nil, 11, 1, 4, 0, 0); r >= 0 {
		t.Fatal("unrestricted rename accepted")
	}
}

// This fixture is produced by Rust and consumed again by Rust after this test.
func TestNativeWASMInterchange(t *testing.T) {
	path := os.Getenv("BKNDB_INTERCHANGE")
	if path == "" {
		t.Skip("run scripts/go/wasm.py interchange")
	}
	db, e := OpenLSM(path)
	if e != nil {
		t.Fatal(e)
	}
	defer db.Close()
	const high uint64 = 9007199254741037
	n, e := db.GetNode(high)
	if e != nil || n == nil || n.ID != high || n.Label != "Native" {
		t.Fatal(n, e)
	}
	result, e := db.GraphQuery("MATCH (n:Native) RETURN id(n) AS id")
	if e != nil {
		t.Fatal(e)
	}
	if id, ok := result.Rows[0][0].(json.Number); !ok || id.String() != "9007199254741037" {
		t.Fatal("query ID lost precision", result.Rows)
	}
	id, e := db.CreateNode("WASM", nil)
	if e != nil || id != high+1 {
		t.Fatal(id, e)
	}
	if _, e = db.Exec("INSERT INTO exchange VALUES (2,'wasm hello',[0.0,1.0])"); e != nil {
		t.Fatal(e)
	}
	hits, e := db.SearchText("exchange", "text", "hello", 10, false)
	if e != nil || len(hits) != 2 {
		t.Fatal(hits, e)
	}
	vectors, e := db.SearchVector("exchange", "embedding", []float32{1, 0}, 2, MetricCosine, false)
	if e != nil || len(vectors) != 2 {
		t.Fatal("native/WASM vector index mismatch", vectors, e)
	}
	if e = db.Compact(); e != nil {
		t.Fatal(e)
	}
}
func FuzzWASMABI(f *testing.F) {
	f.Add([]byte(`{"op":"stats"}`))
	f.Add([]byte(`{"op":"get_node","id":18446744073709551615}`))
	f.Add([]byte(`{"op":"tx_sql","query":"SELECT * FROM t"}`))
	f.Add([]byte{0xff})
	f.Fuzz(func(t *testing.T, input []byte) {
		if len(input) == 0 || len(input) > 128<<10 {
			t.Skip()
		}
		i, e := newWASM("", true, WASMConfig{MemoryLimitMiB: 64})
		if e != nil {
			t.Fatal(e)
		}
		defer i.shutdown()
		p, e := i.host.module.ExportedFunction("bkndb_alloc").Call(context.Background(), uint64(len(input)))
		if e != nil {
			t.Fatal(e)
		}
		if p[0] == 0 {
			t.Fatal("small buffer refused")
		}
		if !i.host.module.Memory().Write(uint32(p[0]), input) {
			t.Fatal("range")
		}
		// A trap is an allowed guest failure. Host resources must still be releasable.
		_, _ = i.host.module.ExportedFunction("bkndb_request").Call(context.Background(), p[0], uint64(len(input)))
	})
}

func TestWASMHandleExhaustionNeverReusesHandles(t *testing.T) {
	d := &wasmDriver{next: ^uintptr(0), databases: map[uintptr]*wasmInstance{}, transactions: map[uintptr]*wasmInstance{}}
	for n := 0; n < 2; n++ {
		if _, e := d.register(nil, false); e == nil {
			t.Fatal("wrapped database handle")
		}
	}
	h, e := newWASMHost("", true, 1<<20)
	if e != nil {
		t.Fatal(e)
	}
	h.next = ^uint32(0)
	for n := 0; n < 2; n++ {
		if _, e = h.open(1, 0); e == nil {
			t.Fatal("wrapped file handle")
		}
	}
	if len(h.files) != 0 || len(h.nodes) != 0 {
		t.Fatal("exhaustion created resources")
	}
}
func TestWASMHostChecksActualGuestMemoryRanges(t *testing.T) {
	i, e := newWASM("", true, WASMConfig{})
	if e != nil {
		t.Fatal(e)
	}
	defer i.shutdown()
	var id uint32
	for k, f := range i.host.files {
		if _, ok := f.(*memoryHostFile); ok {
			id = k
			break
		}
	}
	if id == 0 {
		t.Fatal("no open file")
	}
	if r := i.host.ioCall(context.Background(), i.host.module, 3, id, 0, ^uint32(0)-15, 64); r >= 0 {
		t.Fatal("overflow range accepted")
	}
	if r := i.host.ioCall(context.Background(), i.host.module, 4, id, 0, ^uint32(0)-15, 64); r >= 0 {
		t.Fatal("invalid write range accepted")
	}
}

func TestNativeWASMLocking(t *testing.T) {
	path := os.Getenv("BKNDB_INTERCHANGE_LOCKED")
	if path == "" {
		t.Skip("run scripts/go/wasm.py interchange")
	}
	db, e := OpenLSM(path)
	if e == nil {
		db.Close()
		t.Fatal("WASM acquired a database locked by native Rust")
	}
	if !strings.Contains(e.Error(), "already open") {
		t.Fatal("unexpected locking error:", e)
	}
}
