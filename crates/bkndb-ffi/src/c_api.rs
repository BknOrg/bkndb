#![allow(unsafe_op_in_unsafe_fn)]
//! Standard C-ABI exports for BknDb, providing a stable, cross-language interface
//! for Go (cgo or syscall DLL loading), C, C++, and other FFI consumers.

use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::sync::Arc;

use crate::engine::BknDbEngine;
use crate::lang::FfiQueryResult;
use crate::relational::FfiVectorMetric;
use crate::transaction::BknDbTransaction;
use crate::types::{FfiDirection, FfiEdgeRecord, FfiNodeRecord, FfiPropValue, FfiTypedNeighbor};

unsafe fn c_to_str<'a>(ptr: *const c_char) -> Option<&'a str> {
    if ptr.is_null() {
        None
    } else {
        CStr::from_ptr(ptr).to_str().ok()
    }
}

fn set_err(err_out: *mut *mut c_char, msg: impl ToString) {
    if !err_out.is_null() {
        let c_str = CString::new(msg.to_string()).unwrap_or_default();
        unsafe { *err_out = c_str.into_raw() };
    }
}

fn clear_err(err_out: *mut *mut c_char) {
    if !err_out.is_null() {
        unsafe { *err_out = std::ptr::null_mut() };
    }
}

fn to_c_string(s: impl Into<String>) -> *mut c_char {
    CString::new(s.into()).unwrap_or_default().into_raw()
}

// ---------------------------------------------------------------------------
// JSON <-> FfiPropValue conversions
// ---------------------------------------------------------------------------

fn json_to_prop(v: serde_json::Value) -> FfiPropValue {
    match v {
        serde_json::Value::Null => FfiPropValue::Null,
        serde_json::Value::Bool(b) => FfiPropValue::Bool(b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                FfiPropValue::Int(i)
            } else if let Some(f) = n.as_f64() {
                FfiPropValue::Float(f)
            } else {
                FfiPropValue::Null
            }
        }
        serde_json::Value::String(s) => FfiPropValue::Str(s),
        serde_json::Value::Array(arr) => FfiPropValue::List(arr.into_iter().map(json_to_prop).collect()),
        serde_json::Value::Object(map) => {
            FfiPropValue::Map(map.into_iter().map(|(k, v)| (k, json_to_prop(v))).collect())
        }
    }
}

fn prop_to_json(p: FfiPropValue) -> serde_json::Value {
    match p {
        FfiPropValue::Null => serde_json::Value::Null,
        FfiPropValue::Bool(b) => serde_json::Value::Bool(b),
        FfiPropValue::Int(i) => serde_json::Value::Number(serde_json::Number::from(i)),
        FfiPropValue::Float(f) => serde_json::Number::from_f64(f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        FfiPropValue::Str(s) => serde_json::Value::String(s),
        FfiPropValue::Bytes(b) => serde_json::Value::Array(b.into_iter().map(serde_json::Value::from).collect()),
        FfiPropValue::Timestamp(t) => serde_json::Value::Number(serde_json::Number::from(t)),
        FfiPropValue::Uuid { hi, lo } => serde_json::Value::String(format!("{:016x}{:016x}", hi, lo)),
        FfiPropValue::List(l) => serde_json::Value::Array(l.into_iter().map(prop_to_json).collect()),
        FfiPropValue::Map(m) => {
            serde_json::Value::Object(m.into_iter().map(|(k, v)| (k, prop_to_json(v))).collect())
        }
    }
}

fn parse_props_json(ptr: *const c_char) -> Result<HashMap<String, FfiPropValue>, String> {
    let s = unsafe { c_to_str(ptr) };
    match s {
        None | Some("") => Ok(HashMap::new()),
        Some(raw) => {
            let val: serde_json::Value = serde_json::from_str(raw).map_err(|e| e.to_string())?;
            match val {
                serde_json::Value::Object(map) => {
                    Ok(map.into_iter().map(|(k, v)| (k, json_to_prop(v))).collect())
                }
                _ => Err("properties must be a JSON object".to_string()),
            }
        }
    }
}

fn parse_params_json(
    ptr: *const c_char,
) -> Result<(Vec<FfiPropValue>, Option<HashMap<String, FfiPropValue>>), String> {
    let s = unsafe { c_to_str(ptr) };
    match s {
        None | Some("") => Ok((Vec::new(), None)),
        Some(raw) => {
            let val: serde_json::Value = serde_json::from_str(raw).map_err(|e| e.to_string())?;
            match val {
                serde_json::Value::Array(arr) => Ok((arr.into_iter().map(json_to_prop).collect(), None)),
                serde_json::Value::Object(map) => Ok((
                    Vec::new(),
                    Some(map.into_iter().map(|(k, v)| (k, json_to_prop(v))).collect()),
                )),
                _ => Err("params must be a JSON array or object".to_string()),
            }
        }
    }
}

fn parse_unset_keys_json(ptr: *const c_char) -> Result<Vec<String>, String> {
    let s = unsafe { c_to_str(ptr) };
    match s {
        None | Some("") => Ok(Vec::new()),
        Some(raw) => {
            let val: serde_json::Value = serde_json::from_str(raw).map_err(|e| e.to_string())?;
            match val {
                serde_json::Value::Array(arr) => {
                    let mut keys = Vec::with_capacity(arr.len());
                    for item in arr {
                        if let serde_json::Value::String(k) = item {
                            keys.push(k);
                        }
                    }
                    Ok(keys)
                }
                _ => Err("unset keys must be a JSON array of strings".to_string()),
            }
        }
    }
}

#[derive(serde::Serialize)]
struct QueryResultJson {
    columns: Vec<String>,
    rows: Vec<Vec<serde_json::Value>>,
    affected: u64,
}

impl From<FfiQueryResult> for QueryResultJson {
    fn from(r: FfiQueryResult) -> Self {
        Self {
            columns: r.columns,
            rows: r.rows.into_iter().map(|row| row.into_iter().map(prop_to_json).collect()).collect(),
            affected: r.affected,
        }
    }
}

#[derive(serde::Serialize)]
struct NodeRecordJson {
    id: u64,
    label: String,
    properties: serde_json::Value,
}

impl From<FfiNodeRecord> for NodeRecordJson {
    fn from(n: FfiNodeRecord) -> Self {
        Self {
            id: n.id,
            label: n.label,
            properties: prop_to_json(FfiPropValue::Map(n.properties)),
        }
    }
}

#[derive(serde::Serialize)]
struct EdgeRecordJson {
    id: u64,
    from: u64,
    to: u64,
    edge_type: String,
    properties: serde_json::Value,
}

impl From<FfiEdgeRecord> for EdgeRecordJson {
    fn from(e: FfiEdgeRecord) -> Self {
        Self {
            id: e.id,
            from: e.from,
            to: e.to,
            edge_type: e.edge_type,
            properties: prop_to_json(FfiPropValue::Map(e.properties)),
        }
    }
}

#[derive(serde::Serialize)]
struct TypedNeighborJson {
    node_id: u64,
    edge_id: u64,
    edge_type: String,
}

impl From<FfiTypedNeighbor> for TypedNeighborJson {
    fn from(n: FfiTypedNeighbor) -> Self {
        Self {
            node_id: n.node_id,
            edge_id: n.edge_id,
            edge_type: n.edge_type,
        }
    }
}

#[derive(serde::Serialize)]
struct ScoredRowJson {
    pk: serde_json::Value,
    values: serde_json::Value,
    score: f64,
}

impl From<crate::relational::FfiScoredRow> for ScoredRowJson {
    fn from(s: crate::relational::FfiScoredRow) -> Self {
        Self {
            pk: prop_to_json(s.row.pk),
            values: prop_to_json(FfiPropValue::Map(s.row.values)),
            score: s.score,
        }
    }
}

// ---------------------------------------------------------------------------
// Memory Management
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bkndb_free_string(s: *mut c_char) {
    if !s.is_null() {
        drop(CString::from_raw(s));
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bkndb_free_engine(engine: *mut BknDbEngine) {
    if !engine.is_null() {
        drop(Box::from_raw(engine as *mut Arc<BknDbEngine>));
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bkndb_free_transaction(tx: *mut BknDbTransaction) {
    if !tx.is_null() {
        drop(Box::from_raw(tx as *mut Arc<BknDbTransaction>));
    }
}

// ---------------------------------------------------------------------------
// Lifecycle & Maintenance
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bkndb_open(path: *const c_char, err_out: *mut *mut c_char) -> *mut BknDbEngine {
    clear_err(err_out);
    let path_str = match c_to_str(path) {
        Some(s) => s.to_string(),
        None => {
            set_err(err_out, "path must not be null");
            return std::ptr::null_mut();
        }
    };
    match BknDbEngine::open(path_str) {
        Ok(engine) => Box::into_raw(Box::new(engine)) as *mut BknDbEngine,
        Err(e) => {
            set_err(err_out, e);
            std::ptr::null_mut()
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bkndb_in_memory(err_out: *mut *mut c_char) -> *mut BknDbEngine {
    clear_err(err_out);
    match BknDbEngine::in_memory() {
        Ok(engine) => Box::into_raw(Box::new(engine)) as *mut BknDbEngine,
        Err(e) => {
            set_err(err_out, e);
            std::ptr::null_mut()
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bkndb_close(engine: *mut BknDbEngine, err_out: *mut *mut c_char) -> i32 {
    clear_err(err_out);
    if engine.is_null() {
        set_err(err_out, "engine must not be null");
        return -1;
    }
    let engine_arc = &*(engine as *const Arc<BknDbEngine>);
    match engine_arc.close() {
        Ok(()) => 0,
        Err(e) => {
            set_err(err_out, e);
            -1
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bkndb_is_closed(engine: *mut BknDbEngine) -> bool {
    if engine.is_null() {
        return true;
    }
    let engine_arc = &*(engine as *const Arc<BknDbEngine>);
    engine_arc.is_closed()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bkndb_compact(engine: *mut BknDbEngine, err_out: *mut *mut c_char) -> i32 {
    clear_err(err_out);
    if engine.is_null() {
        set_err(err_out, "engine must not be null");
        return -1;
    }
    let engine_arc = &*(engine as *const Arc<BknDbEngine>);
    match engine_arc.compact() {
        Ok(()) => 0,
        Err(e) => {
            set_err(err_out, e);
            -1
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bkndb_backup(
    engine: *mut BknDbEngine,
    dest: *const c_char,
    err_out: *mut *mut c_char,
) -> i32 {
    clear_err(err_out);
    if engine.is_null() {
        set_err(err_out, "engine must not be null");
        return -1;
    }
    let dest_str = match c_to_str(dest) {
        Some(s) => s.to_string(),
        None => {
            set_err(err_out, "dest must not be null");
            return -1;
        }
    };
    let engine_arc = &*(engine as *const Arc<BknDbEngine>);
    match engine_arc.backup(dest_str) {
        Ok(()) => 0,
        Err(e) => {
            set_err(err_out, e);
            -1
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bkndb_stats(engine: *mut BknDbEngine, err_out: *mut *mut c_char) -> *mut c_char {
    clear_err(err_out);
    if engine.is_null() {
        set_err(err_out, "engine must not be null");
        return std::ptr::null_mut();
    }
    let engine_arc = &*(engine as *const Arc<BknDbEngine>);
    match engine_arc.stats() {
        Ok(stats) => {
            #[derive(serde::Serialize)]
            struct TableCountJson {
                table: String,
                rows: u64,
            }
            #[derive(serde::Serialize)]
            struct StorageStatsJson {
                file_bytes: u64,
            }
            #[derive(serde::Serialize)]
            struct DbStatsJson {
                nodes: u64,
                edges: u64,
                tables: Vec<TableCountJson>,
                storage: Option<StorageStatsJson>,
            }

            let json_val = DbStatsJson {
                nodes: stats.nodes,
                edges: stats.edges,
                tables: stats
                    .tables
                    .into_iter()
                    .map(|t| TableCountJson {
                        table: t.table,
                        rows: t.rows,
                    })
                    .collect(),
                storage: stats.storage.map(|s| StorageStatsJson {
                    file_bytes: s.file_bytes,
                }),
            };
            let s = serde_json::to_string(&json_val).unwrap_or_default();
            to_c_string(s)
        }
        Err(e) => {
            set_err(err_out, e);
            std::ptr::null_mut()
        }
    }
}

// ---------------------------------------------------------------------------
// SQL & Graph Queries
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bkndb_sql(
    engine: *mut BknDbEngine,
    query: *const c_char,
    params_json: *const c_char,
    err_out: *mut *mut c_char,
) -> *mut c_char {
    clear_err(err_out);
    if engine.is_null() {
        set_err(err_out, "engine must not be null");
        return std::ptr::null_mut();
    }
    let query_str = match c_to_str(query) {
        Some(s) => s.to_string(),
        None => {
            set_err(err_out, "query must not be null");
            return std::ptr::null_mut();
        }
    };
    let (positional, named) = match parse_params_json(params_json) {
        Ok(p) => p,
        Err(e) => {
            set_err(err_out, e);
            return std::ptr::null_mut();
        }
    };
    let engine_arc = &*(engine as *const Arc<BknDbEngine>);
    match engine_arc.sql(query_str, positional, named) {
        Ok(res) => {
            let json_obj = QueryResultJson::from(res);
            let s = serde_json::to_string(&json_obj).unwrap_or_default();
            to_c_string(s)
        }
        Err(e) => {
            set_err(err_out, e);
            std::ptr::null_mut()
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bkndb_graph_query(
    engine: *mut BknDbEngine,
    query: *const c_char,
    params_json: *const c_char,
    err_out: *mut *mut c_char,
) -> *mut c_char {
    clear_err(err_out);
    if engine.is_null() {
        set_err(err_out, "engine must not be null");
        return std::ptr::null_mut();
    }
    let query_str = match c_to_str(query) {
        Some(s) => s.to_string(),
        None => {
            set_err(err_out, "query must not be null");
            return std::ptr::null_mut();
        }
    };
    let (positional, named) = match parse_params_json(params_json) {
        Ok(p) => p,
        Err(e) => {
            set_err(err_out, e);
            return std::ptr::null_mut();
        }
    };
    let engine_arc = &*(engine as *const Arc<BknDbEngine>);
    match engine_arc.graph_query(query_str, positional, named) {
        Ok(res) => {
            let json_obj = QueryResultJson::from(res);
            let s = serde_json::to_string(&json_obj).unwrap_or_default();
            to_c_string(s)
        }
        Err(e) => {
            set_err(err_out, e);
            std::ptr::null_mut()
        }
    }
}

// ---------------------------------------------------------------------------
// Graph Operations
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bkndb_create_node(
    engine: *mut BknDbEngine,
    label: *const c_char,
    props_json: *const c_char,
    out_id: *mut u64,
    err_out: *mut *mut c_char,
) -> i32 {
    clear_err(err_out);
    if engine.is_null() {
        set_err(err_out, "engine must not be null");
        return -1;
    }
    let label_str = match c_to_str(label) {
        Some(s) => s.to_string(),
        None => {
            set_err(err_out, "label must not be null");
            return -1;
        }
    };
    let props = match parse_props_json(props_json) {
        Ok(p) => p,
        Err(e) => {
            set_err(err_out, e);
            return -1;
        }
    };
    let engine_arc = &*(engine as *const Arc<BknDbEngine>);
    match engine_arc.create_node(label_str, props) {
        Ok(id) => {
            if !out_id.is_null() {
                *out_id = id;
            }
            0
        }
        Err(e) => {
            set_err(err_out, e);
            -1
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bkndb_get_node(
    engine: *mut BknDbEngine,
    id: u64,
    err_out: *mut *mut c_char,
) -> *mut c_char {
    clear_err(err_out);
    if engine.is_null() {
        set_err(err_out, "engine must not be null");
        return std::ptr::null_mut();
    }
    let engine_arc = &*(engine as *const Arc<BknDbEngine>);
    match engine_arc.get_node(id) {
        Ok(Some(node)) => {
            let json_obj = NodeRecordJson::from(node);
            let s = serde_json::to_string(&json_obj).unwrap_or_default();
            to_c_string(s)
        }
        Ok(None) => to_c_string("null"),
        Err(e) => {
            set_err(err_out, e);
            std::ptr::null_mut()
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bkndb_delete_node(engine: *mut BknDbEngine, id: u64, err_out: *mut *mut c_char) -> i32 {
    clear_err(err_out);
    if engine.is_null() {
        set_err(err_out, "engine must not be null");
        return -1;
    }
    let engine_arc = &*(engine as *const Arc<BknDbEngine>);
    match engine_arc.delete_node(id) {
        Ok(()) => 0,
        Err(e) => {
            set_err(err_out, e);
            -1
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bkndb_update_node(
    engine: *mut BknDbEngine,
    id: u64,
    set_props_json: *const c_char,
    unset_keys_json: *const c_char,
    err_out: *mut *mut c_char,
) -> i32 {
    clear_err(err_out);
    if engine.is_null() {
        set_err(err_out, "engine must not be null");
        return -1;
    }
    let set_props = match parse_props_json(set_props_json) {
        Ok(p) => p,
        Err(e) => {
            set_err(err_out, e);
            return -1;
        }
    };
    let unset_keys = match parse_unset_keys_json(unset_keys_json) {
        Ok(k) => k,
        Err(e) => {
            set_err(err_out, e);
            return -1;
        }
    };
    let engine_arc = &*(engine as *const Arc<BknDbEngine>);
    match engine_arc.update_node_properties(id, set_props, unset_keys) {
        Ok(()) => 0,
        Err(e) => {
            set_err(err_out, e);
            -1
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bkndb_create_edge(
    engine: *mut BknDbEngine,
    from: u64,
    edge_type: *const c_char,
    to: u64,
    props_json: *const c_char,
    out_id: *mut u64,
    err_out: *mut *mut c_char,
) -> i32 {
    clear_err(err_out);
    if engine.is_null() {
        set_err(err_out, "engine must not be null");
        return -1;
    }
    let type_str = match c_to_str(edge_type) {
        Some(s) => s.to_string(),
        None => {
            set_err(err_out, "edge_type must not be null");
            return -1;
        }
    };
    let props = match parse_props_json(props_json) {
        Ok(p) => p,
        Err(e) => {
            set_err(err_out, e);
            return -1;
        }
    };
    let engine_arc = &*(engine as *const Arc<BknDbEngine>);
    match engine_arc.create_edge(from, type_str, to, props) {
        Ok(id) => {
            if !out_id.is_null() {
                *out_id = id;
            }
            0
        }
        Err(e) => {
            set_err(err_out, e);
            -1
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bkndb_get_edge(
    engine: *mut BknDbEngine,
    id: u64,
    err_out: *mut *mut c_char,
) -> *mut c_char {
    clear_err(err_out);
    if engine.is_null() {
        set_err(err_out, "engine must not be null");
        return std::ptr::null_mut();
    }
    let engine_arc = &*(engine as *const Arc<BknDbEngine>);
    match engine_arc.get_edge(id) {
        Ok(Some(edge)) => {
            let json_obj = EdgeRecordJson::from(edge);
            let s = serde_json::to_string(&json_obj).unwrap_or_default();
            to_c_string(s)
        }
        Ok(None) => to_c_string("null"),
        Err(e) => {
            set_err(err_out, e);
            std::ptr::null_mut()
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bkndb_delete_edge(
    engine: *mut BknDbEngine,
    id: u64,
    out_deleted: *mut bool,
    err_out: *mut *mut c_char,
) -> i32 {
    clear_err(err_out);
    if engine.is_null() {
        set_err(err_out, "engine must not be null");
        return -1;
    }
    let engine_arc = &*(engine as *const Arc<BknDbEngine>);
    match engine_arc.delete_edge(id) {
        Ok(deleted) => {
            if !out_deleted.is_null() {
                *out_deleted = deleted;
            }
            0
        }
        Err(e) => {
            set_err(err_out, e);
            -1
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bkndb_neighbors(
    engine: *mut BknDbEngine,
    node: u64,
    direction: i32,
    edge_type: *const c_char,
    err_out: *mut *mut c_char,
) -> *mut c_char {
    clear_err(err_out);
    if engine.is_null() {
        set_err(err_out, "engine must not be null");
        return std::ptr::null_mut();
    }
    let dir = match direction {
        0 => FfiDirection::Out,
        1 => FfiDirection::In,
        _ => FfiDirection::Both,
    };
    let type_opt = c_to_str(edge_type).map(|s| s.to_string());
    let engine_arc = &*(engine as *const Arc<BknDbEngine>);
    match engine_arc.neighbors(node, dir, type_opt) {
        Ok(neighbors) => {
            let list: Vec<TypedNeighborJson> = neighbors.into_iter().map(Into::into).collect();
            let s = serde_json::to_string(&list).unwrap_or_default();
            to_c_string(s)
        }
        Err(e) => {
            set_err(err_out, e);
            std::ptr::null_mut()
        }
    }
}

// ---------------------------------------------------------------------------
// Search (Fulltext BM25 & Vector HNSW)
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bkndb_create_fulltext_index(
    engine: *mut BknDbEngine,
    table: *const c_char,
    column: *const c_char,
    err_out: *mut *mut c_char,
) -> i32 {
    clear_err(err_out);
    if engine.is_null() {
        set_err(err_out, "engine must not be null");
        return -1;
    }
    let table_str = match c_to_str(table) {
        Some(s) => s.to_string(),
        None => {
            set_err(err_out, "table must not be null");
            return -1;
        }
    };
    let col_str = match c_to_str(column) {
        Some(s) => s.to_string(),
        None => {
            set_err(err_out, "column must not be null");
            return -1;
        }
    };
    let engine_arc = &*(engine as *const Arc<BknDbEngine>);
    match engine_arc.create_fulltext_index(table_str, col_str) {
        Ok(_) => 0,
        Err(e) => {
            set_err(err_out, e);
            -1
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bkndb_search_text(
    engine: *mut BknDbEngine,
    table: *const c_char,
    column: *const c_char,
    query: *const c_char,
    limit: u32,
    match_all: bool,
    err_out: *mut *mut c_char,
) -> *mut c_char {
    clear_err(err_out);
    if engine.is_null() {
        set_err(err_out, "engine must not be null");
        return std::ptr::null_mut();
    }
    let table_str = match c_to_str(table) {
        Some(s) => s.to_string(),
        None => {
            set_err(err_out, "table must not be null");
            return std::ptr::null_mut();
        }
    };
    let col_str = match c_to_str(column) {
        Some(s) => s.to_string(),
        None => {
            set_err(err_out, "column must not be null");
            return std::ptr::null_mut();
        }
    };
    let query_str = match c_to_str(query) {
        Some(s) => s.to_string(),
        None => {
            set_err(err_out, "query must not be null");
            return std::ptr::null_mut();
        }
    };
    let engine_arc = &*(engine as *const Arc<BknDbEngine>);
    match engine_arc.search_text(table_str, col_str, query_str, limit, match_all, Vec::new()) {
        Ok(results) => {
            let list: Vec<ScoredRowJson> = results.into_iter().map(Into::into).collect();
            let s = serde_json::to_string(&list).unwrap_or_default();
            to_c_string(s)
        }
        Err(e) => {
            set_err(err_out, e);
            std::ptr::null_mut()
        }
    }
}

fn parse_metric(metric: *const c_char) -> FfiVectorMetric {
    match unsafe { c_to_str(metric) } {
        Some(s) if s.eq_ignore_ascii_case("euclidean") => FfiVectorMetric::Euclidean,
        Some(s) if s.eq_ignore_ascii_case("dot") || s.eq_ignore_ascii_case("dot_product") => {
            FfiVectorMetric::Dot
        }
        _ => FfiVectorMetric::Cosine,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bkndb_create_vector_index(
    engine: *mut BknDbEngine,
    table: *const c_char,
    column: *const c_char,
    metric: *const c_char,
    m: u32,
    ef_construction: u32,
    err_out: *mut *mut c_char,
) -> i32 {
    clear_err(err_out);
    if engine.is_null() {
        set_err(err_out, "engine must not be null");
        return -1;
    }
    let table_str = match c_to_str(table) {
        Some(s) => s.to_string(),
        None => {
            set_err(err_out, "table must not be null");
            return -1;
        }
    };
    let col_str = match c_to_str(column) {
        Some(s) => s.to_string(),
        None => {
            set_err(err_out, "column must not be null");
            return -1;
        }
    };
    let vector_metric = parse_metric(metric);
    let engine_arc = &*(engine as *const Arc<BknDbEngine>);
    match engine_arc.create_vector_index(table_str, col_str, vector_metric, m, ef_construction) {
        Ok(_) => 0,
        Err(e) => {
            set_err(err_out, e);
            -1
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bkndb_search_vector(
    engine: *mut BknDbEngine,
    table: *const c_char,
    column: *const c_char,
    vector_ptr: *const f32,
    vector_len: u32,
    limit: u32,
    metric: *const c_char,
    exact: bool,
    err_out: *mut *mut c_char,
) -> *mut c_char {
    clear_err(err_out);
    if engine.is_null() {
        set_err(err_out, "engine must not be null");
        return std::ptr::null_mut();
    }
    if vector_ptr.is_null() || vector_len == 0 {
        set_err(err_out, "vector must not be empty or null");
        return std::ptr::null_mut();
    }
    let table_str = match c_to_str(table) {
        Some(s) => s.to_string(),
        None => {
            set_err(err_out, "table must not be null");
            return std::ptr::null_mut();
        }
    };
    let col_str = match c_to_str(column) {
        Some(s) => s.to_string(),
        None => {
            set_err(err_out, "column must not be null");
            return std::ptr::null_mut();
        }
    };
    let vec_slice = std::slice::from_raw_parts(vector_ptr, vector_len as usize).to_vec();
    let vector_metric = parse_metric(metric);
    let engine_arc = &*(engine as *const Arc<BknDbEngine>);
    match engine_arc.search_vector(
        table_str,
        col_str,
        vec_slice,
        limit,
        vector_metric,
        Vec::new(),
        exact,
        None,
    ) {
        Ok(results) => {
            let list: Vec<ScoredRowJson> = results.into_iter().map(Into::into).collect();
            let s = serde_json::to_string(&list).unwrap_or_default();
            to_c_string(s)
        }
        Err(e) => {
            set_err(err_out, e);
            std::ptr::null_mut()
        }
    }
}

// ---------------------------------------------------------------------------
// Explicit Transactions
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bkndb_begin_transaction(
    engine: *mut BknDbEngine,
    err_out: *mut *mut c_char,
) -> *mut BknDbTransaction {
    clear_err(err_out);
    if engine.is_null() {
        set_err(err_out, "engine must not be null");
        return std::ptr::null_mut();
    }
    let engine_arc = &*(engine as *const Arc<BknDbEngine>);
    match engine_arc.begin_transaction() {
        Ok(tx) => Box::into_raw(Box::new(tx)) as *mut BknDbTransaction,
        Err(e) => {
            set_err(err_out, e);
            std::ptr::null_mut()
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bkndb_tx_sql(
    tx: *mut BknDbTransaction,
    query: *const c_char,
    params_json: *const c_char,
    err_out: *mut *mut c_char,
) -> *mut c_char {
    clear_err(err_out);
    if tx.is_null() {
        set_err(err_out, "transaction must not be null");
        return std::ptr::null_mut();
    }
    let query_str = match c_to_str(query) {
        Some(s) => s.to_string(),
        None => {
            set_err(err_out, "query must not be null");
            return std::ptr::null_mut();
        }
    };
    let (positional, named) = match parse_params_json(params_json) {
        Ok(p) => p,
        Err(e) => {
            set_err(err_out, e);
            return std::ptr::null_mut();
        }
    };
    let tx_arc = &*(tx as *const Arc<BknDbTransaction>);
    match tx_arc.sql(query_str, positional, named) {
        Ok(res) => {
            let json_obj = QueryResultJson::from(res);
            let s = serde_json::to_string(&json_obj).unwrap_or_default();
            to_c_string(s)
        }
        Err(e) => {
            set_err(err_out, e);
            std::ptr::null_mut()
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bkndb_tx_commit(tx: *mut BknDbTransaction, err_out: *mut *mut c_char) -> i32 {
    clear_err(err_out);
    if tx.is_null() {
        set_err(err_out, "transaction must not be null");
        return -1;
    }
    let tx_arc = &*(tx as *const Arc<BknDbTransaction>);
    match tx_arc.commit() {
        Ok(()) => 0,
        Err(e) => {
            set_err(err_out, e);
            -1
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bkndb_tx_rollback(tx: *mut BknDbTransaction, err_out: *mut *mut c_char) -> i32 {
    clear_err(err_out);
    if tx.is_null() {
        set_err(err_out, "transaction must not be null");
        return -1;
    }
    let tx_arc = &*(tx as *const Arc<BknDbTransaction>);
    match tx_arc.rollback() {
        Ok(()) => 0,
        Err(e) => {
            set_err(err_out, e);
            -1
        }
    }
}
