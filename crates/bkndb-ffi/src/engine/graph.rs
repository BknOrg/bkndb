//! Graph methods of [`BknDbEngine`](super::BknDbEngine): nodes, edges, indexes, traversal.
use super::*;

#[uniffi::export]
impl BknDbEngine {
    // ---- graph: nodes & edges ----

    /// Creates a single graph node with the given label and properties.
    pub fn create_node(&self, label: String, properties: HashMap<String, FfiPropValue>) -> Result<u64, FfiBknError> {
        write!(self, |b| ops::create_node(b, &label, properties))
    }

    /// Creates multiple nodes in a single atomic transaction.
    pub fn create_nodes_bulk(&self, nodes: Vec<FfiNodeInput>) -> Result<Vec<u64>, FfiBknError> {
        let nodes: Vec<(String, FfiProps)> = nodes.into_iter().map(|n| (n.label, n.properties)).collect();
        write!(self, |b| ops::create_nodes(b, nodes))
    }

    /// Retrieves a node by its numeric ID.
    pub fn get_node(&self, id: u64) -> Result<Option<FfiNodeRecord>, FfiBknError> {
        Ok(with_db!(self, db => db.graph().get_node(NodeId(id)))?.map(|n| ops::node_record(id, n)))
    }

    /// Deletes a node and all incident edges atomically.
    pub fn delete_node(&self, id: u64) -> Result<(), FfiBknError> {
        write!(self, |b| ops::delete_node(b, id))
    }

    /// Replaces/adds the properties in `set` and removes those named in
    /// `unset`, atomically. Fails with `NotFound` if the node doesn't exist.
    pub fn update_node_properties(
        &self,
        id: u64,
        set: HashMap<String, FfiPropValue>,
        unset: Vec<String>,
    ) -> Result<(), FfiBknError> {
        write!(self, |b| ops::update_node(b, id, set, unset))
    }

    /// Creates a directed edge between two existing nodes.
    pub fn create_edge(
        &self,
        from: u64,
        edge_type: String,
        to: u64,
        properties: HashMap<String, FfiPropValue>,
    ) -> Result<u64, FfiBknError> {
        write!(self, |b| ops::create_edge(b, from, &edge_type, to, properties))
    }

    /// Creates multiple edges in a single atomic transaction.
    pub fn create_edges_bulk(&self, edges: Vec<FfiEdgeInput>) -> Result<Vec<u64>, FfiBknError> {
        let edges: Vec<_> = edges.into_iter().map(|e| (e.from, e.edge_type, e.to, e.properties)).collect();
        write!(self, |b| ops::create_edges(b, edges))
    }

    /// Retrieves an edge by its numeric ID.
    pub fn get_edge(&self, id: u64) -> Result<Option<FfiEdgeRecord>, FfiBknError> {
        Ok(with_db!(self, db => db.graph().get_edge(EdgeId(id)))?.map(|e| ops::edge_record(id, e)))
    }

    /// Deletes one edge; returns whether it existed.
    pub fn delete_edge(&self, id: u64) -> Result<bool, FfiBknError> {
        write!(self, |b| ops::delete_edge(b, id))
    }

    /// See [`BknDbEngine::update_node_properties`].
    pub fn update_edge_properties(
        &self,
        id: u64,
        set: HashMap<String, FfiPropValue>,
        unset: Vec<String>,
    ) -> Result<(), FfiBknError> {
        write!(self, |b| ops::update_edge(b, id, set, unset))
    }

    // ---- graph: indexes ----

    /// Ids of every node with `label`, ascending.
    pub fn nodes_by_label(&self, label: String) -> Result<Vec<u64>, FfiBknError> {
        read!(self, |r| ops::nodes_by_label(r, &label))
    }

    /// Number of nodes with `label`.
    pub fn count_nodes(&self, label: String) -> Result<u64, FfiBknError> {
        Ok(self.nodes_by_label(label)?.len() as u64)
    }

    /// Ids of nodes with `label` whose `property` equals `value`, ascending.
    /// A lookup when the property is indexed (`create_node_index`), else a
    /// scan of the label's nodes.
    pub fn find_nodes(&self, label: String, property: String, value: FfiPropValue) -> Result<Vec<u64>, FfiBknError> {
        let value = value.into();
        read!(self, |r| ops::find_nodes(r, &label, &property, value))
    }

    /// Indexes `property` of nodes with `label` (backfilled immediately;
    /// only Int/Str values are indexed). Returns `false` if it already existed.
    pub fn create_node_index(&self, label: String, property: String) -> Result<bool, FfiBknError> {
        write!(self, |b| ops::set_node_index(b, &label, &property, true))
    }

    pub fn drop_node_index(&self, label: String, property: String) -> Result<bool, FfiBknError> {
        write!(self, |b| ops::set_node_index(b, &label, &property, false))
    }

    pub fn list_node_indexes(&self) -> Result<Vec<FfiPropertyIndex>, FfiBknError> {
        read!(self, |r| ops::node_indexes(r))
    }

    /// Rebuilds every graph index. Only needed once for databases created
    /// by versions without graph indexes (lookups work without it, but scan).
    pub fn rebuild_graph_indexes(&self) -> Result<(), FfiBknError> {
        write!(self, |b| ops::rebuild_graph_indexes(b))
    }

    // ---- graph: traversal ----

    /// Lowest-cost path (Dijkstra): each edge costs its numeric
    /// `weight_property`, or `default_weight` when it has none. Weights must
    /// be non-negative.
    pub fn find_weighted_path(
        &self,
        start: u64,
        target: u64,
        direction: FfiDirection,
        edge_types: Option<Vec<String>>,
        weight_property: String,
        default_weight: f64,
    ) -> Result<Option<FfiWeightedPath>, FfiBknError> {
        read!(self, |r| ops::weighted_path(r, start, target, direction.into(), edge_types, &weight_property, default_weight))
    }

    /// Returns outgoing neighbors for `node` along edges of type `edge_type`.
    pub fn neighbors_out(&self, node: u64, edge_type: String) -> Result<Vec<FfiNeighbor>, FfiBknError> {
        Ok(neighbors_of(self.neighbors(node, FfiDirection::Out, Some(edge_type))?))
    }

    /// Returns incoming neighbors for `node` along edges of type `edge_type`.
    pub fn neighbors_in(&self, node: u64, edge_type: String) -> Result<Vec<FfiNeighbor>, FfiBknError> {
        Ok(neighbors_of(self.neighbors(node, FfiDirection::In, Some(edge_type))?))
    }

    /// Neighbors of `node` in `direction`, over every edge type unless
    /// `edge_type` is given.
    pub fn neighbors(&self, node: u64, direction: FfiDirection, edge_type: Option<String>) -> Result<Vec<FfiTypedNeighbor>, FfiBknError> {
        read!(self, |r| ops::neighbors(r, node, direction.into(), edge_type.as_deref()))
    }

    /// Number of edges at `node` in `direction` (optionally of one type).
    pub fn degree(&self, node: u64, direction: FfiDirection, edge_type: Option<String>) -> Result<u64, FfiBknError> {
        Ok(self.neighbors(node, direction, edge_type)?.len() as u64)
    }

    /// Breadth-first traversal from `start` up to `max_depth` hops. Returns
    /// `start` itself first (depth 0). `edge_types` restricts which edges are
    /// followed; `node_label` only visits (and expands) nodes with that label.
    pub fn traverse(
        &self,
        start: u64,
        direction: FfiDirection,
        max_depth: u32,
        edge_types: Option<Vec<String>>,
        node_label: Option<String>,
    ) -> Result<Vec<FfiTraversalHit>, FfiBknError> {
        read!(self, |r| ops::traverse(r, start, direction.into(), max_depth, edge_types, node_label))
    }

    /// Computes the unweighted shortest path between `start` and `target` using BFS.
    pub fn find_shortest_path(
        &self,
        start: u64,
        target: u64,
        direction: FfiDirection,
        edge_types: Option<Vec<String>>,
    ) -> Result<Option<FfiPathResult>, FfiBknError> {
        let refs: Option<Vec<&str>> = edge_types.as_ref().map(|v| v.iter().map(String::as_str).collect());
        let path = with_db!(self, db => db.graph().find_shortest_path(
            NodeId(start),
            NodeId(target),
            Direction::from(direction),
            refs.as_deref(),
        ))?;
        Ok(path.map(Into::into))
    }

    /// Finds the top `k` hub nodes by degree centrality, optionally filtered by node label.
    pub fn top_hubs(&self, k: u32, direction: FfiDirection, edge_type: Option<String>) -> Result<Vec<FfiHubRecord>, FfiBknError> {
        let hubs = with_db!(self, db => db.graph().top_hubs(k as usize, direction.into(), edge_type.as_deref()))?;
        Ok(hubs.into_iter().map(|(n, d)| FfiHubRecord { node_id: n.0, degree: d as u64 }).collect())
    }

    /// Recursively cascade-deletes `root` and all descendants reachable via `containment_edge`.
    pub fn cascade_delete(&self, root: u64, containment_edge: String) -> Result<Vec<u64>, FfiBknError> {
        write!(self, |b| ops::cascade_delete(b, root, &containment_edge))
    }

    /// Ingests graph nodes, edges and relational rows (upserted by primary
    /// key) in a single atomic transaction.
    pub fn sync_batch(&self, batch: FfiSyncBatch) -> Result<FfiSyncResult, FfiBknError> {
        write!(self, |b| ops::sync(b, batch))
    }

}
