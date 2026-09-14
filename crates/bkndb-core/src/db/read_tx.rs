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
