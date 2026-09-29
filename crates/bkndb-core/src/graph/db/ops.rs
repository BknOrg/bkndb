//! Graph operations over an open transaction (the `*_in` functions).
use super::*;

pub(crate) fn create_node_in<W: StorageWriteTx>(
    wtx: &mut W,
    label: &str,
    properties: Properties,
) -> Result<NodeId, BknError> {
    index::before_first_nodes(wtx)?;
    let id = NodeId(next_id(wtx, NEXT_NODE_ID_KEY)?);
    let record = NodeRecord {
        label: label.to_string(),
        properties,
    };
    wtx.put(NODES, &node_key(id), &encode(&record)?)?;
    index::on_insert(wtx, id, &record)?;
    Ok(id)
}

pub(crate) fn get_node_in<R: StorageReadTx>(
    rtx: &R,
    id: NodeId,
) -> Result<Option<NodeRecord>, BknError> {
    match rtx.get(NODES, &node_key(id))? {
        Some(bytes) => Ok(Some(decode(&bytes)?)),
        None => Ok(None),
    }
}

pub(crate) fn create_edge_in<W: StorageWriteTx>(
    wtx: &mut W,
    from: NodeId,
    edge_type: &str,
    to: NodeId,
    properties: Properties,
) -> Result<EdgeId, BknError> {
    if wtx.get(NODES, &node_key(from))?.is_none() {
        return Err(BknError::NotFound);
    }
    if wtx.get(NODES, &node_key(to))?.is_none() {
        return Err(BknError::NotFound);
    }
    let id = EdgeId(next_id(wtx, NEXT_EDGE_ID_KEY)?);
    let record = EdgeRecord {
        from,
        to,
        edge_type: edge_type.to_string(),
        properties,
    };
    wtx.put(EDGES, &edge_key(id), &encode(&record)?)?;
    wtx.put(ADJ_OUT, &adj_out_key(from, edge_type, to, id), &[])?;
    wtx.put(ADJ_IN, &adj_in_key(to, edge_type, from, id), &[])?;
    Ok(id)
}

pub(crate) fn create_nodes_bulk_in<W: StorageWriteTx>(
    wtx: &mut W,
    nodes: impl IntoIterator<Item = (impl Into<String>, Properties)>,
) -> Result<Vec<NodeId>, BknError> {
    let items: Vec<(String, Properties)> = nodes
        .into_iter()
        .map(|(label, props)| (label.into(), props))
        .collect();
    if items.is_empty() {
        return Ok(Vec::new());
    }
    index::before_first_nodes(wtx)?;
    let start_id = reserve_ids(wtx, NEXT_NODE_ID_KEY, items.len() as u64)?;
    let mut ids = Vec::with_capacity(items.len());
    for (i, (label, properties)) in items.into_iter().enumerate() {
        let id = NodeId(start_id + i as u64);
        let record = NodeRecord { label, properties };
        wtx.put(NODES, &node_key(id), &encode(&record)?)?;
        index::on_insert(wtx, id, &record)?;
        ids.push(id);
    }
    Ok(ids)
}

pub(crate) fn create_edges_bulk_in<W: StorageWriteTx>(
    wtx: &mut W,
    edges: impl IntoIterator<Item = (NodeId, impl Into<String>, NodeId, Properties)>,
) -> Result<Vec<EdgeId>, BknError> {
    let items: Vec<(NodeId, String, NodeId, Properties)> = edges
        .into_iter()
        .map(|(from, edge_type, to, props)| (from, edge_type.into(), to, props))
        .collect();
    if items.is_empty() {
        return Ok(Vec::new());
    }
    let start_id = reserve_ids(wtx, NEXT_EDGE_ID_KEY, items.len() as u64)?;
    let mut ids = Vec::with_capacity(items.len());
    let mut verified = std::collections::HashSet::new();
    for (i, (from, edge_type, to, properties)) in items.into_iter().enumerate() {
        if !verified.contains(&from) {
            if wtx.get(NODES, &node_key(from))?.is_none() {
                return Err(BknError::NotFound);
            }
            verified.insert(from);
        }
        if !verified.contains(&to) {
            if wtx.get(NODES, &node_key(to))?.is_none() {
                return Err(BknError::NotFound);
            }
            verified.insert(to);
        }
        let id = EdgeId(start_id + i as u64);
        let record = EdgeRecord {
            from,
            to,
            edge_type: edge_type.clone(),
            properties,
        };
        wtx.put(EDGES, &edge_key(id), &encode(&record)?)?;
        wtx.put(ADJ_OUT, &adj_out_key(from, &edge_type, to, id), &[])?;
        wtx.put(ADJ_IN, &adj_in_key(to, &edge_type, from, id), &[])?;
        ids.push(id);
    }
    Ok(ids)
}

pub(crate) fn get_edge_in<R: StorageReadTx>(
    rtx: &R,
    id: EdgeId,
) -> Result<Option<EdgeRecord>, BknError> {
    match rtx.get(EDGES, &edge_key(id))? {
        Some(bytes) => Ok(Some(decode(&bytes)?)),
        None => Ok(None),
    }
}

/// A self-loop edge (from == to == id) is reached by both the outgoing and
/// incoming scans below and has its delete attempted twice; this is safe
/// because `StorageWriteTx::delete` is a documented no-op on a missing key
/// on every backend.
pub(crate) fn delete_node_in<W: StorageWriteTx>(wtx: &mut W, id: NodeId) -> Result<(), BknError> {
    let start = node_key(id);
    let end = next_node_prefix(id);

    let out_rows = match end {
        Some(end) => wtx.range(ADJ_OUT, Bound::Included(&start), Bound::Excluded(&end))?,
        None => wtx.range(ADJ_OUT, Bound::Included(&start), Bound::Unbounded)?,
    };
    for (key, _) in out_rows {
        let (_from, edge_type, to, edge_id) = decode_adj_key(&key);
        wtx.delete(ADJ_OUT, &key)?;
        wtx.delete(ADJ_IN, &adj_in_key(NodeId(to), &edge_type, id, edge_id))?;
        wtx.delete(EDGES, &edge_key(edge_id))?;
    }

    let in_rows = match end {
        Some(end) => wtx.range(ADJ_IN, Bound::Included(&start), Bound::Excluded(&end))?,
        None => wtx.range(ADJ_IN, Bound::Included(&start), Bound::Unbounded)?,
    };
    for (key, _) in in_rows {
        let (_to, edge_type, from, edge_id) = decode_adj_key(&key);
        wtx.delete(ADJ_IN, &key)?;
        wtx.delete(ADJ_OUT, &adj_out_key(NodeId(from), &edge_type, id, edge_id))?;
        wtx.delete(EDGES, &edge_key(edge_id))?;
    }

    if let Some(bytes) = wtx.get(NODES, &node_key(id))? {
        let record: NodeRecord = decode(&bytes)?;
        index::on_delete(wtx, id, &record)?;
    }
    wtx.delete(NODES, &node_key(id))?;
    Ok(())
}

/// Atomic read-modify-write of one node's properties (its label is kept).
pub(crate) fn update_node_properties_in<W: StorageWriteTx>(
    wtx: &mut W,
    id: NodeId,
    mutate: impl FnOnce(&mut Properties),
) -> Result<(), BknError> {
    let bytes = wtx.get(NODES, &node_key(id))?.ok_or(BknError::NotFound)?;
    let old: NodeRecord = decode(&bytes)?;
    let mut record = old.clone();
    mutate(&mut record.properties);
    index::on_update(wtx, id, &old, &record)?;
    wtx.put(NODES, &node_key(id), &encode(&record)?)?;
    Ok(())
}

/// Deletes one edge together with both of its adjacency-index rows.
/// Returns `false` if no such edge exists.
pub(crate) fn delete_edge_in<W: StorageWriteTx>(
    wtx: &mut W,
    edge: EdgeId,
) -> Result<bool, BknError> {
    let Some(bytes) = wtx.get(EDGES, &edge_key(edge))? else {
        return Ok(false);
    };
    let record: EdgeRecord = decode(&bytes)?;
    wtx.delete(
        ADJ_OUT,
        &adj_out_key(record.from, &record.edge_type, record.to, edge),
    )?;
    wtx.delete(
        ADJ_IN,
        &adj_in_key(record.to, &record.edge_type, record.from, edge),
    )?;
    wtx.delete(EDGES, &edge_key(edge))?;
    Ok(true)
}

pub(crate) fn update_edge_properties_in<W: StorageWriteTx>(
    wtx: &mut W,
    edge: EdgeId,
    mutate: impl FnOnce(&mut Properties),
) -> Result<(), BknError> {
    let bytes = wtx.get(EDGES, &edge_key(edge))?.ok_or(BknError::NotFound)?;
    let mut record: EdgeRecord = decode(&bytes)?;
    mutate(&mut record.properties);
    wtx.put(EDGES, &edge_key(edge), &encode(&record)?)?;
    Ok(())
}

pub(crate) fn neighbors_out_in<R: StorageReadTx>(
    rtx: &R,
    node: NodeId,
    edge_type: &str,
) -> Result<Vec<(NodeId, EdgeId)>, BknError> {
    let start = adj_type_prefix(node, edge_type);
    let end = adj_type_upper_bound(node, edge_type);
    let rows = rtx.range(ADJ_OUT, Bound::Included(&start), Bound::Included(&end))?;
    Ok(rows
        .into_iter()
        .map(|(k, _)| {
            let (_first, _edge_type, second, edge_id) = decode_adj_key(&k);
            (NodeId(second), edge_id)
        })
        .collect())
}

pub(crate) fn neighbors_in_in<R: StorageReadTx>(
    rtx: &R,
    node: NodeId,
    edge_type: &str,
) -> Result<Vec<(NodeId, EdgeId)>, BknError> {
    let start = adj_type_prefix(node, edge_type);
    let end = adj_type_upper_bound(node, edge_type);
    let rows = rtx.range(ADJ_IN, Bound::Included(&start), Bound::Included(&end))?;
    Ok(rows
        .into_iter()
        .map(|(k, _)| {
            let (_first, _edge_type, second, edge_id) = decode_adj_key(&k);
            (NodeId(second), edge_id)
        })
        .collect())
}

pub(crate) fn neighbors_any_in<R: StorageReadTx>(
    rtx: &R,
    table: crate::TableSpec,
    node: NodeId,
) -> Result<Vec<(String, NodeId, EdgeId)>, BknError> {
    let start = node_key(node);
    let end = next_node_prefix(node);
    let rows = match end {
        Some(end) => rtx.range(table, Bound::Included(&start), Bound::Excluded(&end))?,
        None => rtx.range(table, Bound::Included(&start), Bound::Unbounded)?,
    };
    Ok(rows
        .into_iter()
        .map(|(k, _)| {
            let (_first, edge_type, second, edge_id) = decode_adj_key(&k);
            (edge_type, NodeId(second), edge_id)
        })
        .collect())
}

pub(crate) fn top_hubs_in<R: StorageReadTx>(
    rtx: &R,
    limit: usize,
    direction: crate::graph::traversal::Direction,
    label_filter: Option<&str>,
) -> Result<Vec<(NodeId, usize)>, BknError> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    use std::cmp::Reverse;
    use std::collections::BinaryHeap;

    let candidates: Vec<NodeId> = match label_filter {
        Some(label) => index::nodes_by_label_in(rtx, label)?,
        None => rtx
            .range(NODES, Bound::Unbounded, Bound::Unbounded)?
            .iter()
            .map(|(k, _)| crate::graph::codec::decode_node_id(k))
            .collect::<Result<_, _>>()?,
    };
    let mut heap: BinaryHeap<Reverse<(usize, u64)>> = BinaryHeap::with_capacity(limit);

    for node_id in candidates {

        let deg = match direction {
            crate::graph::traversal::Direction::Out => {
                neighbors_any_in(rtx, ADJ_OUT, node_id)?.len()
            }
            crate::graph::traversal::Direction::In => neighbors_any_in(rtx, ADJ_IN, node_id)?.len(),
            crate::graph::traversal::Direction::Both => {
                neighbors_any_in(rtx, ADJ_OUT, node_id)?.len()
                    + neighbors_any_in(rtx, ADJ_IN, node_id)?.len()
            }
        };

        if heap.len() < limit {
            heap.push(Reverse((deg, node_id.0)));
        } else if let Some(&Reverse((min_deg, _))) = heap.peek()
            && deg > min_deg
        {
            heap.pop();
            heap.push(Reverse((deg, node_id.0)));
        }
    }

    let mut result: Vec<(NodeId, usize)> = heap
        .into_sorted_vec()
        .into_iter()
        .map(|Reverse((deg, id))| (NodeId(id), deg))
        .collect();
    result.reverse();
    Ok(result)
}

pub(crate) fn cascade_delete_in<W: StorageWriteTx>(
    wtx: &mut W,
    root: NodeId,
    containment_edge_type: &str,
) -> Result<Vec<NodeId>, BknError> {
    if get_node_in(wtx, root)?.is_none() {
        return Err(BknError::NotFound);
    }

    let mut to_delete = Vec::new();
    let mut visited = std::collections::HashSet::new();
    let mut queue = std::collections::VecDeque::new();

    visited.insert(root);
    queue.push_back(root);

    while let Some(current) = queue.pop_front() {
        to_delete.push(current);
        let neighbors = neighbors_out_in(wtx, current, containment_edge_type)?;
        for (child, _) in neighbors {
            if visited.insert(child) {
                queue.push_back(child);
            }
        }
    }

    for &node_id in to_delete.iter().rev() {
        delete_node_in(wtx, node_id)?;
    }

    Ok(to_delete)
}
