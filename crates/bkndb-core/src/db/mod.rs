//! `Db<B>`: one shared backend, three facades. Replaces manually calling
//! `GraphDb::from_arc`/`RelationalDb::from_arc` on the same cloned
//! `Arc<B>` — this type is exactly that pattern, wrapped once.

mod batch;
mod read_tx;
mod write_tx;
#[cfg(all(feature = "graph", feature = "relational"))]
mod sync;

pub use batch::DbWriteBatch;
pub use read_tx::DbReadBatch;
#[cfg(all(feature = "graph", feature = "relational"))]
pub use sync::{SyncBatch, SyncBatchResult};

use std::sync::Arc;

use crate::{kv::Kv, StorageBackend};

pub struct Db<B: StorageBackend> {
    backend: Arc<B>,
}

impl<B: StorageBackend> Clone for Db<B> {
    fn clone(&self) -> Self {
        Self {
            backend: self.backend.clone(),
        }
    }
}

impl<B: StorageBackend> Db<B> {
    pub fn new(backend: B) -> Self {
        Self::from_arc(Arc::new(backend))
    }

    pub fn from_arc(backend: Arc<B>) -> Self {
        Self { backend }
    }

    /// Raw KV access, validated against [`crate::RESERVED_TABLE_NAMES`].
    /// Always available — needs neither the "graph" nor "relational" feature.
    /// The storage backend this database runs on (e.g. for backend-specific
    /// maintenance such as compaction).
    pub fn backend(&self) -> &B {
        &self.backend
    }

    /// Opens an explicit write transaction spanning every model. Nothing is
    /// durable until [`DbWriteBatch::commit`]; dropping the batch rolls it
    /// back. Only one write transaction can be open at a time — the next
    /// `begin_write` (or any other write) waits until this one ends.
    /// Prefer [`Db::write_tx`] when the whole transaction fits in a closure.
    pub fn begin_write(&self) -> Result<DbWriteBatch<B::WriteTx<'_>>, crate::BknError> {
        Ok(DbWriteBatch::new(self.backend.begin_write()?))
    }

    pub fn kv(&self) -> Kv<B> {
        Kv::from_arc(self.backend.clone())
    }

    #[cfg(feature = "graph")]
    pub fn graph(&self) -> crate::graph::GraphDb<B> {
        crate::graph::GraphDb::from_arc(self.backend.clone())
    }

    #[cfg(feature = "relational")]
    pub fn relational(&self) -> crate::relational::RelationalDb<B> {
        crate::relational::RelationalDb::from_arc(self.backend.clone())
    }
}
