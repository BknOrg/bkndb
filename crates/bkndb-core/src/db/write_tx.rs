//! [`Db::write_tx`] — the cross-model atomic transaction entry point.
//! Deliberately its own file, separate from [`crate::db::Db`]'s plain
//! accessors, so `db/mod.rs` stays a small, single-purpose "what facades
//! does `Db` expose" file.

use crate::db::{batch::DbWriteBatch, Db};
use crate::{BknError, StorageBackend, StorageWriteTx};

impl<B: StorageBackend> Db<B> {
    /// Runs `f` against one shared write transaction that can touch graph,
    /// relational, and raw KV together, committing only if `f` returns
    /// `Ok` — the same open/wrap/run/commit shape already proven by
    /// [`crate::relational::RelationalDb::write_tx`] and
    /// [`crate::graph::GraphDb::write_tx`], applied a third time.
    pub fn write_tx<F, R>(&self, f: F) -> Result<R, BknError>
    where
        F: for<'w> FnOnce(&mut DbWriteBatch<B::WriteTx<'w>>) -> Result<R, BknError>,
    {
        let wtx = self.backend.begin_write()?;
        let mut batch = DbWriteBatch::new(wtx);
        let result = f(&mut batch)?;
        batch.into_inner().commit()?;
        Ok(result)
    }
}
