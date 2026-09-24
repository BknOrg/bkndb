use crate::graph::{EdgeId, NodeId};
use crate::relational::RelSchema;
use crate::value::{PropValue, Properties};
use crate::{BknError, StorageBackend};
use crate::db::Db;

/// A structured batch of graph nodes, graph edges, and relational rows
/// designed for high-performance atomic synchronization (e.g. codebase-recall indexing).
#[derive(Default, Debug, Clone)]
pub struct SyncBatch<'a> {
    pub nodes: Vec<(String, Properties)>,
    pub edges: Vec<(NodeId, String, NodeId, Properties)>,
    pub relational_rows: Vec<(&'a RelSchema, Vec<Properties>)>,
    pub relational_rows_with_pk: Vec<(&'a RelSchema, Vec<(PropValue, Properties)>)>,
}

/// The result of executing a [`SyncBatch`], returning the sequential IDs
/// and primary keys generated during the atomic transaction.
#[derive(Default, Debug, Clone, PartialEq)]
pub struct SyncBatchResult {
    pub node_ids: Vec<NodeId>,
    pub edge_ids: Vec<EdgeId>,
    pub relational_pks: Vec<Vec<PropValue>>,
}

impl<'a> SyncBatch<'a> {
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

    /// Adds a relational row for an auto-increment or implicit PK schema.
    pub fn add_row(&mut self, schema: &'a RelSchema, values: Properties) -> &mut Self {
        if let Some((s, rows)) = self.relational_rows.last_mut() {
            if std::ptr::eq(*s, schema) || s.name == schema.name {
                rows.push(values);
                return self;
            }
        }
        self.relational_rows.push((schema, vec![values]));
        self
    }

    /// Adds multiple relational rows for an auto-increment or implicit PK schema.
    pub fn add_rows(&mut self, schema: &'a RelSchema, rows: impl IntoIterator<Item = Properties>) -> &mut Self {
        let items: Vec<Properties> = rows.into_iter().collect();
        if !items.is_empty() {
            if let Some((s, existing)) = self.relational_rows.last_mut() {
                if std::ptr::eq(*s, schema) || s.name == schema.name {
                    existing.extend(items);
                    return self;
                }
            }
            self.relational_rows.push((schema, items));
        }
        self
    }

    /// Adds a relational row with an explicit caller-specified PK (e.g. NodeId).
    pub fn add_row_with_pk(&mut self, schema: &'a RelSchema, pk: PropValue, values: Properties) -> &mut Self {
        if let Some((s, rows)) = self.relational_rows_with_pk.last_mut() {
            if std::ptr::eq(*s, schema) || s.name == schema.name {
                rows.push((pk, values));
                return self;
            }
        }
        self.relational_rows_with_pk.push((schema, vec![(pk, values)]));
        self
    }

    /// Adds multiple relational rows with explicit caller-specified PKs.
    pub fn add_rows_with_pk(
        &mut self,
        schema: &'a RelSchema,
        rows: impl IntoIterator<Item = (PropValue, Properties)>,
    ) -> &mut Self {
        let items: Vec<(PropValue, Properties)> = rows.into_iter().collect();
        if !items.is_empty() {
            if let Some((s, existing)) = self.relational_rows_with_pk.last_mut() {
                if std::ptr::eq(*s, schema) || s.name == schema.name {
                    existing.extend(items);
                    return self;
                }
            }
            self.relational_rows_with_pk.push((schema, items));
        }
        self
    }
}

impl<B: StorageBackend> Db<B> {
    /// Executes a [`SyncBatch`] within a single ACID write transaction using bulk primitives.
    /// Reserves sequential node, edge, and relational PKs with minimal counter updates.
    pub fn sync_batch<'a>(&self, batch: SyncBatch<'a>) -> Result<SyncBatchResult, BknError> {
        self.write_tx(|wbatch| {
            let mut graph = wbatch.graph();
            let node_ids = graph.create_nodes_bulk(batch.nodes)?;
            let edge_ids = graph.create_edges_bulk(batch.edges)?;
            drop(graph);

            let mut relational = wbatch.relational();
            let mut relational_pks = Vec::with_capacity(batch.relational_rows.len());
            for (schema, rows) in batch.relational_rows {
                let mut tbl = relational.table(schema);
                let pks = tbl.insert_bulk(rows)?;
                relational_pks.push(pks);
            }

            for (schema, rows) in batch.relational_rows_with_pk {
                let mut tbl = relational.table(schema);
                tbl.insert_with_pk_bulk(rows)?;
            }

            Ok(SyncBatchResult {
                node_ids,
                edge_ids,
                relational_pks,
            })
        })
    }
}
