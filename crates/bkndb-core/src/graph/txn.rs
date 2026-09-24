//! Batch-transaction API for the graph layer, mirroring
//! [`crate::relational::txn`]'s `RelWriteBatch`/`BatchTable` shape exactly:
//! the batch owns its write tx (never borrows it) so it can be handed to a
//! `for<'w> FnOnce(&mut GraphWriteBatch<B::WriteTx<'w>>)` closure and still
//! be able to `commit()` it afterward.

use crate::graph::codec::{ADJ_IN, ADJ_OUT};
use crate::graph::db::{
    create_edge_in, create_edges_bulk_in, create_node_in, create_nodes_bulk_in, delete_node_in,
    get_edge_in, get_node_in, neighbors_any_in, neighbors_in_in, neighbors_out_in,
    update_edge_properties_in,
};
use crate::graph::model::{EdgeId, EdgeRecord, NodeId, NodeRecord, Properties};
use crate::{BknError, StorageReadTx, StorageWriteTx};

/// One open write transaction shared across several [`BatchGraph`] views
/// (one per call to [`GraphWriteBatch::graph`]), so that node/edge
/// mutations spread across several calls commit together atomically.
/// Built via [`crate::graph::GraphDb::write_tx`].
pub struct GraphWriteBatch<W: StorageWriteTx> {
    wtx: W,
}

impl<W: StorageWriteTx> GraphWriteBatch<W> {
    pub(crate) fn new(wtx: W) -> Self {
        Self { wtx }
    }

    pub(crate) fn into_inner(self) -> W {
        self.wtx
    }

    pub fn graph(&mut self) -> BatchGraph<'_, W> {
        BatchGraph::new(&mut self.wtx)
    }
}

/// A view into an in-progress [`GraphWriteBatch`] (or, nested one level
/// deeper, into [`crate::db::DbWriteBatch`]'s cross-model batch). Mirrors
/// [`crate::graph::GraphDb`]'s own write/read methods, but every method here
/// reads and writes through the batch's already-open transaction instead of
/// opening/committing its own.
pub struct BatchGraph<'s, W: StorageWriteTx> {
    wtx: &'s mut W,
}

impl<'s, W: StorageWriteTx> BatchGraph<'s, W> {
    pub(crate) fn new(wtx: &'s mut W) -> Self {
        Self { wtx }
    }

    /// See [`crate::graph::GraphDb::create_node`].
    pub fn create_node(&mut self, label: &str, properties: Properties) -> Result<NodeId, BknError> {
        create_node_in(self.wtx, label, properties)
    }

    /// Creates multiple nodes in a single call, reserving sequential IDs in one counter update.
    pub fn create_nodes_bulk(
        &mut self,
        nodes: impl IntoIterator<Item = (impl Into<String>, Properties)>,
    ) -> Result<Vec<NodeId>, BknError> {
        create_nodes_bulk_in(self.wtx, nodes)
    }

    /// See [`crate::graph::GraphDb::create_edge`].
    pub fn create_edge(
        &mut self,
        from: NodeId,
        edge_type: &str,
        to: NodeId,
        properties: Properties,
    ) -> Result<EdgeId, BknError> {
        create_edge_in(self.wtx, from, edge_type, to, properties)
    }

    /// Creates multiple edges in a single call, reserving sequential IDs in one counter update.
    pub fn create_edges_bulk(
        &mut self,
        edges: impl IntoIterator<Item = (NodeId, impl Into<String>, NodeId, Properties)>,
    ) -> Result<Vec<EdgeId>, BknError> {
        create_edges_bulk_in(self.wtx, edges)
    }

    /// See [`crate::graph::GraphDb::delete_node`].
    pub fn delete_node(&mut self, id: NodeId) -> Result<(), BknError> {
        delete_node_in(self.wtx, id)
    }

    /// See [`crate::graph::GraphDb::update_edge_properties`].
    pub fn update_edge_properties(
        &mut self,
        edge: EdgeId,
        mutate: impl FnOnce(&mut Properties),
    ) -> Result<(), BknError> {
        update_edge_properties_in(self.wtx, edge, mutate)
    }

    /// See [`crate::graph::GraphDb::get_node`]. Reads the batch's own
    /// pending writes (read-your-own-writes), not just the pre-batch
    /// snapshot.
    pub fn get_node(&self, id: NodeId) -> Result<Option<NodeRecord>, BknError> {
        get_node_in(self.wtx, id)
    }

    /// See [`crate::graph::GraphDb::get_edge`].
    pub fn get_edge(&self, id: EdgeId) -> Result<Option<EdgeRecord>, BknError> {
        get_edge_in(self.wtx, id)
    }

    /// Edges of any type from `node`, filtered to a specific `edge_type`.
    pub fn neighbors_out(&self, node: NodeId, edge_type: &str) -> Result<Vec<(NodeId, EdgeId)>, BknError> {
        neighbors_out_in(self.wtx, node, edge_type)
    }

    /// Edges of any type to `node`, filtered to a specific `edge_type`.
    pub fn neighbors_in(&self, node: NodeId, edge_type: &str) -> Result<Vec<(NodeId, EdgeId)>, BknError> {
        neighbors_in_in(self.wtx, node, edge_type)
    }

    /// All outgoing edges from `node`, any type — (edge_type, neighbor, edge_id).
    pub fn neighbors_out_any(&self, node: NodeId) -> Result<Vec<(String, NodeId, EdgeId)>, BknError> {
        neighbors_any_in(self.wtx, ADJ_OUT, node)
    }

    /// All incoming edges into `node`, any type — (edge_type, neighbor, edge_id).
    pub fn neighbors_in_any(&self, node: NodeId) -> Result<Vec<(String, NodeId, EdgeId)>, BknError> {
        neighbors_any_in(self.wtx, ADJ_IN, node)
    }

    pub fn out_degree(&self, node: NodeId) -> Result<usize, BknError> {
        Ok(self.neighbors_out_any(node)?.len())
    }

    pub fn in_degree(&self, node: NodeId) -> Result<usize, BknError> {
        Ok(self.neighbors_in_any(node)?.len())
    }

    pub fn traversal(&self) -> crate::graph::traversal::TraversalOnTx<'_, W> {
        crate::graph::traversal::TraversalOnTx::new(self.wtx)
    }

    pub fn find_shortest_path(
        &self,
        start: NodeId,
        target: NodeId,
        direction: crate::graph::traversal::Direction,
        edge_types: Option<&[&str]>,
    ) -> Result<Option<crate::graph::traversal::PathResult>, BknError> {
        crate::graph::traversal::find_shortest_path_in(self.wtx, start, target, direction, edge_types)
    }

    pub fn top_hubs(
        &self,
        limit: usize,
        direction: crate::graph::traversal::Direction,
        label: Option<&str>,
    ) -> Result<Vec<(NodeId, usize)>, BknError> {
        crate::graph::db::top_hubs_in(self.wtx, limit, direction, label)
    }

    pub fn cascade_delete(
        &mut self,
        root: NodeId,
        containment_edge_type: &str,
    ) -> Result<Vec<NodeId>, BknError> {
        crate::graph::db::cascade_delete_in(self.wtx, root, containment_edge_type)
    }
}

/// A read-only view of the graph over an open [`StorageReadTx`].
pub struct ReadGraph<'s, R: StorageReadTx> {
    rtx: &'s R,
}

impl<'s, R: StorageReadTx> ReadGraph<'s, R> {
    pub(crate) fn new(rtx: &'s R) -> Self {
        Self { rtx }
    }

    pub fn get_node(&self, id: NodeId) -> Result<Option<NodeRecord>, BknError> {
        get_node_in(self.rtx, id)
    }

    pub fn get_edge(&self, id: EdgeId) -> Result<Option<EdgeRecord>, BknError> {
        get_edge_in(self.rtx, id)
    }

    pub fn neighbors_out(&self, node: NodeId, edge_type: &str) -> Result<Vec<(NodeId, EdgeId)>, BknError> {
        neighbors_out_in(self.rtx, node, edge_type)
    }

    pub fn neighbors_in(&self, node: NodeId, edge_type: &str) -> Result<Vec<(NodeId, EdgeId)>, BknError> {
        neighbors_in_in(self.rtx, node, edge_type)
    }

    pub fn neighbors_out_any(&self, node: NodeId) -> Result<Vec<(String, NodeId, EdgeId)>, BknError> {
        neighbors_any_in(self.rtx, ADJ_OUT, node)
    }

    pub fn neighbors_in_any(&self, node: NodeId) -> Result<Vec<(String, NodeId, EdgeId)>, BknError> {
        neighbors_any_in(self.rtx, ADJ_IN, node)
    }

    pub fn out_degree(&self, node: NodeId) -> Result<usize, BknError> {
        Ok(self.neighbors_out_any(node)?.len())
    }

    pub fn in_degree(&self, node: NodeId) -> Result<usize, BknError> {
        Ok(self.neighbors_in_any(node)?.len())
    }

    pub fn traversal(&self) -> crate::graph::traversal::TraversalOnTx<'_, R> {
        crate::graph::traversal::TraversalOnTx::new(self.rtx)
    }

    pub fn find_shortest_path(
        &self,
        start: NodeId,
        target: NodeId,
        direction: crate::graph::traversal::Direction,
        edge_types: Option<&[&str]>,
    ) -> Result<Option<crate::graph::traversal::PathResult>, BknError> {
        crate::graph::traversal::find_shortest_path_in(self.rtx, start, target, direction, edge_types)
    }

    pub fn top_hubs(
        &self,
        limit: usize,
        direction: crate::graph::traversal::Direction,
        label: Option<&str>,
    ) -> Result<Vec<(NodeId, usize)>, BknError> {
        crate::graph::db::top_hubs_in(self.rtx, limit, direction, label)
    }
}


