use std::ops::Bound;
use std::sync::Arc;

use crate::graph::codec::{
    adj_in_key, adj_out_key, adj_type_prefix, adj_type_upper_bound, decode_adj_key, edge_key,
    next_node_prefix, node_key, ADJ_IN, ADJ_OUT, EDGES, META, NEXT_EDGE_ID_KEY, NEXT_NODE_ID_KEY,
    NODES,
};
use crate::graph::model::{EdgeId, EdgeRecord, NodeId, NodeRecord, Properties};
use crate::{BknError, StorageBackend, StorageReadTx, StorageWriteTx};

fn encode<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, BknError> {
    bincode::serialize(value).map_err(|e| BknError::Encoding(e.to_string()))
}

fn decode<T: for<'de> serde::Deserialize<'de>>(bytes: &[u8]) -> Result<T, BknError> {
    bincode::deserialize(bytes).map_err(|e| BknError::Encoding(e.to_string()))
}

/// Reads the persisted id counter (default 1 if absent), writes counter+count,
/// and returns the old value — all inside the caller's write tx, so id
/// allocation and record inserts commit atomically together.
pub(crate) fn reserve_ids<W: StorageWriteTx>(
    wtx: &mut W,
    counter_key: &[u8],
    count: u64,
) -> Result<u64, BknError> {
    if count == 0 {
        return Ok(1);
    }
    let current = match wtx.get(META, counter_key)? {
        Some(bytes) => u64::from_be_bytes(
            bytes
                .as_slice()
                .try_into()
                .map_err(|_| BknError::Encoding("corrupt id counter".to_string()))?,
        ),
        None => 1,
    };
    wtx.put(META, counter_key, &(current + count).to_be_bytes())?;
    Ok(current)
}

fn next_id<W: StorageWriteTx>(wtx: &mut W, counter_key: &[u8]) -> Result<u64, BknError> {
    reserve_ids(wtx, counter_key, 1)
}

/// Property-graph facade layered on top of a raw [`StorageBackend`].
pub struct GraphDb<B: StorageBackend> {
    pub(crate) backend: Arc<B>,
}

impl<B: StorageBackend> GraphDb<B> {
    pub fn new(backend: B) -> Self {
        Self::from_arc(Arc::new(backend))
    }

    /// Shares an existing backend instance (e.g. with a [`crate::relational::RelationalDb`]
    /// over the same underlying database) instead of taking sole ownership of it.
    pub fn from_arc(backend: Arc<B>) -> Self {
        Self { backend }
    }

    pub fn create_node(&self, label: &str, properties: Properties) -> Result<NodeId, BknError> {
        let mut wtx = self.backend.begin_write()?;
        let id = create_node_in(&mut wtx, label, properties)?;
        wtx.commit()?;
        Ok(id)
    }

    pub fn create_nodes_bulk(&self, nodes: impl IntoIterator<Item = (impl Into<String>, Properties)>) -> Result<Vec<NodeId>, BknError> {
        let mut wtx = self.backend.begin_write()?;
        let ids = create_nodes_bulk_in(&mut wtx, nodes)?;
        wtx.commit()?;
        Ok(ids)
    }

    pub fn get_node(&self, id: NodeId) -> Result<Option<NodeRecord>, BknError> {
        let rtx = self.backend.begin_read()?;
        get_node_in(&rtx, id)
    }

    pub fn create_edge(
        &self,
        from: NodeId,
        edge_type: &str,
        to: NodeId,
        properties: Properties,
    ) -> Result<EdgeId, BknError> {
        let mut wtx = self.backend.begin_write()?;
        let id = create_edge_in(&mut wtx, from, edge_type, to, properties)?;
        wtx.commit()?;
        Ok(id)
    }

    pub fn create_edges_bulk(&self, edges: impl IntoIterator<Item = (NodeId, impl Into<String>, NodeId, Properties)>) -> Result<Vec<EdgeId>, BknError> {
        let mut wtx = self.backend.begin_write()?;
        let ids = create_edges_bulk_in(&mut wtx, edges)?;
        wtx.commit()?;
        Ok(ids)
    }

    pub fn get_edge(&self, id: EdgeId) -> Result<Option<EdgeRecord>, BknError> {
        let rtx = self.backend.begin_read()?;
        get_edge_in(&rtx, id)
    }

    /// Edges of any type from `node`, filtered to a specific `edge_type`.
    pub fn neighbors_out(&self, node: NodeId, edge_type: &str) -> Result<Vec<(NodeId, EdgeId)>, BknError> {
        let rtx = self.backend.begin_read()?;
        neighbors_out_in(&rtx, node, edge_type)
    }

    pub fn neighbors_in(&self, node: NodeId, edge_type: &str) -> Result<Vec<(NodeId, EdgeId)>, BknError> {
        let rtx = self.backend.begin_read()?;
        neighbors_in_in(&rtx, node, edge_type)
    }

    /// All outgoing edges from `node`, any type — (edge_type, neighbor, edge_id).
    pub fn neighbors_out_any(&self, node: NodeId) -> Result<Vec<(String, NodeId, EdgeId)>, BknError> {
        let rtx = self.backend.begin_read()?;
        neighbors_any_in(&rtx, ADJ_OUT, node)
    }

    /// All incoming edges into `node`, any type — (edge_type, neighbor, edge_id).
    pub fn neighbors_in_any(&self, node: NodeId) -> Result<Vec<(String, NodeId, EdgeId)>, BknError> {
        let rtx = self.backend.begin_read()?;
        neighbors_any_in(&rtx, ADJ_IN, node)
    }

    pub fn out_degree(&self, node: NodeId) -> Result<usize, BknError> {
        Ok(self.neighbors_out_any(node)?.len())
    }

    pub fn in_degree(&self, node: NodeId) -> Result<usize, BknError> {
        Ok(self.neighbors_in_any(node)?.len())
    }

    /// Atomically removes `id` and every edge touching it (outgoing and
    /// incoming), along with both adjacency-index rows and the canonical
    /// `edges` record for each.
    pub fn delete_node(&self, id: NodeId) -> Result<(), BknError> {
        let mut wtx = self.backend.begin_write()?;
        delete_node_in(&mut wtx, id)?;
        wtx.commit()?;
        Ok(())
    }

    /// Atomic read-modify-write of one edge's properties. Only the canonical
    /// `edges` table is touched — adjacency indexes carry no properties, so
    /// they never need updating for a property-only mutation.
    pub fn update_edge_properties(
        &self,
        edge: EdgeId,
        mutate: impl FnOnce(&mut Properties),
    ) -> Result<(), BknError> {
        let mut wtx = self.backend.begin_write()?;
        update_edge_properties_in(&mut wtx, edge, mutate)?;
        wtx.commit()?;
        Ok(())
    }

    pub fn traversal(&self) -> crate::graph::traversal::TraversalBuilder<'_, B> {
        crate::graph::traversal::TraversalBuilder::new(self)
    }

    pub fn find_shortest_path(
        &self,
        start: NodeId,
        target: NodeId,
        direction: crate::graph::traversal::Direction,
        edge_types: Option<&[&str]>,
    ) -> Result<Option<crate::graph::traversal::PathResult>, BknError> {
        let rtx = self.backend.begin_read()?;
        crate::graph::traversal::find_shortest_path_in(&rtx, start, target, direction, edge_types)
    }

    pub fn top_hubs(
        &self,
        limit: usize,
        direction: crate::graph::traversal::Direction,
        label: Option<&str>,
    ) -> Result<Vec<(NodeId, usize)>, BknError> {
        let rtx = self.backend.begin_read()?;
        top_hubs_in(&rtx, limit, direction, label)
    }

    pub fn cascade_delete(
        &self,
        root: NodeId,
        containment_edge_type: &str,
    ) -> Result<Vec<NodeId>, BknError> {
        let mut wtx = self.backend.begin_write()?;
        let deleted = cascade_delete_in(&mut wtx, root, containment_edge_type)?;
        wtx.commit()?;
        Ok(deleted)
    }

    /// Runs `f` against one shared write transaction spanning however many
    /// graph mutations it performs (via [`crate::graph::txn::GraphWriteBatch::graph`]),
    /// committing only if `f` returns `Ok`. Mirrors
    /// [`crate::relational::RelationalDb::write_tx`]'s already-proven
    /// open/wrap/run/commit shape exactly.
    pub fn write_tx<F, R>(&self, f: F) -> Result<R, BknError>
    where
        F: for<'w> FnOnce(&mut crate::graph::txn::GraphWriteBatch<B::WriteTx<'w>>) -> Result<R, BknError>,
    {
        let wtx = self.backend.begin_write()?;
        let mut batch = crate::graph::txn::GraphWriteBatch::new(wtx);
        let result = f(&mut batch)?;
        batch.into_inner().commit()?;
        Ok(result)
    }
}

// --- Free functions parameterized over an already-open transaction ---
//
// The shared core of every `GraphDb` write/read method above (each now just
// "open a tx, call the matching `_in` function, commit") and of
// `crate::graph::txn`'s batch API, which calls them directly against one
// caller-held transaction spanning several mutations instead of committing
// after each one.

pub(crate) fn create_node_in<W: StorageWriteTx>(
    wtx: &mut W,
    label: &str,
    properties: Properties,
) -> Result<NodeId, BknError> {
    let id = NodeId(next_id(wtx, NEXT_NODE_ID_KEY)?);
    let record = NodeRecord {
        label: label.to_string(),
        properties,
    };
    wtx.put(NODES, &node_key(id), &encode(&record)?)?;
    Ok(id)
}

pub(crate) fn get_node_in<R: StorageReadTx>(rtx: &R, id: NodeId) -> Result<Option<NodeRecord>, BknError> {
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
    let start_id = reserve_ids(wtx, NEXT_NODE_ID_KEY, items.len() as u64)?;
    let mut ids = Vec::with_capacity(items.len());
    for (i, (label, properties)) in items.into_iter().enumerate() {
        let id = NodeId(start_id + i as u64);
        let record = NodeRecord { label, properties };
        wtx.put(NODES, &node_key(id), &encode(&record)?)?;
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

pub(crate) fn get_edge_in<R: StorageReadTx>(rtx: &R, id: EdgeId) -> Result<Option<EdgeRecord>, BknError> {
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

    wtx.delete(NODES, &node_key(id))?;
    Ok(())
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

    let rows = rtx.range(NODES, Bound::Unbounded, Bound::Unbounded)?;
    let mut heap: BinaryHeap<Reverse<(usize, u64)>> = BinaryHeap::with_capacity(limit);

    for (k, v) in rows {
        let node_id = crate::graph::codec::decode_node_id(&k)?;
        if let Some(target_label) = label_filter {
            let record: NodeRecord = decode(&v)?;
            if record.label != target_label {
                continue;
            }
        }

        let deg = match direction {
            crate::graph::traversal::Direction::Out => neighbors_any_in(rtx, ADJ_OUT, node_id)?.len(),
            crate::graph::traversal::Direction::In => neighbors_any_in(rtx, ADJ_IN, node_id)?.len(),
            crate::graph::traversal::Direction::Both => {
                neighbors_any_in(rtx, ADJ_OUT, node_id)?.len()
                    + neighbors_any_in(rtx, ADJ_IN, node_id)?.len()
            }
        };

        if heap.len() < limit {
            heap.push(Reverse((deg, node_id.0)));
        } else if let Some(&Reverse((min_deg, _))) = heap.peek() {
            if deg > min_deg {
                heap.pop();
                heap.push(Reverse((deg, node_id.0)));
            }
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


