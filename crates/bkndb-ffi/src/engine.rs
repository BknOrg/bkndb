use std::collections::HashMap;
use std::sync::Arc;

use bkndb_core::graph::{EdgeId, NodeId, Properties};
use bkndb_core::Db;
use bkndb::BknDb;
use bkndb::MemoryStorageBackend;

use crate::error::FfiBknError;
use crate::types::{
    core_props_to_ffi, ffi_props_to_core, FfiDirection, FfiEdgeInput, FfiEdgeRecord,
    FfiHubRecord, FfiNeighbor, FfiNodeInput, FfiNodeRecord, FfiPathResult, FfiPathStep,
    FfiPropValue, FfiSyncBatch, FfiSyncResult,
};

enum EngineInner {
    Disk(BknDb),
    Mem(Db<MemoryStorageBackend>),
}

macro_rules! dispatch {
    ($self:expr, $db:ident => $expr:expr) => {
        match &$self.inner {
            EngineInner::Disk($db) => $expr,
            EngineInner::Mem($db) => $expr,
        }
    };
}

/// The main mobile entrypoint for BknDb, thread-safe and exported via UniFFI.
#[derive(uniffi::Object)]
pub struct BknDbEngine {
    inner: EngineInner,
}

#[uniffi::export]
impl BknDbEngine {
    /// Opens or creates an on-disk single-file database at `path`.
    #[uniffi::constructor]
    pub fn open(path: String) -> Result<Arc<Self>, FfiBknError> {
        let db = BknDb::open(path)?;
        Ok(Arc::new(Self {
            inner: EngineInner::Disk(db),
        }))
    }

    /// Creates an ephemeral in-memory database instance.
    #[uniffi::constructor]
    pub fn in_memory() -> Result<Arc<Self>, FfiBknError> {
        let db = BknDb::in_memory();
        Ok(Arc::new(Self {
            inner: EngineInner::Mem(db),
        }))
    }

    /// Creates a single graph node with the given label and properties.
    pub fn create_node(
        &self,
        label: String,
        properties: HashMap<String, FfiPropValue>,
    ) -> Result<u64, FfiBknError> {
        let id = dispatch!(self, db => db.graph().create_node(&label, ffi_props_to_core(properties)))?;
        Ok(id.0)
    }

    /// Creates multiple nodes in a single atomic transaction.
    pub fn create_nodes_bulk(&self, nodes: Vec<FfiNodeInput>) -> Result<Vec<u64>, FfiBknError> {
        let items: Vec<(String, Properties)> = nodes
            .into_iter()
            .map(|n| (n.label, ffi_props_to_core(n.properties)))
            .collect();
        let ids = dispatch!(self, db => db.graph().create_nodes_bulk(items))?;
        Ok(ids.into_iter().map(|id| id.0).collect())
    }

    /// Retrieves a node by its numeric ID.
    pub fn get_node(&self, id: u64) -> Result<Option<FfiNodeRecord>, FfiBknError> {
        let node_opt = dispatch!(self, db => db.graph().get_node(NodeId(id)))?;
        Ok(node_opt.map(|n| FfiNodeRecord {
            id,
            label: n.label,
            properties: core_props_to_ffi(n.properties),
        }))
    }

    /// Deletes a node and all incident edges atomically.
    pub fn delete_node(&self, id: u64) -> Result<(), FfiBknError> {
        dispatch!(self, db => db.graph().delete_node(NodeId(id)))?;
        Ok(())
    }

    /// Creates a directed edge between two existing nodes.
    pub fn create_edge(
        &self,
        from: u64,
        edge_type: String,
        to: u64,
        properties: HashMap<String, FfiPropValue>,
    ) -> Result<u64, FfiBknError> {
        let id = dispatch!(self, db => db.graph().create_edge(
            NodeId(from),
            &edge_type,
            NodeId(to),
            ffi_props_to_core(properties),
        ))?;
        Ok(id.0)
    }

    /// Creates multiple edges in a single atomic transaction.
    pub fn create_edges_bulk(&self, edges: Vec<FfiEdgeInput>) -> Result<Vec<u64>, FfiBknError> {
        let items: Vec<(NodeId, String, NodeId, Properties)> = edges
            .into_iter()
            .map(|e| (NodeId(e.from), e.edge_type, NodeId(e.to), ffi_props_to_core(e.properties)))
            .collect();
        let ids = dispatch!(self, db => db.graph().create_edges_bulk(items))?;
        Ok(ids.into_iter().map(|id| id.0).collect())
    }

    /// Retrieves an edge by its numeric ID.
    pub fn get_edge(&self, id: u64) -> Result<Option<FfiEdgeRecord>, FfiBknError> {
        let edge_opt = dispatch!(self, db => db.graph().get_edge(EdgeId(id)))?;
        Ok(edge_opt.map(|e| FfiEdgeRecord {
            id,
            from: e.from.0,
            to: e.to.0,
            edge_type: e.edge_type,
            properties: core_props_to_ffi(e.properties),
        }))
    }

    /// Returns outgoing neighbors for `node` along edges of type `edge_type`.
    pub fn neighbors_out(&self, node: u64, edge_type: String) -> Result<Vec<FfiNeighbor>, FfiBknError> {
        let neighbors = dispatch!(self, db => db.graph().neighbors_out(NodeId(node), &edge_type))?;
        Ok(neighbors
            .into_iter()
            .map(|(nid, eid)| FfiNeighbor {
                node_id: nid.0,
                edge_id: eid.0,
            })
            .collect())
    }

    /// Returns incoming neighbors for `node` along edges of type `edge_type`.
    pub fn neighbors_in(&self, node: u64, edge_type: String) -> Result<Vec<FfiNeighbor>, FfiBknError> {
        let neighbors = dispatch!(self, db => db.graph().neighbors_in(NodeId(node), &edge_type))?;
        Ok(neighbors
            .into_iter()
            .map(|(nid, eid)| FfiNeighbor {
                node_id: nid.0,
                edge_id: eid.0,
            })
            .collect())
    }

    /// Computes the unweighted shortest path between `start` and `target` using BFS.
    pub fn find_shortest_path(
        &self,
        start: u64,
        target: u64,
        direction: FfiDirection,
        edge_types: Option<Vec<String>>,
    ) -> Result<Option<FfiPathResult>, FfiBknError> {
        let edge_types_ref: Option<Vec<&str>> = edge_types
            .as_ref()
            .map(|v| v.iter().map(|s| s.as_str()).collect());
        let types_slice = edge_types_ref.as_deref();

        let path_opt = dispatch!(self, db => db.graph().find_shortest_path(
            NodeId(start),
            NodeId(target),
            direction.into(),
            types_slice,
        ))?;

        Ok(path_opt.map(|p| FfiPathResult {
            node_ids: p.nodes().into_iter().map(|n| n.0).collect(),
            edge_ids: p.edges().into_iter().map(|e| e.0).collect(),
            steps: p
                .steps
                .into_iter()
                .map(|s| FfiPathStep {
                    node_id: s.node.0,
                    via_edge_id: s.via_edge.map(|e| e.0),
                    edge_type: s.edge_type,
                })
                .collect(),
        }))
    }

    /// Finds the top `k` hub nodes by degree centrality, optionally filtered by edge type.
    pub fn top_hubs(
        &self,
        k: u32,
        direction: FfiDirection,
        edge_type: Option<String>,
    ) -> Result<Vec<FfiHubRecord>, FfiBknError> {
        let edge_ref = edge_type.as_deref();
        let hubs = dispatch!(self, db => db.graph().top_hubs(k as usize, direction.into(), edge_ref))?;
        Ok(hubs
            .into_iter()
            .map(|(nid, deg)| FfiHubRecord {
                node_id: nid.0,
                degree: deg as u64,
            })
            .collect())
    }

    /// Recursively cascade-deletes `root` and all descendants reachable via `containment_edge`.
    pub fn cascade_delete(&self, root: u64, containment_edge: String) -> Result<Vec<u64>, FfiBknError> {
        let deleted = dispatch!(self, db => db.graph().cascade_delete(NodeId(root), &containment_edge))?;
        Ok(deleted.into_iter().map(|n| n.0).collect())
    }

    /// Ingests a structured batch of graph nodes and edges in a single atomic transaction.
    pub fn sync_batch(&self, batch: FfiSyncBatch) -> Result<FfiSyncResult, FfiBknError> {
        let mut core_batch = bkndb_core::db::SyncBatch::new();
        for n in batch.nodes {
            core_batch.add_node(n.label, ffi_props_to_core(n.properties));
        }
        for e in batch.edges {
            core_batch.add_edge(
                NodeId(e.from),
                e.edge_type,
                NodeId(e.to),
                ffi_props_to_core(e.properties),
            );
        }
        let res = dispatch!(self, db => db.sync_batch(core_batch))?;
        Ok(FfiSyncResult {
            node_ids: res.node_ids.into_iter().map(|n| n.0).collect(),
            edge_ids: res.edge_ids.into_iter().map(|e| e.0).collect(),
        })
    }
}
