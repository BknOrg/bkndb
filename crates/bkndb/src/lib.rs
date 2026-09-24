pub use bkndb_core::*;

#[cfg(feature = "redb-backend")]
pub use bkndb_storage_redb::RedbStorageBackend;

#[cfg(feature = "mem-backend")]
pub use bkndb_storage_mem::MemoryStorageBackend;

#[cfg(feature = "lsm-backend")]
pub use bkndb_storage_lsm::{LsmOptions, LsmStorageBackend};

#[cfg(feature = "redb-backend")]
use std::path::Path;

/// The primary user-facing database handle for BknDb, backed by
/// [`RedbStorageBackend`] for ACID single-file on-disk persistence.
#[cfg(feature = "redb-backend")]
#[derive(Clone)]
pub struct BknDb {
    inner: Db<RedbStorageBackend>,
}

#[cfg(feature = "redb-backend")]
impl BknDb {
    /// Opens or creates a single-file BknDb database at `path`.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, BknError> {
        let backend = RedbStorageBackend::open(path)?;
        Ok(Self {
            inner: Db::new(backend),
        })
    }

    /// Creates an ephemeral in-memory database instance for testing or caching.
    #[cfg(feature = "mem-backend")]
    pub fn in_memory() -> Db<MemoryStorageBackend> {
        Db::new(MemoryStorageBackend::new())
    }

    /// Access the underlying [`Db<RedbStorageBackend>`].
    pub fn inner(&self) -> &Db<RedbStorageBackend> {
        &self.inner
    }

    /// Convenience helper for joining a list of graph node IDs with a relational table
    /// whose primary key is the node ID.
    pub fn join_nodes_with_table(
        &self,
        nodes: &[bkndb_core::graph::NodeId],
        schema: &bkndb_core::relational::RelSchema,
    ) -> Result<Vec<bkndb_core::hybrid::JoinedNode>, BknError> {
        self.read_tx(|tx| tx.join_nodes_with_table(nodes, schema))
    }

    /// Convenience helper for joining a list of graph node IDs with a relational table
    /// by a foreign-key integer column.
    pub fn join_nodes_by_column(
        &self,
        nodes: &[bkndb_core::graph::NodeId],
        schema: &bkndb_core::relational::RelSchema,
        foreign_key_col: &str,
    ) -> Result<Vec<bkndb_core::hybrid::JoinedNodeRows>, BknError> {
        self.read_tx(|tx| tx.join_nodes_by_column(nodes, schema, foreign_key_col))
    }

    /// Ingests a structured batch of graph nodes, edges, and relational rows
    /// in a single atomic transaction using optimized bulk primitives.
    pub fn sync_batch<'a>(&self, batch: SyncBatch<'a>) -> Result<SyncBatchResult, BknError> {
        self.inner.sync_batch(batch)
    }
}

#[cfg(feature = "redb-backend")]
impl std::ops::Deref for BknDb {
    type Target = Db<RedbStorageBackend>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

#[cfg(all(not(feature = "redb-backend"), feature = "mem-backend"))]
pub struct BknDb;

#[cfg(all(not(feature = "redb-backend"), feature = "mem-backend"))]
impl BknDb {
    /// Creates an ephemeral in-memory database instance for testing or caching.
    pub fn in_memory() -> Db<MemoryStorageBackend> {
        Db::new(MemoryStorageBackend::new())
    }
}

