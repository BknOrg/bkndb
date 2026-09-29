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
pub use sync::{NodeRef, SyncBatch, SyncBatchResult};

use std::ops::Bound;
use std::sync::Arc;

use crate::{kv::Kv, BknError, StorageBackend, StorageReadTx, TableSpec};

/// Logical size of a database — see [`Db::stats`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DbStats {
    /// Graph nodes (0 without the `graph` feature).
    pub nodes: u64,
    /// Graph edges (0 without the `graph` feature).
    pub edges: u64,
    /// `(name, row count)` of every relational table registered in the
    /// catalog, by name.
    pub tables: Vec<(String, u64)>,
}

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

    /// Counts nodes, edges and the rows of every registered table, all from
    /// one read snapshot. Streams through each table (O(rows) time, O(1)
    /// memory) — meant for monitoring and tooling, not hot paths.
    pub fn stats(&self) -> Result<DbStats, BknError> {
        let rtx = self.backend.begin_read()?;
        let count = |table: TableSpec| -> Result<u64, BknError> {
            let mut n = 0;
            for kv in rtx.scan(table, Bound::Unbounded, Bound::Unbounded)? {
                kv?;
                n += 1;
            }
            Ok(n)
        };
        #[allow(unused_mut)]
        let mut stats = DbStats::default();
        #[cfg(feature = "graph")]
        {
            stats.nodes = count(crate::graph::codec::NODES)?;
            stats.edges = count(crate::graph::codec::EDGES)?;
        }
        #[cfg(feature = "relational")]
        for schema in crate::relational::catalog::list_schemas_in(&rtx)? {
            stats.tables.push((schema.name().to_string(), count(schema.base_table())?));
        }
        #[cfg(not(any(feature = "graph", feature = "relational")))]
        let _ = count;
        Ok(stats)
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
