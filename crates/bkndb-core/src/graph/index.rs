//! Secondary indexes over graph nodes: by label, and by (label, property)
//! value.
//!
//! - `node_labels`: `len16 ++ label ++ node_id(8)` → empty. Lets "all nodes
//!   with label X" be a prefix scan instead of decoding every node.
//! - `node_props`: `len16 ++ label ++ len16 ++ property ++ sortable(value)
//!   ++ node_id(8)` → empty, for each registered (label, property) pair.
//!   Only `Int`/`Str` values are indexable; nodes whose value has another
//!   kind (or is missing) simply have no entry, and lookups by such a value
//!   scan instead. (Properties are untyped, and other kinds' sortable
//!   encodings can coincide with these — e.g. `Timestamp(5)` and `Int(5)`.)
//!
//! Both are maintained by every node write (`create_node*`, `update_node_*`,
//! `delete_node`). A property index is complete from the moment it's
//! created (it's backfilled then). The label index, however, didn't exist
//! in older files: it's only trusted once the `graph:label_index` marker is
//! set — automatically for databases whose first node was created by a
//! version that maintains it, or by [`rebuild_indexes_in`]. Until then,
//! label lookups fall back to a full node scan, so results are always
//! correct, just slower.
use std::ops::Bound;

use crate::graph::codec::{decode_node_id, node_key, META, NEXT_NODE_ID_KEY, NODES};
use crate::graph::model::{NodeId, NodeRecord};
use crate::value::{sortable_key, PropValue};

/// Index key bytes for a property value, for the kinds this index holds.
fn index_value_key(value: &PropValue) -> Option<Vec<u8>> {
    match value {
        PropValue::Int(_) | PropValue::Str(_) => sortable_key(value),
        _ => None,
    }
}
use crate::{BknError, StorageReadTx, StorageWriteTx, TableSpec};

pub const NODE_LABELS: TableSpec = TableSpec("node_labels");
pub const NODE_PROPS: TableSpec = TableSpec("node_props");

const LABEL_INDEX_MARKER: &[u8] = b"graph:label_index";
const PROP_INDEX_PREFIX: &[u8] = b"graph:prop_index:";

fn push_len_prefixed(out: &mut Vec<u8>, s: &str) -> Result<(), BknError> {
    let len = u16::try_from(s.len()).map_err(|_| BknError::Encoding(format!("name too long for an index key ({} bytes)", s.len())))?;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(s.as_bytes());
    Ok(())
}

fn label_prefix(label: &str) -> Result<Vec<u8>, BknError> {
    let mut k = Vec::with_capacity(2 + label.len() + 8);
    push_len_prefixed(&mut k, label)?;
    Ok(k)
}

fn label_key(label: &str, id: NodeId) -> Result<Vec<u8>, BknError> {
    let mut k = label_prefix(label)?;
    k.extend_from_slice(&node_key(id));
    Ok(k)
}

fn prop_prefix(label: &str, property: &str) -> Result<Vec<u8>, BknError> {
    let mut k = label_prefix(label)?;
    push_len_prefixed(&mut k, property)?;
    Ok(k)
}

fn registry_key(label: &str, property: &str) -> Result<Vec<u8>, BknError> {
    let mut k = PROP_INDEX_PREFIX.to_vec();
    k.extend(prop_prefix(label, property)?);
    Ok(k)
}

fn prefix_end(prefix: &[u8]) -> Option<Vec<u8>> {
    let mut out = prefix.to_vec();
    while let Some(last) = out.pop() {
        if last != 0xFF {
            out.push(last + 1);
            return Some(out);
        }
    }
    None
}

fn scan_prefix<R: StorageReadTx>(rtx: &R, table: TableSpec, prefix: &[u8]) -> Result<Vec<Vec<u8>>, BknError> {
    let end = prefix_end(prefix);
    let upper = match &end {
        Some(e) => Bound::Excluded(e.as_slice()),
        None => Bound::Unbounded,
    };
    Ok(rtx.range(table, Bound::Included(prefix), upper)?.into_iter().map(|(k, _)| k).collect())
}

fn trailing_node_id(key: &[u8]) -> Result<NodeId, BknError> {
    let split = key
        .len()
        .checked_sub(8)
        .ok_or_else(|| BknError::Encoding("corrupt graph index key".to_string()))?;
    decode_node_id(&key[split..])
}

fn decode<T: for<'de> serde::Deserialize<'de>>(bytes: &[u8]) -> Result<T, BknError> {
    bincode::deserialize(bytes).map_err(|e| BknError::Encoding(e.to_string()))
}

/// Every registered indexed property of nodes with `label`.
fn indexed_properties<R: StorageReadTx>(rtx: &R, label: &str) -> Result<Vec<String>, BknError> {
    let mut prefix = PROP_INDEX_PREFIX.to_vec();
    prefix.extend(label_prefix(label)?);
    scan_prefix(rtx, META, &prefix)?
        .into_iter()
        .map(|k| {
            let rest = &k[prefix.len()..];
            let len = u16::from_be_bytes(
                rest.get(..2)
                    .and_then(|b| b.try_into().ok())
                    .ok_or_else(|| BknError::Encoding("corrupt property index registry key".to_string()))?,
            ) as usize;
            let name = rest
                .get(2..2 + len)
                .ok_or_else(|| BknError::Encoding("corrupt property index registry key".to_string()))?;
            String::from_utf8(name.to_vec()).map_err(|e| BknError::Encoding(e.to_string()))
        })
        .collect()
}

fn prop_entry(label: &str, property: &str, value: Option<&PropValue>, id: NodeId) -> Result<Option<Vec<u8>>, BknError> {
    let Some(encoded) = value.and_then(index_value_key) else {
        return Ok(None);
    };
    let mut k = prop_prefix(label, property)?;
    k.extend(encoded);
    k.extend_from_slice(&node_key(id));
    Ok(Some(k))
}

// ---- maintenance hooks, called by every node write ----

/// Called before allocating ids for new nodes: a database that has never had
/// a node gets a complete label index from the start.
pub(crate) fn before_first_nodes<W: StorageWriteTx>(wtx: &mut W) -> Result<(), BknError> {
    if wtx.get(META, NEXT_NODE_ID_KEY)?.is_none() && wtx.get(META, LABEL_INDEX_MARKER)?.is_none() {
        wtx.put(META, LABEL_INDEX_MARKER, &[1])?;
    }
    Ok(())
}

pub(crate) fn on_insert<W: StorageWriteTx>(wtx: &mut W, id: NodeId, record: &NodeRecord) -> Result<(), BknError> {
    wtx.put(NODE_LABELS, &label_key(&record.label, id)?, &[])?;
    for prop in indexed_properties(wtx, &record.label)? {
        if let Some(k) = prop_entry(&record.label, &prop, record.properties.get(&prop), id)? {
            wtx.put(NODE_PROPS, &k, &[])?;
        }
    }
    Ok(())
}

pub(crate) fn on_delete<W: StorageWriteTx>(wtx: &mut W, id: NodeId, record: &NodeRecord) -> Result<(), BknError> {
    wtx.delete(NODE_LABELS, &label_key(&record.label, id)?)?;
    for prop in indexed_properties(wtx, &record.label)? {
        if let Some(k) = prop_entry(&record.label, &prop, record.properties.get(&prop), id)? {
            wtx.delete(NODE_PROPS, &k)?;
        }
    }
    Ok(())
}

pub(crate) fn on_update<W: StorageWriteTx>(wtx: &mut W, id: NodeId, old: &NodeRecord, new: &NodeRecord) -> Result<(), BknError> {
    for prop in indexed_properties(wtx, &new.label)? {
        let (before, after) = (old.properties.get(&prop), new.properties.get(&prop));
        if before != after {
            if let Some(k) = prop_entry(&old.label, &prop, before, id)? {
                wtx.delete(NODE_PROPS, &k)?;
            }
            if let Some(k) = prop_entry(&new.label, &prop, after, id)? {
                wtx.put(NODE_PROPS, &k, &[])?;
            }
        }
    }
    Ok(())
}

// ---- queries ----

fn label_index_ready<R: StorageReadTx>(rtx: &R) -> Result<bool, BknError> {
    Ok(rtx.get(META, LABEL_INDEX_MARKER)?.is_some())
}

pub(crate) fn scan_all_nodes<R: StorageReadTx>(rtx: &R) -> Result<Vec<(NodeId, NodeRecord)>, BknError> {
    rtx.range(NODES, Bound::Unbounded, Bound::Unbounded)?
        .into_iter()
        .map(|(k, v)| Ok((decode_node_id(&k)?, decode(&v)?)))
        .collect()
}

/// Ids of every node with `label`, ascending.
pub(crate) fn nodes_by_label_in<R: StorageReadTx>(rtx: &R, label: &str) -> Result<Vec<NodeId>, BknError> {
    if label_index_ready(rtx)? {
        return scan_prefix(rtx, NODE_LABELS, &label_prefix(label)?)?
            .iter()
            .map(|k| trailing_node_id(k))
            .collect();
    }
    Ok(scan_all_nodes(rtx)?
        .into_iter()
        .filter(|(_, r)| r.label == label)
        .map(|(id, _)| id)
        .collect())
}

/// Ids of nodes with `label` whose `property` equals `value`, ascending.
/// Uses the property index when one exists and `value` is indexable.
pub(crate) fn find_nodes_in<R: StorageReadTx>(
    rtx: &R,
    label: &str,
    property: &str,
    value: &PropValue,
) -> Result<Vec<NodeId>, BknError> {
    if let Some(encoded) = index_value_key(value)
        && rtx.get(META, &registry_key(label, property)?)?.is_some()
    {
        let mut prefix = prop_prefix(label, property)?;
        prefix.extend(encoded);
        return scan_prefix(rtx, NODE_PROPS, &prefix)?
            .iter()
            .map(|k| trailing_node_id(k))
            .collect();
    }
    let mut out = Vec::new();
    for id in nodes_by_label_in(rtx, label)? {
        if let Some(bytes) = rtx.get(NODES, &node_key(id))? {
            let record: NodeRecord = decode(&bytes)?;
            if record.properties.get(property) == Some(value) {
                out.push(id);
            }
        }
    }
    Ok(out)
}

/// Every registered property index as `(label, property)`.
pub(crate) fn property_indexes_in<R: StorageReadTx>(rtx: &R) -> Result<Vec<(String, String)>, BknError> {
    let mut out = Vec::new();
    for k in scan_prefix(rtx, META, PROP_INDEX_PREFIX)? {
        let mut rest = &k[PROP_INDEX_PREFIX.len()..];
        let mut parts = Vec::with_capacity(2);
        for _ in 0..2 {
            let len = rest
                .get(..2)
                .map(|b| u16::from_be_bytes([b[0], b[1]]) as usize)
                .ok_or_else(|| BknError::Encoding("corrupt property index registry key".to_string()))?;
            let bytes = rest
                .get(2..2 + len)
                .ok_or_else(|| BknError::Encoding("corrupt property index registry key".to_string()))?;
            parts.push(String::from_utf8(bytes.to_vec()).map_err(|e| BknError::Encoding(e.to_string()))?);
            rest = &rest[2 + len..];
        }
        let property = parts.pop().unwrap_or_default();
        let label = parts.pop().unwrap_or_default();
        out.push((label, property));
    }
    Ok(out)
}

// ---- index management ----

fn clear_prefix<W: StorageWriteTx>(wtx: &mut W, table: TableSpec, prefix: &[u8]) -> Result<(), BknError> {
    for k in scan_prefix(wtx, table, prefix)? {
        wtx.delete(table, &k)?;
    }
    Ok(())
}

fn backfill_property<W: StorageWriteTx>(wtx: &mut W, label: &str, property: &str) -> Result<(), BknError> {
    clear_prefix(wtx, NODE_PROPS, &prop_prefix(label, property)?)?;
    for id in nodes_by_label_in(wtx, label)? {
        if let Some(bytes) = wtx.get(NODES, &node_key(id))? {
            let record: NodeRecord = decode(&bytes)?;
            if let Some(k) = prop_entry(label, property, record.properties.get(property), id)? {
                wtx.put(NODE_PROPS, &k, &[])?;
            }
        }
    }
    Ok(())
}

/// Registers and backfills an index on `property` of nodes with `label`.
/// Returns `false` if it already existed.
pub(crate) fn create_property_index_in<W: StorageWriteTx>(wtx: &mut W, label: &str, property: &str) -> Result<bool, BknError> {
    let reg = registry_key(label, property)?;
    if wtx.get(META, &reg)?.is_some() {
        return Ok(false);
    }
    wtx.put(META, &reg, &[])?;
    backfill_property(wtx, label, property)?;
    Ok(true)
}

/// Removes a property index and its entries. Returns `false` if it didn't exist.
pub(crate) fn drop_property_index_in<W: StorageWriteTx>(wtx: &mut W, label: &str, property: &str) -> Result<bool, BknError> {
    let reg = registry_key(label, property)?;
    if wtx.get(META, &reg)?.is_none() {
        return Ok(false);
    }
    wtx.delete(META, &reg)?;
    clear_prefix(wtx, NODE_PROPS, &prop_prefix(label, property)?)?;
    Ok(true)
}

/// Rebuilds the label index and every property index from the node table,
/// and marks the label index complete. Needed once for databases created
/// before graph indexes existed (lookups work without it, via full scans).
pub(crate) fn rebuild_indexes_in<W: StorageWriteTx>(wtx: &mut W) -> Result<(), BknError> {
    let nodes = scan_all_nodes(wtx)?;
    clear_prefix(wtx, NODE_LABELS, &[])?;
    for (id, record) in &nodes {
        wtx.put(NODE_LABELS, &label_key(&record.label, *id)?, &[])?;
    }
    wtx.put(META, LABEL_INDEX_MARKER, &[1])?;
    for (label, property) in property_indexes_in(wtx)? {
        backfill_property(wtx, &label, &property)?;
    }
    Ok(())
}
