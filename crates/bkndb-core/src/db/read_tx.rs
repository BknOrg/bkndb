//! [`Db::read_tx`] — the cross-model consistent read transaction entry point.
//! Owns one read tx spanning graph, relational, and raw KV together over the
//! same storage snapshot, eliminating repeated `begin_read()` calls.

use crate::kv::txn::ReadKv;
use crate::{BknError, StorageBackend, StorageReadTx};

pub struct DbReadBatch<R: StorageReadTx> {
    rtx: R,
}

impl<R: StorageReadTx> DbReadBatch<R> {
    pub(crate) fn new(rtx: R) -> Self {
        Self { rtx }
    }

    /// Access raw KV storage validated against [`crate::RESERVED_TABLE_NAMES`].
    pub fn kv(&self) -> ReadKv<'_, R> {
        ReadKv::new(&self.rtx)
    }

    #[cfg(feature = "graph")]
    pub fn graph(&self) -> crate::graph::txn::ReadGraph<'_, R> {
        crate::graph::txn::ReadGraph::new(&self.rtx)
    }

    #[cfg(feature = "relational")]
    pub fn relational(&self) -> crate::relational::RelReadView<'_, R> {
        crate::relational::RelReadView::new(&self.rtx)
    }

    #[cfg(all(feature = "graph", feature = "relational"))]
    pub fn join_nodes_with_table(
        &self,
        nodes: &[crate::graph::NodeId],
        schema: impl Into<crate::relational::TableSchema>,
    ) -> Result<Vec<crate::hybrid::JoinedNode>, BknError> {
        crate::hybrid::join_nodes_with_table(&self.rtx, nodes, &crate::relational::db::resolve_schema(&self.rtx, schema.into())?)
    }

    #[cfg(all(feature = "graph", feature = "relational"))]
    pub fn join_nodes_by_column(
        &self,
        nodes: &[crate::graph::NodeId],
        schema: impl Into<crate::relational::TableSchema>,
        foreign_key_col: &str,
    ) -> Result<Vec<crate::hybrid::JoinedNodeRows>, BknError> {
        crate::hybrid::join_nodes_by_column(&self.rtx, nodes, &crate::relational::db::resolve_schema(&self.rtx, schema.into())?, foreign_key_col)
    }

    #[cfg(all(feature = "graph", feature = "relational"))]
    pub fn join_rows_with_nodes(
        &self,
        rows: &[crate::relational::Row],
        schema: impl Into<crate::relational::TableSchema>,
        node_id_column: &str,
    ) -> Result<Vec<(crate::relational::Row, Option<crate::graph::NodeRecord>)>, BknError> {
        crate::hybrid::join_rows_with_nodes(&self.rtx, rows, &crate::relational::db::resolve_schema(&self.rtx, schema.into())?, node_id_column)
    }

    /// Rows matching `query`, each paired with the node its `node_id_column`
    /// references. See [`crate::hybrid::query_rows_with_nodes`].
    #[cfg(all(feature = "graph", feature = "relational"))]
    pub fn query_rows_with_nodes(
        &self,
        schema: impl Into<crate::relational::TableSchema>,
        query: &crate::relational::Query,
        node_id_column: &str,
    ) -> Result<Vec<(crate::relational::Row, Option<crate::graph::NodeRecord>)>, BknError> {
        crate::hybrid::query_rows_with_nodes(&self.rtx, &crate::relational::db::resolve_schema(&self.rtx, schema.into())?, query, node_id_column)
    }
}

impl<B: StorageBackend> crate::db::Db<B> {
    /// Runs `f` against one shared, consistent snapshot read transaction that
    /// can query graph, relational, and raw KV together.
    pub fn read_tx<F, Res>(&self, f: F) -> Result<Res, BknError>
    where
        F: for<'r> FnOnce(&DbReadBatch<B::ReadTx<'r>>) -> Result<Res, BknError>,
    {
        let rtx = self.backend.begin_read()?;
        let batch = DbReadBatch::new(rtx);
        f(&batch)
    }
}
