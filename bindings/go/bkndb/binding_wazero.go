//go:build bkndb_wasm && ((linux && !android && (amd64 || arm64)) || (darwin && !ios && (amd64 || arm64)) || (windows && amd64))

package bkndb

import (
	"context"
	_ "embed"
	"encoding/json"
	"errors"
	"fmt"
	"strings"
	"sync"

	"github.com/tetratelabs/wazero"
)

//go:embed wasm/bkndb.wasm
var wasmModule []byte
var wasmCache = wazero.NewCompilationCache()

type wasmInstance struct {
	mu      sync.Mutex
	runtime wazero.Runtime
	host    *wasmHost
	ctx     context.Context
	closed  bool
	done    chan struct{}
}
type wasmDriver struct {
	mu           sync.Mutex
	next         uintptr
	databases    map[uintptr]*wasmInstance
	transactions map[uintptr]*wasmInstance
}

func init() {
	defaultDriver = &wasmDriver{databases: map[uintptr]*wasmInstance{}, transactions: map[uintptr]*wasmInstance{}}
}
func (d *wasmDriver) register(i *wasmInstance, tx bool) (uintptr, error) {
	d.mu.Lock()
	defer d.mu.Unlock()
	if d.next == ^uintptr(0) {
		return 0, errors.New("handle space exhausted")
	}
	d.next++
	if tx {
		d.transactions[d.next] = i
	} else {
		d.databases[d.next] = i
	}
	return d.next, nil
}
func (d *wasmDriver) lookup(h uintptr, tx bool) (*wasmInstance, error) {
	d.mu.Lock()
	defer d.mu.Unlock()
	var i *wasmInstance
	if tx {
		i = d.transactions[h]
	} else {
		i = d.databases[h]
	}
	if i == nil {
		return nil, errors.New("database or transaction is closed")
	}
	return i, nil
}
func (i *wasmInstance) shutdown() {
	if i.closed {
		return
	}
	i.closed = true
	_ = i.runtime.Close(context.Background())
	i.host.closeAll()
	close(i.done)
}
func (i *wasmInstance) rpc(req map[string]any) (json.RawMessage, error) {
	if i.closed {
		return nil, errors.New("database is closed")
	}
	if err := i.ctx.Err(); err != nil {
		i.shutdown()
		return nil, err
	}
	bytes, err := json.Marshal(req)
	if err != nil {
		return nil, err
	}
	if len(bytes) == 0 || len(bytes) > 16<<20 {
		return nil, errors.New("ResourceLimit: request exceeds 16 MiB")
	}
	m := i.host.module
	p, err := m.ExportedFunction("bkndb_alloc").Call(i.ctx, uint64(len(bytes)))
	if err != nil {
		i.shutdown()
		return nil, fmt.Errorf("WASM allocation: %w", err)
	}
	if len(p) != 1 || p[0] == 0 {
		return nil, errors.New("ResourceLimit: guest allocation refused")
	}
	if !m.Memory().Write(uint32(p[0]), bytes) {
		i.shutdown()
		return nil, errors.New("invalid WASM request buffer")
	}
	output, err := m.ExportedFunction("bkndb_request").Call(i.ctx, p[0], uint64(len(bytes)))
	if err != nil {
		i.shutdown()
		return nil, fmt.Errorf("WASM call interrupted; commit outcome may be unknown; reopen and reconcile: %w", err)
	}
	if len(output) != 1 || output[0] == 0 {
		i.shutdown()
		return nil, errors.New("invalid WASM response")
	}
	ptr, n := uint32(output[0]>>32), uint32(output[0])
	if n > 16<<20 {
		i.shutdown()
		return nil, errors.New("invalid WASM response length")
	}
	buf, ok := m.Memory().Read(ptr, n)
	if !ok {
		i.shutdown()
		return nil, errors.New("invalid WASM response range")
	}
	var response struct {
		Result json.RawMessage `json:"result"`
		Error  string          `json:"error"`
	}
	decodeErr := json.Unmarshal(buf, &response)
	_, err = m.ExportedFunction("bkndb_dealloc").Call(i.ctx, uint64(ptr))
	if err != nil {
		i.shutdown()
		return nil, err
	}
	if decodeErr != nil {
		i.shutdown()
		return nil, decodeErr
	}
	if response.Error != "" {
		return nil, errors.New(response.Error)
	}
	return response.Result, nil
}
func newWASM(path string, memory bool, config WASMConfig) (*wasmInstance, error) {
	ctx := config.Context
	if ctx == nil {
		ctx = context.Background()
	}
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	limit := config.MemoryLimitMiB
	if limit == 0 {
		limit = 256
	}
	if limit < 16 || limit > 4096 {
		return nil, errors.New("WASM memory limit must be 16..4096 MiB")
	}
	h, err := newWASMHost(path, memory, uint64(limit)<<20)
	if err != nil {
		return nil, err
	}
	r := wazero.NewRuntimeWithConfig(ctx, wazero.NewRuntimeConfig().WithCompilationCache(wasmCache).WithMemoryLimitPages(limit*16).WithCloseOnContextDone(true))
	i := &wasmInstance{runtime: r, host: h, ctx: ctx, done: make(chan struct{})}
	if _, err = r.NewHostModuleBuilder("bkndb_host_v1").NewFunctionBuilder().WithFunc(h.ioCall).Export("io_call").Instantiate(ctx); err != nil {
		i.shutdown()
		return nil, err
	}
	compiled, err := r.CompileModule(ctx, wasmModule)
	if err != nil {
		i.shutdown()
		return nil, err
	}
	module, err := r.InstantiateModule(ctx, compiled, wazero.NewModuleConfig())
	if err != nil {
		i.shutdown()
		return nil, err
	}
	h.module = module
	version, err := module.ExportedFunction("bkndb_abi_version").Call(ctx)
	if err != nil || len(version) != 1 || version[0] != 1 {
		i.shutdown()
		return nil, errors.New("unsupported BknDb WASM ABI")
	}
	if _, err = i.rpc(map[string]any{"op": "open"}); err != nil {
		i.shutdown()
		return nil, err
	}
	if ctx.Done() != nil {
		go func() {
			select {
			case <-ctx.Done():
				i.mu.Lock()
				i.shutdown()
				i.mu.Unlock()
			case <-i.done:
			}
		}()
	}
	return i, nil
}
func (d *wasmDriver) open(string) (uintptr, error) {
	return 0, errors.New("Redb Open is unavailable with bkndb_wasm; use OpenLSM explicitly")
}
func (d *wasmDriver) openLSM(path string) (uintptr, error) {
	return d.openLSMConfig(path, WASMConfig{})
}
func (d *wasmDriver) openLSMConfig(path string, c WASMConfig) (uintptr, error) {
	i, e := newWASM(path, false, c)
	if e != nil {
		return 0, e
	}
	h, e := d.register(i, false)
	if e != nil {
		i.mu.Lock()
		i.shutdown()
		i.mu.Unlock()
	}
	return h, e
}
func (d *wasmDriver) inMemory() (uintptr, error) {
	i, e := newWASM("", true, WASMConfig{})
	if e != nil {
		return 0, e
	}
	h, e := d.register(i, false)
	if e != nil {
		i.mu.Lock()
		i.shutdown()
		i.mu.Unlock()
	}
	return h, e
}
func (d *wasmDriver) close(h uintptr) error {
	i, e := d.lookup(h, false)
	if e != nil {
		return e
	}
	i.mu.Lock()
	defer i.mu.Unlock()
	if i.closed {
		return nil
	}
	_, e = i.rpc(map[string]any{"op": "close"})
	i.shutdown()
	return e
}
func (d *wasmDriver) isClosed(h uintptr) bool {
	i, e := d.lookup(h, false)
	if e != nil {
		return true
	}
	i.mu.Lock()
	defer i.mu.Unlock()
	return i.closed
}
func (d *wasmDriver) freeEngine(h uintptr) {
	_ = d.close(h)
	d.mu.Lock()
	delete(d.databases, h)
	d.mu.Unlock()
}
func (d *wasmDriver) call(h uintptr, req map[string]any, tx bool) (json.RawMessage, error) {
	i, e := d.lookup(h, tx)
	if e != nil {
		return nil, e
	}
	i.mu.Lock()
	defer i.mu.Unlock()
	if tx {
		req["transaction"] = uint64(h)
	}
	return i.rpc(req)
}
func (d *wasmDriver) compact(h uintptr) error {
	_, e := d.call(h, map[string]any{"op": "compact"}, false)
	return e
}
func (d *wasmDriver) backup(h uintptr, dest string) error {
	i, e := d.lookup(h, false)
	if e != nil {
		return e
	}
	i.mu.Lock()
	defer i.mu.Unlock()
	if e = i.host.setBackup(dest); e != nil {
		return e
	}
	_, e = i.rpc(map[string]any{"op": "backup"})
	return e
}
func (d *wasmDriver) stats(h uintptr) (string, error) {
	i, e := d.lookup(h, false)
	if e != nil {
		return "", e
	}
	i.mu.Lock()
	defer i.mu.Unlock()
	r, e := i.rpc(map[string]any{"op": "stats"})
	if e != nil {
		return "", e
	}
	if i.host.memory {
		var fields map[string]json.RawMessage
		if e = json.Unmarshal(r, &fields); e != nil {
			return "", e
		}
		fields["storage"] = json.RawMessage("null")
		r, e = json.Marshal(fields)
	}
	return string(r), e
}
func wasmMetric(metric string) string {
	if strings.EqualFold(metric, "euclidean") {
		return "euclidean"
	}
	if strings.EqualFold(metric, "dot") || strings.EqualFold(metric, "dot_product") {
		return "dot"
	}
	return "cosine"
}

func parseJSON(s string) (any, error) {
	if s == "" {
		return nil, nil
	}
	var r any
	dec := json.NewDecoder(strings.NewReader(s))
	dec.UseNumber()
	e := dec.Decode(&r)
	return r, e
}
func (d *wasmDriver) query(h uintptr, q, p, op string, tx bool) (string, error) {
	v, e := parseJSON(p)
	if e != nil {
		return "", e
	}
	r, e := d.call(h, map[string]any{"op": op, "query": q, "params": v}, tx)
	return string(r), e
}
func (d *wasmDriver) sql(h uintptr, q, p string) (string, error) {
	return d.query(h, q, p, "sql", false)
}
func (d *wasmDriver) graphQuery(h uintptr, q, p string) (string, error) {
	return d.query(h, q, p, "graph_query", false)
}
func (d *wasmDriver) createNode(h uintptr, label, p string) (uint64, error) {
	v, e := parseJSON(p)
	if e != nil {
		return 0, e
	}
	r, e := d.call(h, map[string]any{"op": "create_node", "label": label, "props": v}, false)
	var id uint64
	if e == nil {
		e = json.Unmarshal(r, &id)
	}
	return id, e
}
func (d *wasmDriver) getNode(h uintptr, id uint64) (string, error) {
	r, e := d.call(h, map[string]any{"op": "get_node", "id": id}, false)
	return string(r), e
}
func (d *wasmDriver) deleteNode(h uintptr, id uint64) error {
	_, e := d.call(h, map[string]any{"op": "delete_node", "id": id}, false)
	return e
}
func (d *wasmDriver) updateNode(h uintptr, id uint64, p, u string) error {
	v, e := parseJSON(p)
	if e != nil {
		return e
	}
	unset, e := parseJSON(u)
	if e != nil {
		return e
	}
	if unset == nil {
		unset = []string{}
	}
	_, e = d.call(h, map[string]any{"op": "update_node", "id": id, "props": v, "unset": unset}, false)
	return e
}
func (d *wasmDriver) createEdge(h uintptr, from uint64, kind string, to uint64, p string) (uint64, error) {
	v, e := parseJSON(p)
	if e != nil {
		return 0, e
	}
	r, e := d.call(h, map[string]any{"op": "create_edge", "from": from, "to": to, "edge_type": kind, "props": v}, false)
	var id uint64
	if e == nil {
		e = json.Unmarshal(r, &id)
	}
	return id, e
}
func (d *wasmDriver) getEdge(h uintptr, id uint64) (string, error) {
	r, e := d.call(h, map[string]any{"op": "get_edge", "id": id}, false)
	return string(r), e
}
func (d *wasmDriver) deleteEdge(h uintptr, id uint64) (bool, error) {
	r, e := d.call(h, map[string]any{"op": "delete_edge", "id": id}, false)
	var b bool
	if e == nil {
		e = json.Unmarshal(r, &b)
	}
	return b, e
}
func (d *wasmDriver) neighbors(h uintptr, id uint64, dir int, kind string) (string, error) {
	r, e := d.call(h, map[string]any{"op": "neighbors", "id": id, "direction": dir, "edge_type": kind}, false)
	return string(r), e
}
func (d *wasmDriver) createFulltextIndex(h uintptr, t, c string) error {
	_, e := d.call(h, map[string]any{"op": "create_fulltext_index", "table": t, "column": c}, false)
	return e
}
func (d *wasmDriver) searchText(h uintptr, t, c, q string, limit int, all bool) (string, error) {
	r, e := d.call(h, map[string]any{"op": "search_text", "table": t, "column": c, "query": q, "limit": limit, "match_all": all}, false)
	return string(r), e
}
func (d *wasmDriver) createVectorIndex(h uintptr, t, c, metric string, m, ef int) error {
	_, e := d.call(h, map[string]any{"op": "create_vector_index", "table": t, "column": c, "metric": wasmMetric(metric), "m": m, "ef": ef}, false)
	return e
}
func (d *wasmDriver) searchVector(h uintptr, t, c string, v []float32, limit int, metric string, exact bool) (string, error) {
	r, e := d.call(h, map[string]any{"op": "search_vector", "table": t, "column": c, "vector": v, "limit": limit, "metric": wasmMetric(metric), "exact": exact}, false)
	return string(r), e
}
func (d *wasmDriver) beginTransaction(h uintptr) (uintptr, error) {
	i, e := d.lookup(h, false)
	if e != nil {
		return 0, e
	}
	i.mu.Lock()
	defer i.mu.Unlock()
	token, e := d.register(i, true)
	if e != nil {
		return 0, e
	}
	if _, e = i.rpc(map[string]any{"op": "begin", "transaction": uint64(token)}); e != nil {
		d.mu.Lock()
		delete(d.transactions, token)
		d.mu.Unlock()
		return 0, e
	}
	return token, nil
}

func (d *wasmDriver) txSQL(h uintptr, q, p string) (string, error) {
	return d.query(h, q, p, "tx_sql", true)
}
func (d *wasmDriver) txCommit(h uintptr) error {
	_, e := d.call(h, map[string]any{"op": "commit"}, true)
	return e
}
func (d *wasmDriver) txRollback(h uintptr) error {
	_, e := d.call(h, map[string]any{"op": "rollback"}, true)
	return e
}
func (d *wasmDriver) freeTransaction(h uintptr) {
	_ = d.txRollback(h)
	d.mu.Lock()
	delete(d.transactions, h)
	d.mu.Unlock()
}
