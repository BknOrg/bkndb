//! `Db<B>`: one shared backend, three facades. Replaces manually calling
//! `GraphDb::from_arc`/`RelationalDb::from_arc` on the same cloned
//! `Arc<B>` — this type is exactly that pattern, wrapped once.

mod batch;
mod read_tx;
mod write_tx;

pub use batch::DbWriteBatch;
pub use read_tx::DbReadBatch;

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
