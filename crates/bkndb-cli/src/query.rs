use crate::{
    args::*,
    backend, checkpoint,
    error::{Error, Result},
    files::Database,
};
use bkndb::{
    Db, StorageBackend, StorageReadTx, TableSpec,
    graph::{EdgeId, NodeId},
    lang::{Params, QueryResult, split_sql_script, sql},
    value::{PropValue, Properties},
};
use serde_json::{Value, json};
use std::ops::Bound;

pub fn require_write(write: bool) -> Result<()> {
    if !write {
        Err(Error::input("this operation requires --write"))
    } else {
        Ok(())
    }
}
pub fn value(v: Value) -> Result<PropValue> {
    Ok(match v {
        Value::Null => PropValue::Null,
        Value::Bool(v) => PropValue::Bool(v),
        Value::String(v) => PropValue::Str(v),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                PropValue::Int(i)
            } else if n.is_f64() {
                PropValue::Float(n.as_f64().ok_or_else(|| Error::input("invalid float"))?)
            } else {
                return Err(Error::input(
                    "property integers must fit signed 64 bits; graph IDs use separate uint64 arguments",
                ));
            }
        }
        Value::Array(a) => PropValue::List(a.into_iter().map(value).collect::<Result<_>>()?),
        Value::Object(mut m) => {
            if m.len() == 1 && m.contains_key("$bkndb") {
                from_wire(m.remove("$bkndb").unwrap())?
            } else {
                PropValue::Map(
                    m.into_iter()
                        .map(|(k, v)| Ok((k, value(v)?)))
                        .collect::<Result<_>>()?,
                )
            }
        }
    })
}
pub fn params(text: &str) -> Result<Params> {
    if text.len() > 16 * 1024 * 1024 {
        return Err(Error::new(9, "parameters exceed 16 MiB"));
    }
    match serde_json::from_str(text)? {
        Value::Array(a) => Ok(Params::positional(
            a.into_iter().map(value).collect::<Result<Vec<_>>>()?,
        )),
        Value::Object(m) => Ok(Params::named(
            m.into_iter()
                .map(|(k, v)| Ok((k, value(v)?)))
                .collect::<Result<Vec<_>>>()?,
        )),
        _ => Err(Error::input("parameters must be a JSON array or object")),
    }
}
pub fn properties(text: &str) -> Result<Properties> {
    match value(serde_json::from_str(text)?)? {
        PropValue::Map(m) => Ok(m),
        _ => Err(Error::input("properties must be a JSON object")),
    }
}
pub fn result(r: QueryResult) -> Value {
    json!({"columns":r.columns,"rows":r.rows.into_iter().map(|row|row.into_iter().map(to_wire).collect::<Vec<_>>()).collect::<Vec<_>>(),"affected":r.affected})
}
pub fn sql(
    db: &Db<backend::Backend>,
    source: &str,
    params: &Params,
    write: bool,
    per_statement: bool,
) -> Result<Value> {
    if source.len() > 16 * 1024 * 1024 {
        return Err(Error::new(9, "SQL script exceeds 16 MiB"));
    }
    let stmts = split_sql_script(source)?;
    // Parse everything before executing; a bad statement cannot partially apply
    // an atomic script, and mutation checks never depend on text prefixes.
    let parsed = stmts
        .iter()
        .map(|s| sql::parse(s))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mutating = parsed.iter().any(|s| !s.is_read_only());
    if mutating {
        require_write(write)?;
    }
    let mut results = Vec::new();
    if per_statement {
        for s in stmts {
            checkpoint()?;
            match db.relational().sql(s, params.clone()) {
                Ok(r) => results.push(result(r)),
                Err(e) => {
                    let original: Error = e.into();
                    if results.is_empty() {
                        return Err(original);
                    }
                    return Err(Error::new(if original.code==8{8}else{10},"per-statement script stopped; earlier statements completed").with_details(json!({"completed_statements":results.len(),"cause":{"code":original.code,"message":original.message}})));
                }
            }
        }
    } else if mutating {
        let mut tx = db.begin_write()?;
        for s in stmts {
            checkpoint()?;
            results.push(result(tx.relational().sql(s, params.clone())?));
        }
        checkpoint()?;
        crate::failpoint("before_commit");
        tx.commit()?;
        crate::failpoint("after_commit");
    } else {
        db.read_tx(|tx| {
            for s in stmts {
                results.push(result(tx.relational().sql(s, params.clone())?));
            }
            Ok(())
        })?;
    }
    if results.len() == 1 {
        Ok(results.remove(0))
    } else {
        Ok(json!({"statements":results,"atomic":!per_statement}))
    }
}
pub fn schemas(db: &Database, table: Option<&str>) -> Result<Value> {
    let tables = db.db.relational().list_tables()?;
    let schemas:Vec<_>=tables.iter().filter(|s|table.is_none_or(|t|t==s.name())).map(|s|json!({"name":s.name(),"columns":s.columns(),"primary_key":s.primary_key(),"auto_increment":s.auto_increment_pk(),"indexed_columns":s.indexed_columns()})).collect();
    if table.is_some() && schemas.is_empty() {
        return Err(Error::input("table not found"));
    }
    Ok(json!(schemas))
}
pub fn indexes(db: &Database, table: Option<&str>) -> Result<Value> {
    let mut out = Vec::new();
    for s in db.db.relational().list_tables()? {
        if table.is_some_and(|t| t != s.name()) {
            continue;
        }
        for c in s.indexed_columns() {
            out.push(json!({"kind":"secondary","table":s.name(),"column":c}));
        }
        for c in db.db.relational().fulltext_indexes(s.name())? {
            out.push(json!({"kind":"text","table":s.name(),"column":c}));
        }
        for v in db.db.relational().vector_indexes(s.name())? {
            out.push(json!({"kind":"vector","table":s.name(),"column":v.column,"metric":format!("{:?}",v.metric),"m":v.m,"ef_construction":v.ef_construction,"dimensions":v.dimensions,"vectors":v.vectors}));
        }
    }
    if table.is_none() {
        for (label, property) in db.db.graph().property_indexes()? {
            out.push(json!({"kind":"property","label":label,"property":property}));
        }
    }
    Ok(json!(out))
}
pub fn graph(command: &Graph, write: bool) -> Result<Value> {
    match command {
        Graph::Query {
            source,
            query,
            params: p,
        } => {
            let db = Database::open(source, false)?;
            Ok(result(db.db.graph().query(query, params(p)?)?))
        }
        Graph::Neighbors {
            source,
            id,
            direction,
        } => {
            let db = Database::open(source, false)?;
            let graph = db.db.graph();
            Ok(json!(if direction == "in" {
                graph.neighbors_in_any(NodeId(*id))?
            } else {
                graph.neighbors_out_any(NodeId(*id))?
            }))
        }
        Graph::Node { command } => node(command, write),
        Graph::Edge { command } => edge(command, write),
    }
}
fn node(command: &Node, write: bool) -> Result<Value> {
    let (source, mutation) = match command {
        Node::Add { source, .. } | Node::Update { source, .. } | Node::Delete { source, .. } => {
            (source, true)
        }
        Node::Get { source, .. } | Node::List { source, .. } => (source, false),
    };
    if mutation {
        require_write(write)?;
    }
    let db = Database::open(source, mutation)?;
    let g = db.db.graph();
    Ok(match command {
        Node::Add {
            label,
            properties: p,
            ..
        } => json!({"id":g.create_node(label,properties(p)?)?.0}),
        Node::Get { id, .. } => json!({"id":id,"node":g.get_node(NodeId(*id))?.map(node_wire)}),
        Node::Update {
            id, properties: p, ..
        } => {
            let p = properties(p)?;
            g.update_node_properties(NodeId(*id), |props| props.extend(p))?;
            json!({"updated":id})
        }
        Node::Delete { id, .. } => {
            g.delete_node(NodeId(*id))?;
            json!({"deleted":id})
        }
        Node::List { label, limit, .. } => {
            limit_check(*limit)?;
            let tx = db.db.backend().begin_read()?;
            let mut rows = Vec::new();
            for item in tx.scan(TableSpec("nodes"), Bound::Unbounded, Bound::Unbounded)? {
                checkpoint()?;
                if rows.len() >= *limit {
                    break;
                }
                let (key, _) = item?;
                let id = u64::from_be_bytes(
                    key.as_slice()
                        .try_into()
                        .map_err(|_| Error::new(5, "invalid node ID"))?,
                );
                if let Some(node) = g.get_node(NodeId(id))? {
                    if label.as_ref().is_none_or(|l| l == &node.label) {
                        rows.push(json!({"id":id,"node":node_wire(node)}));
                    }
                }
            }
            json!(rows)
        }
    })
}
fn edge(command: &Edge, write: bool) -> Result<Value> {
    let (source, mutation) = match command {
        Edge::Add { source, .. } | Edge::Update { source, .. } | Edge::Delete { source, .. } => {
            (source, true)
        }
        Edge::Get { source, .. } | Edge::List { source, .. } => (source, false),
    };
    if mutation {
        require_write(write)?;
    }
    let db = Database::open(source, mutation)?;
    let g = db.db.graph();
    Ok(match command {
        Edge::Add {
            from,
            to,
            edge_type,
            properties: p,
            ..
        } => json!({"id":g.create_edge(NodeId(*from),edge_type,NodeId(*to),properties(p)?)?.0}),
        Edge::Get { id, .. } => json!({"id":id,"edge":g.get_edge(EdgeId(*id))?.map(edge_wire)}),
        Edge::Update {
            id, properties: p, ..
        } => {
            let p = properties(p)?;
            g.update_edge_properties(EdgeId(*id), |props| props.extend(p))?;
            json!({"updated":id})
        }
        Edge::Delete { id, .. } => json!({"deleted":g.delete_edge(EdgeId(*id))?}),
        Edge::List { limit, .. } => {
            limit_check(*limit)?;
            let tx = db.db.backend().begin_read()?;
            let mut rows = Vec::new();
            for item in tx.scan(TableSpec("edges"), Bound::Unbounded, Bound::Unbounded)? {
                checkpoint()?;
                if rows.len() >= *limit {
                    break;
                }
                let (key, _) = item?;
                let id = u64::from_be_bytes(
                    key.as_slice()
                        .try_into()
                        .map_err(|_| Error::new(5, "invalid edge ID"))?,
                );
                rows.push(json!({"id":id,"edge":g.get_edge(EdgeId(id))?.map(edge_wire)}));
            }
            json!(rows)
        }
    })
}
pub fn limit_check(limit: usize) -> Result<()> {
    if limit > 1_000_000 {
        Err(Error::new(9, "result limit exceeds 1,000,000"))
    } else {
        Ok(())
    }
}
pub fn search(command: &Search) -> Result<Value> {
    let (source, table, column, limit) = match command {
        Search::Text {
            source,
            table,
            column,
            limit,
            ..
        }
        | Search::Vector {
            source,
            table,
            column,
            limit,
            ..
        } => (source, table, column, *limit),
    };
    limit_check(limit)?;
    let db = Database::open(source, false)?;
    let rel = db.db.relational();
    let rows = match command {
        Search::Text { query, .. } => rel.search_text(table, column, query, limit, false, None)?,
        Search::Vector {
            vector,
            metric,
            exact,
            ..
        } => {
            let vector: Vec<f32> = serde_json::from_str(vector)?;
            if vector.is_empty() || vector.len() > 65536 || vector.iter().any(|f| !f.is_finite()) {
                return Err(Error::input("vector must contain 1..65536 finite values"));
            }
            rel.search_vector_with(
                table,
                column,
                &vector,
                limit,
                metric.engine(),
                None,
                bkndb::relational::VectorSearchOptions {
                    exact: *exact,
                    ef_search: None,
                },
            )?
        }
    };
    Ok(json!(
        rows.into_iter()
            .map(|r| json!({"pk":to_wire(r.row.pk),"values":properties_wire(r.row.values),"score":r.score}))
            .collect::<Vec<_>>()
    ))
}
pub fn index(command: &Index, write: bool) -> Result<Value> {
    require_write(write)?;
    let (s, create) = match command {
        Index::Create(s) => (s, true),
        Index::Drop(s) => (s, false),
    };
    let db = Database::open(&s.source, true)?;
    let rel = db.db.relational();
    let changed = match (s.kind, create) {
        (IndexKind::Secondary, true) => {
            rel.create_index(&s.table, &s.column)?;
            true
        }
        (IndexKind::Secondary, false) => {
            rel.drop_index(&s.table, &s.column)?;
            true
        }
        (IndexKind::Text, true) => rel.create_fulltext_index(&s.table, &s.column)?,
        (IndexKind::Text, false) => rel.drop_fulltext_index(&s.table, &s.column)?,
        (IndexKind::Vector, true) => rel.create_vector_index(
            &s.table,
            &s.column,
            bkndb::relational::VectorIndexOptions {
                metric: s.metric.engine(),
                m: s.m as usize,
                ef_construction: s.ef_construction as usize,
            },
        )?,
        (IndexKind::Vector, false) => rel.drop_vector_index(&s.table, &s.column)?,
        (IndexKind::Property, true) => db.db.graph().create_property_index(&s.table, &s.column)?,
        (IndexKind::Property, false) => db.db.graph().drop_property_index(&s.table, &s.column)?,
    };
    Ok(json!({"changed":changed}))
}

/// Tagged, lossless value representation. JSON has no NaN/Infinity numbers;
/// FloatBits preserves their IEEE-754 payload rather than silently writing null.
pub fn to_wire(value: PropValue) -> Value {
    match value {
        PropValue::Float(f) if !f.is_finite() => {
            json!({"FloatBits":format!("{:016x}",f.to_bits())})
        }
        PropValue::List(items) => {
            json!({"List":items.into_iter().map(to_wire).collect::<Vec<_>>()})
        }
        PropValue::Map(items) => json!({"Map":properties_wire(items)}),
        other => serde_json::to_value(other).expect("finite PropValue serialization"),
    }
}
pub fn from_wire(value: Value) -> Result<PropValue> {
    if let Some(map) = value.as_object() {
        if map.len() == 1 {
            if let Some(bits) = map.get("FloatBits") {
                let bits = bits
                    .as_str()
                    .filter(|s| s.len() == 16)
                    .ok_or_else(|| Error::input("FloatBits must contain 16 hex digits"))?;
                let bits =
                    u64::from_str_radix(bits, 16).map_err(|e| Error::input(e.to_string()))?;
                return Ok(PropValue::Float(f64::from_bits(bits)));
            }
            if let Some(items) = map.get("List").and_then(Value::as_array) {
                return Ok(PropValue::List(
                    items
                        .iter()
                        .cloned()
                        .map(from_wire)
                        .collect::<Result<_>>()?,
                ));
            }
            if let Some(items) = map.get("Map").and_then(Value::as_object) {
                return Ok(PropValue::Map(
                    items
                        .iter()
                        .map(|(k, v)| Ok((k.clone(), from_wire(v.clone())?)))
                        .collect::<Result<_>>()?,
                ));
            }
        }
    }
    Ok(serde_json::from_value(value)?)
}
fn properties_wire(properties: Properties) -> Value {
    Value::Object(
        properties
            .into_iter()
            .map(|(k, v)| (k, to_wire(v)))
            .collect(),
    )
}
fn node_wire(node: bkndb::graph::NodeRecord) -> Value {
    json!({"label":node.label,"properties":properties_wire(node.properties)})
}
fn edge_wire(edge: bkndb::graph::EdgeRecord) -> Value {
    json!({"from":edge.from,"to":edge.to,"edge_type":edge.edge_type,"properties":properties_wire(edge.properties)})
}
