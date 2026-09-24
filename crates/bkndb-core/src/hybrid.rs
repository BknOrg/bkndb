use crate::graph::{NodeId, NodeRecord};
use crate::relational::{RelSchema, Row};
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

/// Batched direct join matching `NodeId` to the relational table's primary key (`Row.pk`).
///
/// In the direct PK pattern (`table.insert_with_pk(PropValue::Int(node_id.0 as i64), row)`),
/// this retrieves the graph node metadata and the corresponding relational row in a single pass
/// over the shared snapshot without N+1 storage roundtrips.
pub fn join_nodes_with_table<R: StorageReadTx>(
    rtx: &R,
    nodes: &[NodeId],
    schema: &RelSchema,
) -> Result<Vec<JoinedNode>, BknError> {
    if schema.primary_key_column().kind != crate::relational::ColumnKind::Int {
        return Err(BknError::Encoding(format!(
            "table '{}' primary key '{}' is not Int, cannot join directly with NodeId",
            schema.name, schema.primary_key
        )));
    }
    let mut out = Vec::with_capacity(nodes.len());
    for &id in nodes {
        let node = crate::graph::db::get_node_in(rtx, id)?;
        let pk = PropValue::Int(id.0 as i64);
        let row = crate::relational::db::get_in(rtx, schema, &pk)?;
        out.push(JoinedNode { id, node, row });
    }
    Ok(out)
}

/// Batched foreign-key join matching `NodeId` to an integer column in a relational table.
///
/// For tables where a column (such as `file_node_id` or `parent_id`) references a `NodeId`,
/// this uses the secondary index on that column if available (or falls back to a scan)
/// to fetch all associated rows for each node.
pub fn join_nodes_by_column<R: StorageReadTx>(
    rtx: &R,
    nodes: &[NodeId],
    schema: &RelSchema,
    foreign_key_col: &str,
) -> Result<Vec<JoinedNodeRows>, BknError> {
    let col = schema
        .column(foreign_key_col)
        .ok_or_else(|| BknError::Encoding(format!("no such column '{foreign_key_col}' in schema '{}'", schema.name)))?;
    if col.kind != crate::relational::ColumnKind::Int {
        return Err(BknError::Encoding(format!(
            "foreign key column '{}' is {:?}, expected Int",
            foreign_key_col, col.kind
        )));
    }
    let mut out = Vec::with_capacity(nodes.len());
    for &id in nodes {
        let node = crate::graph::db::get_node_in(rtx, id)?;
        let val = PropValue::Int(id.0 as i64);
        let rows = if schema.is_indexed(foreign_key_col) {
            crate::relational::db::index_lookup_eq_in(rtx, schema, foreign_key_col, &val)?
        } else {
            let all = crate::relational::db::scan_all_in(rtx, schema)?;
            all.into_iter()
                .filter(|r| r.get(schema, foreign_key_col) == Some(&val))
                .collect()
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
    schema: &RelSchema,
    node_id_column: &str,
) -> Result<Vec<(Row, Option<NodeRecord>)>, BknError> {
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let node_id_val = row.get(schema, node_id_column);
        let node = match node_id_val {
            Some(PropValue::Int(i)) if *i >= 0 => {
                crate::graph::db::get_node_in(rtx, NodeId(*i as u64))?
            }
            _ => None,
        };
        out.push((row.clone(), node));
    }
    Ok(out)
}
