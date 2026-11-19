//! Versioned, bounded RPC ABI for the same SQL/graph/search core used natively.
//! Each instance owns one database and at most one owned write transaction.
mod host;
use bkndb_core::{
    BknError, Db, DbWriteBatch,
    graph::{EdgeId, NodeId},
    lang::{Params, QueryResult},
    relational::{VectorIndexOptions, VectorMetric, VectorSearchOptions},
    value::{PropValue, Properties},
};
use bkndb_storage_lsm::{LsmOptions, LsmStorageBackend, LsmWriteTx};
use serde_json::{Value as J, json};
use std::{cell::RefCell, collections::BTreeMap, sync::Arc};
const MAX_BUFFER: usize = 16 * 1024 * 1024;
const MAX_BUFFERS: usize = 4;
#[derive(Default)]
struct State {
    db: Option<Db<LsmStorageBackend>>,
    tx: Option<DbWriteBatch<LsmWriteTx<'static>>>,
    aborted: bool,
    token: u64,
}
thread_local! {
    static STATE:RefCell<State> = RefCell::new(State::default());
    static BUFFERS:RefCell<BTreeMap<u32,Box<[u8]>>> = RefCell::new(BTreeMap::new());
}
#[unsafe(no_mangle)]
pub extern "C" fn bkndb_abi_version() -> u32 {
    1
}
#[unsafe(no_mangle)]
pub extern "C" fn bkndb_alloc(len: u32) -> u32 {
    if len == 0 || len as usize > MAX_BUFFER {
        return 0;
    }
    BUFFERS.with(|b| {
        let mut b = b.borrow_mut();
        if b.len() >= MAX_BUFFERS {
            return 0;
        }
        let buf = vec![0; len as usize].into_boxed_slice();
        let ptr = buf.as_ptr() as u32;
        b.insert(ptr, buf);
        ptr
    })
}
#[unsafe(no_mangle)]
pub extern "C" fn bkndb_dealloc(ptr: u32) {
    BUFFERS.with(|b| {
        b.borrow_mut().remove(&ptr);
    });
}
fn response(value: J) -> u64 {
    let mut bytes = serde_json::to_vec(&value)
        .unwrap_or_else(|_| br#"{"error":"response encoding failed"}"#.to_vec());
    if bytes.len() > MAX_BUFFER {
        bytes = br#"{"error":"ResourceLimit: response exceeds 16 MiB"}"#.to_vec();
    }
    let len = bytes.len() as u32;
    let ptr = bkndb_alloc(len);
    if ptr == 0 {
        return 0;
    }
    BUFFERS.with(|b| {
        b.borrow_mut()
            .get_mut(&ptr)
            .unwrap()
            .copy_from_slice(&bytes)
    });
    ((ptr as u64) << 32) | len as u64
}
#[unsafe(no_mangle)]
pub extern "C" fn bkndb_request(ptr: u32, len: u32) -> u64 {
    let input = BUFFERS.with(|b| b.borrow_mut().remove(&ptr));
    let result = input
        .filter(|b| b.len() == len as usize)
        .ok_or_else(|| invalid("unregistered request buffer"))
        .and_then(|b| serde_json::from_slice::<J>(&b).map_err(|e| invalid(e.to_string())))
        .and_then(|r| STATE.with(|s| dispatch(&mut s.borrow_mut(), &r)));
    response(match result {
        Ok(v) => json!({"result":v}),
        Err(e) => json!({"error":e.to_string()}),
    })
}
fn invalid(s: impl Into<String>) -> BknError {
    BknError::InvalidArgument(s.into())
}
fn text<'a>(r: &'a J, key: &str) -> Result<&'a str, BknError> {
    r[key]
        .as_str()
        .ok_or_else(|| invalid(format!("{key} must be a string")))
}
fn number(r: &J, key: &str) -> Result<u64, BknError> {
    r[key]
        .as_u64()
        .ok_or_else(|| invalid(format!("{key} must be an unsigned 64-bit integer")))
}
fn size(r: &J, key: &str) -> Result<usize, BknError> {
    usize::try_from(number(r, key)?).map_err(|_| invalid(format!("{key} exceeds address space")))
}
fn prop(v: &J) -> Result<PropValue, BknError> {
    Ok(match v {
        J::Null => PropValue::Null,
        J::Bool(b) => PropValue::Bool(*b),
        J::String(s) => PropValue::Str(s.clone()),
        J::Number(n) => {
            if let Some(i) = n.as_i64() {
                PropValue::Int(i)
            } else {
                PropValue::Float(n.as_f64().ok_or_else(|| invalid("invalid numeric value"))?)
            }
        }
        J::Array(a) => PropValue::List(a.iter().map(prop).collect::<Result<_, _>>()?),
        J::Object(m) => PropValue::Map(
            m.iter()
                .map(|(k, v)| Ok((k.clone(), prop(v)?)))
                .collect::<Result<_, BknError>>()?,
        ),
    })
}
fn props(v: &J) -> Result<Properties, BknError> {
    if v.is_null() {
        return Ok(Properties::new());
    }
    match prop(v)? {
        PropValue::Map(p) => Ok(p),
        _ => Err(invalid("properties must be an object")),
    }
}
fn params(v: &J) -> Result<Params, BknError> {
    Ok(match v {
        J::Null => Params::none(),
        J::Array(a) => Params::positional(a.iter().map(prop).collect::<Result<Vec<_>, _>>()?),
        J::Object(_) => Params::named(props(v)?),
        _ => return Err(invalid("params must be an array or object")),
    })
}
fn json_prop(p: PropValue) -> J {
    match p {
        PropValue::Null => J::Null,
        PropValue::Bool(v) => json!(v),
        PropValue::Int(v) | PropValue::Timestamp(v) => json!(v),
        PropValue::Float(v) => json!(v),
        PropValue::Str(v) => json!(v),
        PropValue::Bytes(v) => json!(v),
        PropValue::Uuid(v) => json!(v.iter().map(|b| format!("{b:02x}")).collect::<String>()),
        PropValue::List(v) => J::Array(v.into_iter().map(json_prop).collect()),
        PropValue::Map(v) => J::Object(v.into_iter().map(|(k, v)| (k, json_prop(v))).collect()),
    }
}
fn result(r: QueryResult) -> J {
    json!({"columns":r.columns,"rows":r.rows.into_iter().map(|r|r.into_iter().map(json_prop).collect::<Vec<_>>()).collect::<Vec<_>>(),"affected":r.affected})
}
fn metric(r: &J) -> Result<VectorMetric, BknError> {
    match text(r, "metric")?.to_ascii_lowercase().as_str() {
        "cosine" => Ok(VectorMetric::Cosine),
        "euclidean" => Ok(VectorMetric::Euclidean),
        "dot" | "dotproduct" | "dot_product" => Ok(VectorMetric::Dot),
        _ => Err(invalid("unsupported vector metric")),
    }
}
fn scored(rows: Vec<bkndb_core::relational::ScoredRow>) -> J {
    J::Array(rows.into_iter().map(|s|json!({"pk":json_prop(s.row.pk),"values":json_prop(PropValue::Map(s.row.values)),"score":s.score})).collect())
}
fn dispatch(s: &mut State, r: &J) -> Result<J, BknError> {
    let op = text(r, "op")?;
    if op == "open" {
        if s.db.is_some() {
            return Err(invalid("database already open"));
        }
        s.db = Some(Db::new(LsmStorageBackend::open_with_io(
            "database",
            LsmOptions::default(),
            Arc::new(host::HostIo),
        )?));
        return Ok(J::Null);
    }
    if op == "close" {
        s.tx = None;
        s.db = None;
        return Ok(J::Null);
    }
    let db = s.db.as_ref().ok_or_else(|| invalid("database is closed"))?;
    if matches!(op, "tx_sql" | "commit" | "rollback")
        && (s.tx.is_none() || number(r, "transaction")? != s.token)
    {
        return Err(invalid("transaction is closed"));
    }
    match op {
        "begin" => {
            if s.tx.is_some() {
                return Err(invalid("transaction in progress"));
            }
            s.token = number(r, "transaction")?;
            s.tx = Some(db.begin_write()?);
            s.aborted = false;
            Ok(J::Null)
        }
        "rollback" => {
            s.tx = None;
            s.aborted = false;
            Ok(J::Null)
        }
        "commit" => {
            let tx =
                s.tx.take()
                    .ok_or_else(|| invalid("transaction is closed"))?;
            if s.aborted {
                return Err(invalid("transaction aborted"));
            }
            tx.commit()?;
            Ok(J::Null)
        }
        "tx_sql" => {
            if s.aborted {
                return Err(invalid("transaction aborted"));
            }
            let output = (|| {
                let p = params(&r["params"])?;
                s.tx.as_mut()
                    .ok_or_else(|| invalid("transaction is closed"))?
                    .relational()
                    .sql(text(r, "query")?, p)
            })();
            if output.is_err() {
                s.aborted = true;
            }
            output.map(result)
        }
        "sql" => {
            let q = text(r, "query")?;
            let p = params(&r["params"])?;
            if bkndb_core::lang::sql::parse(q)?.is_read_only() {
                db.read_tx(|b| b.relational().sql(q, p)).map(result)
            } else {
                if s.tx.is_some() {
                    return Err(invalid("transaction in progress"));
                }
                db.write_tx(|b| b.relational().sql(q, p)).map(result)
            }
        }
        "graph_query" => db
            .graph()
            .query(text(r, "query")?, params(&r["params"])?)
            .map(result),
        "stats" => {
            let logical = db.stats()?;
            let physical = db.backend().stats()?;
            Ok(
                json!({"nodes":logical.nodes,"edges":logical.edges,"tables":logical.tables.into_iter().map(|(table,rows)|json!({"table":table,"rows":rows})).collect::<Vec<_>>(),"storage":{"file_bytes":physical.file_bytes}}),
            )
        }
        "backup" => {
            db.backend().backup_to("backup")?;
            Ok(J::Null)
        }
        "get_node" => {
            let id = number(r, "id")?;
            Ok(match db.graph().get_node(NodeId(id))? {
                Some(n) => {
                    json!({"id":id,"label":n.label,"properties":json_prop(PropValue::Map(n.properties))})
                }
                None => J::Null,
            })
        }
        "get_edge" => {
            let id = number(r, "id")?;
            Ok(match db.graph().get_edge(EdgeId(id))? {
                Some(e) => {
                    json!({"id":id,"from":e.from.0,"to":e.to.0,"edge_type":e.edge_type,"properties":json_prop(PropValue::Map(e.properties))})
                }
                None => J::Null,
            })
        }
        "neighbors" => {
            let node = NodeId(number(r, "id")?);
            let dir = number(r, "direction")?;
            let kind = text(r, "edge_type")?;
            if dir > 2 {
                return Err(invalid("invalid direction"));
            }
            let mut rows = Vec::new();
            db.read_tx(|b| {
                let graph = b.graph();
                if dir == 0 || dir == 2 {
                    rows.extend(graph.neighbors_out_any(node)?);
                }
                if dir == 1 || dir == 2 {
                    rows.extend(graph.neighbors_in_any(node)?);
                }
                Ok(())
            })?;
            Ok(J::Array(
                rows.into_iter()
                    .filter(|(k, _, _)| kind.is_empty() || k == kind)
                    .map(|(k, n, e)| json!({"node_id":n.0,"edge_id":e.0,"edge_type":k}))
                    .collect(),
            ))
        }
        "search_text" => db
            .relational()
            .search_text(
                text(r, "table")?,
                text(r, "column")?,
                text(r, "query")?,
                size(r, "limit")?,
                r["match_all"].as_bool().unwrap_or(false),
                None,
            )
            .map(scored),
        "search_vector" => {
            let v = r["vector"]
                .as_array()
                .ok_or_else(|| invalid("vector must be an array"))?
                .iter()
                .map(|v| {
                    v.as_f64()
                        .map(|v| v as f32)
                        .filter(|v| v.is_finite())
                        .ok_or_else(|| invalid("invalid vector component"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            db.relational()
                .search_vector_with(
                    text(r, "table")?,
                    text(r, "column")?,
                    &v,
                    size(r, "limit")?,
                    metric(r)?,
                    None,
                    VectorSearchOptions {
                        exact: r["exact"].as_bool().unwrap_or(false),
                        ef_search: None,
                    },
                )
                .map(scored)
        }
        _ => {
            if s.tx.is_some() {
                return Err(invalid("transaction in progress"));
            }
            match op {
                "compact" => {
                    db.backend().force_compact()?;
                    Ok(J::Null)
                }
                "create_node" => db
                    .graph()
                    .create_node(text(r, "label")?, props(&r["props"])?)
                    .map(|n| json!(n.0)),
                "delete_node" => {
                    db.graph().delete_node(NodeId(number(r, "id")?))?;
                    Ok(J::Null)
                }
                "update_node" => {
                    let set = props(&r["props"])?;
                    let unset = r["unset"]
                        .as_array()
                        .ok_or_else(|| invalid("unset must be array"))?
                        .iter()
                        .map(|v| {
                            v.as_str()
                                .map(str::to_owned)
                                .ok_or_else(|| invalid("unset keys must be strings"))
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    db.graph()
                        .update_node_properties(NodeId(number(r, "id")?), |p| {
                            p.extend(set);
                            for k in unset {
                                p.remove(&k);
                            }
                        })?;
                    Ok(J::Null)
                }
                "create_edge" => db
                    .graph()
                    .create_edge(
                        NodeId(number(r, "from")?),
                        text(r, "edge_type")?,
                        NodeId(number(r, "to")?),
                        props(&r["props"])?,
                    )
                    .map(|e| json!(e.0)),
                "delete_edge" => db
                    .graph()
                    .delete_edge(EdgeId(number(r, "id")?))
                    .map(|b| json!(b)),
                "create_fulltext_index" => {
                    db.relational()
                        .create_fulltext_index(text(r, "table")?, text(r, "column")?)?;
                    Ok(J::Null)
                }
                "create_vector_index" => {
                    db.relational().create_vector_index(
                        text(r, "table")?,
                        text(r, "column")?,
                        VectorIndexOptions {
                            metric: metric(r)?,
                            m: size(r, "m")?,
                            ef_construction: size(r, "ef")?,
                        },
                    )?;
                    Ok(J::Null)
                }
                _ => Err(invalid("unsupported operation")),
            }
        }
    }
}
