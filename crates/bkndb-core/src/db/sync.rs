use crate::graph::{EdgeId, NodeId};
use crate::relational::TableSchema;
use crate::value::{PropValue, Properties};
use crate::{BknError, StorageBackend};
use crate::db::Db;

/// A structured batch of graph nodes, graph edges, and relational rows
/// designed for high-performance atomic synchronization (e.g. codebase-recall indexing).
///
/// Relational rows are **upserted**: re-syncing a row whose primary key
/// already exists replaces it (secondary indexes stay consistent) instead of
/// failing, so the same batch can be applied repeatedly.
#[derive(Default, Debug, Clone)]
pub struct SyncBatch {
    pub nodes: Vec<(String, Properties)>,
    pub edges: Vec<(NodeId, String, NodeId, Properties)>,
    pub relational_rows: Vec<(TableSchema, Vec<Properties>)>,
    pub relational_rows_with_pk: Vec<(TableSchema, Vec<(PropValue, Properties)>)>,
}

/// Appends `items` to the last group if it's for the same table, otherwise
/// starts a new group — keeps consecutive rows for one table in one bulk call.
fn push_grouped<T>(groups: &mut Vec<(TableSchema, Vec<T>)>, schema: TableSchema, items: Vec<T>) {
    if items.is_empty() {
        return;
    }
    match groups.last_mut() {
        Some((s, existing)) if *s == schema => existing.extend(items),
        _ => groups.push((schema, items)),
    }
}

/// The result of executing a [`SyncBatch`], returning the sequential IDs
/// and primary keys generated during the atomic transaction.
#[derive(Default, Debug, Clone, PartialEq)]
pub struct SyncBatchResult {
    pub node_ids: Vec<NodeId>,
    pub edge_ids: Vec<EdgeId>,
    pub relational_pks: Vec<Vec<PropValue>>,
}

impl SyncBatch {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a single graph node to the batch.
    pub fn add_node(&mut self, label: impl Into<String>, properties: Properties) -> &mut Self {
        self.nodes.push((label.into(), properties));
        self
    }

    /// Adds multiple graph nodes to the batch.
    pub fn add_nodes(&mut self, nodes: impl IntoIterator<Item = (impl Into<String>, Properties)>) -> &mut Self {
        self.nodes.extend(nodes.into_iter().map(|(l, p)| (l.into(), p)));
        self
    }

    /// Adds a single graph edge to the batch.
    pub fn add_edge(
        &mut self,
        from: NodeId,
        edge_type: impl Into<String>,
        to: NodeId,
        properties: Properties,
    ) -> &mut Self {
        self.edges.push((from, edge_type.into(), to, properties));
        self
    }

    /// Adds multiple graph edges to the batch.
    pub fn add_edges(
        &mut self,
        edges: impl IntoIterator<Item = (NodeId, impl Into<String>, NodeId, Properties)>,
    ) -> &mut Self {
        self.edges.extend(edges.into_iter().map(|(f, t, to, p)| (f, t.into(), to, p)));
        self
    }

    /// Adds a relational row. On an auto-increment schema a row without a
    /// pk gets a fresh one; a row that carries its pk is upserted.
    pub fn add_row(&mut self, schema: impl Into<TableSchema>, values: Properties) -> &mut Self {
        push_grouped(&mut self.relational_rows, schema.into(), vec![values]);
        self
    }

    /// Adds multiple relational rows (see [`SyncBatch::add_row`]).
    pub fn add_rows(&mut self, schema: impl Into<TableSchema>, rows: impl IntoIterator<Item = Properties>) -> &mut Self {
        push_grouped(&mut self.relational_rows, schema.into(), rows.into_iter().collect());
        self
    }

    /// Adds a relational row with an explicit caller-specified PK (e.g. NodeId).
    pub fn add_row_with_pk(&mut self, schema: impl Into<TableSchema>, pk: PropValue, values: Properties) -> &mut Self {
        push_grouped(&mut self.relational_rows_with_pk, schema.into(), vec![(pk, values)]);
        self
    }

    /// Adds multiple relational rows with explicit caller-specified PKs.
    pub fn add_rows_with_pk(
        &mut self,
        schema: impl Into<TableSchema>,
        rows: impl IntoIterator<Item = (PropValue, Properties)>,
    ) -> &mut Self {
        push_grouped(&mut self.relational_rows_with_pk, schema.into(), rows.into_iter().collect());
        self
    }
}

impl<B: StorageBackend> Db<B> {
    /// Executes a [`SyncBatch`] within a single ACID write transaction using bulk primitives.
    /// Reserves sequential node, edge, and relational PKs with minimal counter updates.
    /// Relational rows are upserted (see [`SyncBatch`]).
    pub fn sync_batch(&self, batch: SyncBatch) -> Result<SyncBatchResult, BknError> {
        self.write_tx(|wbatch| {
            let mut graph = wbatch.graph();
            let node_ids = graph.create_nodes_bulk(batch.nodes)?;
            let edge_ids = graph.create_edges_bulk(batch.edges)?;
            drop(graph);

            let mut relational = wbatch.relational();
            let mut relational_pks = Vec::with_capacity(batch.relational_rows.len());
            for (schema, rows) in batch.relational_rows {
                let mut tbl = relational.table(schema);
                let pks = tbl.upsert_bulk(rows)?;
                relational_pks.push(pks);
            }

            for (schema, rows) in batch.relational_rows_with_pk {
                let mut tbl = relational.table(schema);
                tbl.upsert_with_pk_bulk(rows)?;
            }

            Ok(SyncBatchResult {
                node_ids,
                edge_ids,
                relational_pks,
            })
        })
    }
}
