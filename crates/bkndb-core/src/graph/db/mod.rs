use std::ops::Bound;
use std::sync::Arc;

use crate::graph::codec::{
    ADJ_IN, ADJ_OUT, EDGES, META, NEXT_EDGE_ID_KEY, NEXT_NODE_ID_KEY, NODES, adj_in_key,
    adj_out_key, adj_type_prefix, adj_type_upper_bound, decode_adj_key, edge_key, next_node_prefix,
    node_key,
};
use crate::graph::index;
use crate::graph::model::{EdgeId, EdgeRecord, NodeId, NodeRecord, PropValue, Properties};
use crate::{BknError, StorageBackend, StorageReadTx, StorageWriteTx};

mod ops;

pub(crate) use ops::*;

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

    pub fn create_nodes_bulk(
        &self,
        nodes: impl IntoIterator<Item = (impl Into<String>, Properties)>,
    ) -> Result<Vec<NodeId>, BknError> {
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

    pub fn create_edges_bulk(
        &self,
        edges: impl IntoIterator<Item = (NodeId, impl Into<String>, NodeId, Properties)>,
    ) -> Result<Vec<EdgeId>, BknError> {
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
    pub fn neighbors_out(
        &self,
        node: NodeId,
        edge_type: &str,
    ) -> Result<Vec<(NodeId, EdgeId)>, BknError> {
        let rtx = self.backend.begin_read()?;
        neighbors_out_in(&rtx, node, edge_type)
    }

    pub fn neighbors_in(
        &self,
        node: NodeId,
        edge_type: &str,
    ) -> Result<Vec<(NodeId, EdgeId)>, BknError> {
        let rtx = self.backend.begin_read()?;
        neighbors_in_in(&rtx, node, edge_type)
    }

    /// All outgoing edges from `node`, any type — (edge_type, neighbor, edge_id).
    pub fn neighbors_out_any(
        &self,
        node: NodeId,
    ) -> Result<Vec<(String, NodeId, EdgeId)>, BknError> {
        let rtx = self.backend.begin_read()?;
        neighbors_any_in(&rtx, ADJ_OUT, node)
    }

    /// All incoming edges into `node`, any type — (edge_type, neighbor, edge_id).
    pub fn neighbors_in_any(
        &self,
        node: NodeId,
    ) -> Result<Vec<(String, NodeId, EdgeId)>, BknError> {
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

    /// Atomic read-modify-write of one node's properties. Errors with
    /// [`BknError::NotFound`] if the node doesn't exist.
    pub fn update_node_properties(
        &self,
        id: NodeId,
        mutate: impl FnOnce(&mut Properties),
    ) -> Result<(), BknError> {
        let mut wtx = self.backend.begin_write()?;
        update_node_properties_in(&mut wtx, id, mutate)?;
        wtx.commit()
    }

    /// Deletes a single edge; returns `false` if it didn't exist.
    pub fn delete_edge(&self, edge: EdgeId) -> Result<bool, BknError> {
        let mut wtx = self.backend.begin_write()?;
        let existed = delete_edge_in(&mut wtx, edge)?;
        wtx.commit()?;
        Ok(existed)
    }

    /// Runs a `MATCH ... RETURN ...` pattern query (see [`crate::lang::graph`])
    /// against a read snapshot.
    ///
    /// ```ignore
    /// db.query("MATCH (a:Person {name: $n})-[:KNOWS]->(b) RETURN b.name", Params::named([("n", "Ana")]))?;
    /// ```
    pub fn query(&self, text: &str, params: impl Into<crate::lang::Params>) -> Result<crate::lang::QueryResult, BknError> {
        crate::lang::graph::run(&self.backend.begin_read()?, text, &params.into())
    }

    /// Ids of every node with `label`, ascending (via the label index).
    pub fn nodes_by_label(&self, label: &str) -> Result<Vec<NodeId>, BknError> {
        index::nodes_by_label_in(&self.backend.begin_read()?, label)
    }

    /// Ids of nodes with `label` whose `property` equals `value`, ascending.
    /// Uses a property index when one exists (see
    /// [`GraphDb::create_property_index`]), else scans the label's nodes.
    pub fn find_nodes(&self, label: &str, property: &str, value: &PropValue) -> Result<Vec<NodeId>, BknError> {
        index::find_nodes_in(&self.backend.begin_read()?, label, property, value)
    }

    /// Indexes `property` of nodes with `label` (backfilled immediately) so
    /// [`GraphDb::find_nodes`] becomes a lookup. Only `Int`/`Str` values are
    /// indexed. Returns `false` if the index already existed.
    pub fn create_property_index(&self, label: &str, property: &str) -> Result<bool, BknError> {
        let mut wtx = self.backend.begin_write()?;
        let created = index::create_property_index_in(&mut wtx, label, property)?;
        wtx.commit()?;
        Ok(created)
    }

    pub fn drop_property_index(&self, label: &str, property: &str) -> Result<bool, BknError> {
        let mut wtx = self.backend.begin_write()?;
        let dropped = index::drop_property_index_in(&mut wtx, label, property)?;
        wtx.commit()?;
        Ok(dropped)
    }

    /// Every property index as `(label, property)`.
    pub fn property_indexes(&self) -> Result<Vec<(String, String)>, BknError> {
        index::property_indexes_in(&self.backend.begin_read()?)
    }

    /// Rebuilds all graph indexes from the node table. Only needed once for
    /// databases created before graph indexes existed, where label lookups
    /// otherwise fall back to full scans.
    pub fn rebuild_indexes(&self) -> Result<(), BknError> {
        let mut wtx = self.backend.begin_write()?;
        index::rebuild_indexes_in(&mut wtx)?;
        wtx.commit()
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

    /// Lowest-cost path (Dijkstra) where each edge costs its numeric
    /// `weight_property`, or `default_weight` when absent. See
    /// [`crate::graph::WeightedPath`].
    pub fn find_weighted_path(
        &self,
        start: NodeId,
        target: NodeId,
        direction: crate::graph::traversal::Direction,
        edge_types: Option<&[&str]>,
        weight_property: &str,
        default_weight: f64,
    ) -> Result<Option<crate::graph::traversal::WeightedPath>, BknError> {
        let rtx = self.backend.begin_read()?;
        crate::graph::traversal::find_weighted_path_in(&rtx, start, target, direction, edge_types, weight_property, default_weight)
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
        F: for<'w> FnOnce(
            &mut crate::graph::txn::GraphWriteBatch<B::WriteTx<'w>>,
        ) -> Result<R, BknError>,
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
