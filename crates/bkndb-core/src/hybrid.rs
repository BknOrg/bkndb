use std::collections::HashMap;

use crate::graph::{NodeId, NodeRecord};
use crate::relational::{ColumnKind, Query, Row, TableSchema};
use crate::value::PropValue;
use crate::{BknError, StorageReadTx};

#[derive(Debug, Clone, PartialEq)]
pub struct JoinedNode {
    pub id: NodeId,
    pub node: Option<NodeRecord>,
    pub row: Option<Row>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct JoinedNodeRows {
    pub id: NodeId,
    pub node: Option<NodeRecord>,
    pub rows: Vec<Row>,
}

fn node_id_value(id: NodeId) -> Result<PropValue, BknError> {
    i64::try_from(id.0)
        .map(PropValue::Int)
        .map_err(|_| BknError::Encoding(format!("NodeId {} does not fit in an Int column", id.0)))
}

/// Batched direct join matching `NodeId` to the relational table's primary key (`Row.pk`).
///
/// In the direct PK pattern (`table.insert_with_pk(PropValue::Int(node_id.0 as i64), row)`),
/// this retrieves the graph node metadata and the corresponding relational row in a single pass
/// over the shared snapshot without N+1 storage roundtrips.
pub fn join_nodes_with_table<R: StorageReadTx>(
    rtx: &R,
    nodes: &[NodeId],
    schema: &TableSchema,
) -> Result<Vec<JoinedNode>, BknError> {
    if schema.primary_key_column().kind != ColumnKind::Int {
        return Err(BknError::Encoding(format!(
            "table '{}' primary key '{}' is not Int, cannot join directly with NodeId",
            schema.name(),
            schema.primary_key()
        )));
    }
    let mut out = Vec::with_capacity(nodes.len());
    for &id in nodes {
        let node = crate::graph::db::get_node_in(rtx, id)?;
        let row = crate::relational::db::get_in(rtx, schema, &node_id_value(id)?)?;
        out.push(JoinedNode { id, node, row });
    }
    Ok(out)
}

/// Batched foreign-key join matching `NodeId` to an integer column in a relational table.
///
/// For tables where a column (such as `file_node_id` or `parent_id`) references a `NodeId`,
/// this uses the secondary index on that column if available; otherwise it scans the table
/// once for all requested nodes.
pub fn join_nodes_by_column<R: StorageReadTx>(
    rtx: &R,
    nodes: &[NodeId],
    schema: &TableSchema,
    foreign_key_col: &str,
) -> Result<Vec<JoinedNodeRows>, BknError> {
    let col = schema.column(foreign_key_col).ok_or_else(|| {
        BknError::Encoding(format!("no such column '{foreign_key_col}' in schema '{}'", schema.name()))
    })?;
    if col.kind != ColumnKind::Int {
        return Err(BknError::Encoding(format!(
            "foreign key column '{}' is {:?}, expected Int",
            foreign_key_col, col.kind
        )));
    }

    let mut by_fk: Option<HashMap<i64, Vec<Row>>> = None;
    if !schema.is_indexed(foreign_key_col) {
        let mut groups: HashMap<i64, Vec<Row>> = HashMap::new();
        for row in crate::relational::db::scan_all_in(rtx, schema)? {
            if let Some(PropValue::Int(fk)) = row.get(schema, foreign_key_col) {
                groups.entry(*fk).or_default().push(row.clone());
            }
        }
        by_fk = Some(groups);
    }

    let mut out = Vec::with_capacity(nodes.len());
    for &id in nodes {
        let node = crate::graph::db::get_node_in(rtx, id)?;
        let val = node_id_value(id)?;
        let rows = match (&mut by_fk, &val) {
            (Some(groups), PropValue::Int(fk)) => groups.get(fk).cloned().unwrap_or_default(),
            _ => crate::relational::db::index_lookup_eq_in(rtx, schema, foreign_key_col, &val)?,
        };
        out.push(JoinedNodeRows { id, node, rows });
    }
    Ok(out)
}

/// Batched reverse join: given a slice of relational `Row`s, resolves their associated
/// graph `Node` from a designated column (or the primary key).
pub fn join_rows_with_nodes<R: StorageReadTx>(
    rtx: &R,
    rows: &[Row],
    schema: &TableSchema,
    node_id_column: &str,
) -> Result<Vec<(Row, Option<NodeRecord>)>, BknError> {
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let node = match row.get(schema, node_id_column) {
            Some(PropValue::Int(i)) if *i >= 0 => crate::graph::db::get_node_in(rtx, NodeId(*i as u64))?,
            _ => None,
        };
        out.push((row.clone(), node));
    }
    Ok(out)
}

/// Rows of `schema` matching `query`, each paired with the graph node its
/// `node_id_column` points at — "filter relationally, then hop into the graph".
pub fn query_rows_with_nodes<R: StorageReadTx>(
    rtx: &R,
    schema: &TableSchema,
    query: &Query,
    node_id_column: &str,
) -> Result<Vec<(Row, Option<NodeRecord>)>, BknError> {
    let rows = crate::relational::query::select_in(rtx, schema, query)?;
    join_rows_with_nodes(rtx, &rows, schema, node_id_column)
}
