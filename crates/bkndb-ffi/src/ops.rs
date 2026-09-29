//! Backend-generic implementations of every FFI operation, written against
//! an open batch. The engine runs them in a one-shot transaction per call;
//! `BknDbTransaction` runs them inside its long-lived transaction.
use std::collections::HashMap;

use bkndb_core::graph::{Direction, EdgeId, NodeId, Properties};
use bkndb_core::relational::{Agg, Query, TableSchema};
use bkndb_core::value::PropValue;
use bkndb_core::{BknError, DbReadBatch, DbWriteBatch, StorageReadTx, StorageWriteTx};

use crate::relational::FfiRow;
use crate::types::{
    core_props_to_ffi, ffi_props_to_core, FfiEdgeRecord, FfiNodeRecord, FfiNodeRef, FfiPropValue, FfiPropertyIndex,
    FfiSyncBatch, FfiSyncResult, FfiTraversalHit, FfiTypedNeighbor, FfiWeightedPath,
};

pub(crate) type FfiProps = HashMap<String, FfiPropValue>;

fn apply_changes(p: &mut Properties, set: FfiProps, unset: Vec<String>) {
    for k in unset {
        p.remove(&k);
    }
    for (k, v) in set {
        p.insert(k, v.into());
    }
}

pub(crate) fn node_record(id: u64, n: bkndb_core::graph::NodeRecord) -> FfiNodeRecord {
    FfiNodeRecord { id, label: n.label, properties: core_props_to_ffi(n.properties) }
}

pub(crate) fn edge_record(id: u64, e: bkndb_core::graph::EdgeRecord) -> FfiEdgeRecord {
    FfiEdgeRecord { id, from: e.from.0, to: e.to.0, edge_type: e.edge_type, properties: core_props_to_ffi(e.properties) }
}

/// Neighbors of `node` in `direction`, optionally restricted to one edge
/// type, as `(edge_type, neighbor, edge)`. Works on both graph view kinds.
macro_rules! neighbors_of {
    ($g:expr, $node:expr, $direction:expr, $edge_type:expr) => {{
        let node = $node;
        let edge_type: Option<&str> = $edge_type;
        let mut result: Vec<(String, NodeId, EdgeId)> = Vec::new();
        if matches!($direction, Direction::Out | Direction::Both) {
            match edge_type {
                Some(t) => result.extend($g.neighbors_out(node, t)?.into_iter().map(|(n, e)| (t.to_string(), n, e))),
                None => result.extend($g.neighbors_out_any(node)?),
            }
        }
        if matches!($direction, Direction::In | Direction::Both) {
            match edge_type {
                Some(t) => result.extend($g.neighbors_in(node, t)?.into_iter().map(|(n, e)| (t.to_string(), n, e))),
                None => result.extend($g.neighbors_in_any(node)?),
            }
        }
        result
    }};
}

fn typed_neighbors(v: Vec<(String, NodeId, EdgeId)>) -> Vec<FfiTypedNeighbor> {
    v.into_iter()
        .map(|(t, n, e)| FfiTypedNeighbor { node_id: n.0, edge_id: e.0, edge_type: t })
        .collect()
}

fn update_sets(sets: FfiProps) -> Vec<(String, PropValue)> {
    sets.into_iter().map(|(k, v)| (k, v.into())).collect()
}

// ---- Graph writes ----

pub(crate) fn create_node<W: StorageWriteTx>(b: &mut DbWriteBatch<W>, label: &str, props: FfiProps) -> Result<u64, BknError> {
    Ok(b.graph().create_node(label, ffi_props_to_core(props))?.0)
}

pub(crate) fn create_nodes<W: StorageWriteTx>(b: &mut DbWriteBatch<W>, nodes: Vec<(String, FfiProps)>) -> Result<Vec<u64>, BknError> {
    let items = nodes.into_iter().map(|(l, p)| (l, ffi_props_to_core(p)));
    Ok(b.graph().create_nodes_bulk(items)?.into_iter().map(|n| n.0).collect())
}

pub(crate) fn create_edge<W: StorageWriteTx>(
    b: &mut DbWriteBatch<W>,
    from: u64,
    edge_type: &str,
    to: u64,
    props: FfiProps,
) -> Result<u64, BknError> {
    Ok(b.graph().create_edge(NodeId(from), edge_type, NodeId(to), ffi_props_to_core(props))?.0)
}

pub(crate) fn create_edges<W: StorageWriteTx>(
    b: &mut DbWriteBatch<W>,
    edges: Vec<(u64, String, u64, FfiProps)>,
) -> Result<Vec<u64>, BknError> {
    let items = edges.into_iter().map(|(f, t, to, p)| (NodeId(f), t, NodeId(to), ffi_props_to_core(p)));
    Ok(b.graph().create_edges_bulk(items)?.into_iter().map(|e| e.0).collect())
}

pub(crate) fn delete_node<W: StorageWriteTx>(b: &mut DbWriteBatch<W>, id: u64) -> Result<(), BknError> {
    b.graph().delete_node(NodeId(id))
}

pub(crate) fn delete_edge<W: StorageWriteTx>(b: &mut DbWriteBatch<W>, id: u64) -> Result<bool, BknError> {
    b.graph().delete_edge(EdgeId(id))
}

pub(crate) fn update_node<W: StorageWriteTx>(b: &mut DbWriteBatch<W>, id: u64, set: FfiProps, unset: Vec<String>) -> Result<(), BknError> {
    b.graph().update_node_properties(NodeId(id), |p| apply_changes(p, set, unset))
}

pub(crate) fn update_edge<W: StorageWriteTx>(b: &mut DbWriteBatch<W>, id: u64, set: FfiProps, unset: Vec<String>) -> Result<(), BknError> {
    b.graph().update_edge_properties(EdgeId(id), |p| apply_changes(p, set, unset))
}

pub(crate) fn cascade_delete<W: StorageWriteTx>(b: &mut DbWriteBatch<W>, root: u64, edge_type: &str) -> Result<Vec<u64>, BknError> {
    Ok(b.graph().cascade_delete(NodeId(root), edge_type)?.into_iter().map(|n| n.0).collect())
}

// ---- Graph reads inside a write transaction (see its own writes) ----

pub(crate) fn tx_get_node<W: StorageWriteTx>(b: &mut DbWriteBatch<W>, id: u64) -> Result<Option<FfiNodeRecord>, BknError> {
    Ok(b.graph().get_node(NodeId(id))?.map(|n| node_record(id, n)))
}

pub(crate) fn tx_get_edge<W: StorageWriteTx>(b: &mut DbWriteBatch<W>, id: u64) -> Result<Option<FfiEdgeRecord>, BknError> {
    Ok(b.graph().get_edge(EdgeId(id))?.map(|e| edge_record(id, e)))
}

pub(crate) fn tx_neighbors<W: StorageWriteTx>(
    b: &mut DbWriteBatch<W>,
    node: u64,
    direction: Direction,
    edge_type: Option<&str>,
) -> Result<Vec<FfiTypedNeighbor>, BknError> {
    let g = b.graph();
    Ok(typed_neighbors(neighbors_of!(g, NodeId(node), direction, edge_type)))
}

// ---- Graph indexes ----

fn ids(v: Vec<NodeId>) -> Vec<u64> {
    v.into_iter().map(|n| n.0).collect()
}

pub(crate) fn tx_nodes_by_label<W: StorageWriteTx>(b: &mut DbWriteBatch<W>, label: &str) -> Result<Vec<u64>, BknError> {
    Ok(ids(b.graph().nodes_by_label(label)?))
}

pub(crate) fn tx_find_nodes<W: StorageWriteTx>(b: &mut DbWriteBatch<W>, label: &str, property: &str, value: PropValue) -> Result<Vec<u64>, BknError> {
    Ok(ids(b.graph().find_nodes(label, property, &value)?))
}

pub(crate) fn set_node_index<W: StorageWriteTx>(b: &mut DbWriteBatch<W>, label: &str, property: &str, create: bool) -> Result<bool, BknError> {
    let mut g = b.graph();
    if create { g.create_property_index(label, property) } else { g.drop_property_index(label, property) }
}

pub(crate) fn rebuild_graph_indexes<W: StorageWriteTx>(b: &mut DbWriteBatch<W>) -> Result<(), BknError> {
    b.graph().rebuild_indexes()
}

pub(crate) fn nodes_by_label<R: StorageReadTx>(r: &DbReadBatch<R>, label: &str) -> Result<Vec<u64>, BknError> {
    Ok(ids(r.graph().nodes_by_label(label)?))
}

pub(crate) fn find_nodes<R: StorageReadTx>(r: &DbReadBatch<R>, label: &str, property: &str, value: PropValue) -> Result<Vec<u64>, BknError> {
    Ok(ids(r.graph().find_nodes(label, property, &value)?))
}

pub(crate) fn node_indexes<R: StorageReadTx>(r: &DbReadBatch<R>) -> Result<Vec<FfiPropertyIndex>, BknError> {
    Ok(r.graph().property_indexes()?.into_iter().map(|(label, property)| FfiPropertyIndex { label, property }).collect())
}

pub(crate) fn weighted_path<R: StorageReadTx>(
    r: &DbReadBatch<R>,
    start: u64,
    target: u64,
    direction: Direction,
    edge_types: Option<Vec<String>>,
    weight_property: &str,
    default_weight: f64,
) -> Result<Option<FfiWeightedPath>, BknError> {
    let refs: Option<Vec<&str>> = edge_types.as_ref().map(|v| v.iter().map(String::as_str).collect());
    let found = r.graph().find_weighted_path(NodeId(start), NodeId(target), direction, refs.as_deref(), weight_property, default_weight)?;
    Ok(found.map(|w| FfiWeightedPath { path: w.path.into(), cost: w.cost }))
}

// ---- Graph reads on a snapshot ----

pub(crate) fn neighbors<R: StorageReadTx>(
    r: &DbReadBatch<R>,
    node: u64,
    direction: Direction,
    edge_type: Option<&str>,
) -> Result<Vec<FfiTypedNeighbor>, BknError> {
    let g = r.graph();
    Ok(typed_neighbors(neighbors_of!(g, NodeId(node), direction, edge_type)))
}

pub(crate) fn traverse<R: StorageReadTx>(
    r: &DbReadBatch<R>,
    start: u64,
    direction: Direction,
    max_depth: u32,
    edge_types: Option<Vec<String>>,
    node_label: Option<String>,
) -> Result<Vec<FfiTraversalHit>, BknError> {
    let g = r.graph();
    let mut t = g.traversal().start(NodeId(start)).direction(direction).max_depth(max_depth);
    if let Some(types) = &edge_types {
        let refs: Vec<&str> = types.iter().map(String::as_str).collect();
        t = t.filter_edge_types(&refs);
    }
    if let Some(label) = node_label {
        t = t.filter_node_label(label);
    }
    Ok(t.run()?
        .into_iter()
        .map(|n| FfiTraversalHit {
            node_id: n.node.0,
            depth: n.depth,
            via_edge_id: n.via_edge.map(|e| e.0),
            parent_id: n.parent.map(|p| p.0),
        })
        .collect())
}

// ---- Relational writes ----

pub(crate) fn create_table<W: StorageWriteTx>(b: &mut DbWriteBatch<W>, schema: TableSchema) -> Result<bool, BknError> {
    b.relational().create_table(schema)
}

pub(crate) fn ensure_table<W: StorageWriteTx>(b: &mut DbWriteBatch<W>, schema: TableSchema) -> Result<(), BknError> {
    b.relational().ensure_table(schema)
}

pub(crate) fn drop_table<W: StorageWriteTx>(b: &mut DbWriteBatch<W>, name: &str) -> Result<bool, BknError> {
    b.relational().drop_table(name)
}

pub(crate) fn set_index<W: StorageWriteTx>(b: &mut DbWriteBatch<W>, table: &str, column: &str, indexed: bool) -> Result<(), BknError> {
    let mut rel = b.relational();
    let current = rel.table_schema(table)?.ok_or_else(|| BknError::TableNotFound(table.to_string()))?;
    let builder = current.to_builder();
    let schema = if indexed { builder.index(column) } else { builder.drop_index(column) }.build()?;
    rel.ensure_table(schema)
}

pub(crate) fn insert<W: StorageWriteTx>(b: &mut DbWriteBatch<W>, table: &str, values: FfiProps, upsert: bool) -> Result<PropValue, BknError> {
    let mut rel = b.relational();
    let mut t = rel.table_named(table)?;
    if upsert {
        t.upsert(ffi_props_to_core(values))
    } else {
        t.insert(ffi_props_to_core(values))
    }
}

pub(crate) fn insert_many<W: StorageWriteTx>(
    b: &mut DbWriteBatch<W>,
    table: &str,
    rows: Vec<FfiProps>,
    upsert: bool,
) -> Result<Vec<PropValue>, BknError> {
    let mut rel = b.relational();
    let mut t = rel.table_named(table)?;
    let rows = rows.into_iter().map(ffi_props_to_core);
    if upsert { t.upsert_bulk(rows) } else { t.insert_bulk(rows) }
}

pub(crate) fn update_rows<W: StorageWriteTx>(b: &mut DbWriteBatch<W>, table: &str, query: &Query, set: FfiProps) -> Result<u64, BknError> {
    let sets = update_sets(set);
    let sets: Vec<(&str, PropValue)> = sets.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
    let mut rel = b.relational();
    Ok(rel.table_named(table)?.update_where(query, &sets)? as u64)
}

pub(crate) fn delete_rows<W: StorageWriteTx>(b: &mut DbWriteBatch<W>, table: &str, query: &Query) -> Result<u64, BknError> {
    let mut rel = b.relational();
    Ok(rel.table_named(table)?.delete_where(query)? as u64)
}

/// Graph nodes/edges plus relational rows (upserted) in one transaction.
pub(crate) fn sync<W: StorageWriteTx>(b: &mut DbWriteBatch<W>, batch: FfiSyncBatch) -> Result<FfiSyncResult, BknError> {
    let node_ids = create_nodes(b, batch.nodes.into_iter().map(|n| (n.label, n.properties)).collect())?;
    let resolve = |r: FfiNodeRef| -> Result<u64, BknError> {
        match r {
            FfiNodeRef::Existing { id } => Ok(id),
            FfiNodeRef::New { index } => node_ids.get(index as usize).copied().ok_or_else(|| {
                BknError::Encoding(format!("linked edge refers to new node {index}, but the batch creates {}", node_ids.len()))
            }),
        }
    };
    let mut edges: Vec<(u64, String, u64, FfiProps)> =
        batch.edges.into_iter().map(|e| (e.from, e.edge_type, e.to, e.properties)).collect();
    for e in batch.linked_edges {
        edges.push((resolve(e.from)?, e.edge_type, resolve(e.to)?, e.properties));
    }
    let edge_ids = create_edges(b, edges)?;
    let mut row_pks = Vec::with_capacity(batch.rows.len());
    for t in batch.rows {
        let pks = insert_many(b, &t.table, t.rows, true)?;
        row_pks.push(pks.into_iter().map(Into::into).collect());
    }
    Ok(FfiSyncResult { node_ids, edge_ids, row_pks })
}

// ---- Relational reads (inside a write transaction, or on a snapshot) ----

pub(crate) fn tx_get_row<W: StorageWriteTx>(b: &mut DbWriteBatch<W>, table: &str, pk: PropValue) -> Result<Option<FfiRow>, BknError> {
    let mut rel = b.relational();
    Ok(rel.table_named(table)?.get(&pk)?.map(Into::into))
}

pub(crate) fn tx_select<W: StorageWriteTx>(b: &mut DbWriteBatch<W>, table: &str, query: &Query) -> Result<Vec<FfiRow>, BknError> {
    let mut rel = b.relational();
    Ok(rel.table_named(table)?.find(query)?.into_iter().map(Into::into).collect())
}

pub(crate) fn tx_count<W: StorageWriteTx>(b: &mut DbWriteBatch<W>, table: &str, query: &Query) -> Result<u64, BknError> {
    let mut rel = b.relational();
    Ok(rel.table_named(table)?.count(query)? as u64)
}

pub(crate) fn get_row<R: StorageReadTx>(r: &DbReadBatch<R>, table: &str, pk: PropValue) -> Result<Option<FfiRow>, BknError> {
    Ok(r.relational().table_named(table)?.get(&pk)?.map(Into::into))
}

pub(crate) fn select<R: StorageReadTx>(r: &DbReadBatch<R>, table: &str, query: &Query) -> Result<Vec<FfiRow>, BknError> {
    Ok(r.relational().table_named(table)?.find(query)?.into_iter().map(Into::into).collect())
}

pub(crate) fn count<R: StorageReadTx>(r: &DbReadBatch<R>, table: &str, query: &Query) -> Result<u64, BknError> {
    Ok(r.relational().table_named(table)?.count(query)? as u64)
}

pub(crate) fn aggregate<R: StorageReadTx>(
    r: &DbReadBatch<R>,
    table: &str,
    query: &Query,
    group_by: &[String],
    aggs: &[Agg],
) -> Result<Vec<crate::relational::FfiAggregateRow>, BknError> {
    let group_by: Vec<&str> = group_by.iter().map(String::as_str).collect();
    Ok(r.relational().table_named(table)?.aggregate(query, &group_by, aggs)?.into_iter().map(Into::into).collect())
}

pub(crate) fn list_tables<R: StorageReadTx>(r: &DbReadBatch<R>) -> Result<Vec<TableSchema>, BknError> {
    r.relational().list_tables()
}

pub(crate) fn table_schema<R: StorageReadTx>(r: &DbReadBatch<R>, name: &str) -> Result<Option<TableSchema>, BknError> {
    r.relational().table_schema(name)
}
