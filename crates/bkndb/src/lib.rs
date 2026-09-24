pub use bkndb_core::*;

#[cfg(feature = "lsm-backend")]
pub use bkndb_storage_lsm::{LsmOptions, LsmStorageBackend};

#[cfg(feature = "redb-backend")]
pub use bkndb_storage_redb::RedbStorageBackend;

#[cfg(feature = "mem-backend")]
pub use bkndb_storage_mem::MemoryStorageBackend;

use std::path::Path;

/// The primary user-facing database handle for BknDb, backed by the native
/// [`LsmStorageBackend`] for high-performance single-file on-disk persistence (.bkndb).
#[cfg(feature = "lsm-backend")]
#[derive(Clone)]
pub struct BknDb {
    inner: Db<LsmStorageBackend>,
}

#[cfg(feature = "lsm-backend")]
impl BknDb {
    /// Opens or creates a single-file native BknDb database (`.bkndb`) at `path`.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, BknError> {
        let backend = LsmStorageBackend::open(path)?;
        Ok(Self {
            inner: Db::new(backend),
        })
    }

    /// Opens or creates a single-file native BknDb database with custom LSM options.
    pub fn open_with_options<P: AsRef<Path>>(path: P, options: LsmOptions) -> Result<Self, BknError> {
        let backend = LsmStorageBackend::open_with_options(path, options)?;
        Ok(Self {
            inner: Db::new(backend),
        })
    }

    /// Opens or creates an alternative on-disk database backed by Redb.
    #[cfg(feature = "redb-backend")]
    pub fn open_redb<P: AsRef<Path>>(path: P) -> Result<Db<RedbStorageBackend>, BknError> {
        let backend = RedbStorageBackend::open(path)?;
        Ok(Db::new(backend))
    }

    /// Creates an ephemeral in-memory database instance for testing or caching.
    #[cfg(feature = "mem-backend")]
    pub fn in_memory() -> Db<MemoryStorageBackend> {
        Db::new(MemoryStorageBackend::new())
    }

    /// Access the underlying [`Db<LsmStorageBackend>`].
    pub fn inner(&self) -> &Db<LsmStorageBackend> {
        &self.inner
    }

    /// Convenience helper for joining a list of graph node IDs with a relational table
    /// whose primary key is the node ID.
    #[cfg(all(feature = "graph", feature = "relational-layer"))]
    pub fn join_nodes_with_table(
        &self,
        nodes: &[bkndb_core::graph::NodeId],
        schema: &bkndb_core::relational::RelSchema,
    ) -> Result<Vec<bkndb_core::hybrid::JoinedNode>, BknError> {
        self.read_tx(|tx| tx.join_nodes_with_table(nodes, schema))
    }

    /// Convenience helper for joining a list of graph node IDs with a relational table
    /// by a foreign-key integer column.
    #[cfg(all(feature = "graph", feature = "relational-layer"))]
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
    #[cfg(feature = "graph")]
    pub fn sync_batch<'a>(&self, batch: SyncBatch<'a>) -> Result<SyncBatchResult, BknError> {
        self.inner.sync_batch(batch)
    }
}

#[cfg(feature = "lsm-backend")]
impl std::ops::Deref for BknDb {
    type Target = Db<LsmStorageBackend>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

/// Fallback BknDb when lsm-backend is disabled but redb-backend is enabled.
#[cfg(all(not(feature = "lsm-backend"), feature = "redb-backend"))]
#[derive(Clone)]
pub struct BknDb {
    inner: Db<RedbStorageBackend>,
}

#[cfg(all(not(feature = "lsm-backend"), feature = "redb-backend"))]
impl BknDb {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, BknError> {
        let backend = RedbStorageBackend::open(path)?;
        Ok(Self {
            inner: Db::new(backend),
        })
    }

    #[cfg(feature = "mem-backend")]
    pub fn in_memory() -> Db<MemoryStorageBackend> {
        Db::new(MemoryStorageBackend::new())
    }

    pub fn inner(&self) -> &Db<RedbStorageBackend> {
        &self.inner
    }

    #[cfg(all(feature = "graph", feature = "relational-layer"))]
    pub fn join_nodes_with_table(
        &self,
        nodes: &[bkndb_core::graph::NodeId],
        schema: &bkndb_core::relational::RelSchema,
    ) -> Result<Vec<bkndb_core::hybrid::JoinedNode>, BknError> {
        self.read_tx(|tx| tx.join_nodes_with_table(nodes, schema))
    }

    #[cfg(all(feature = "graph", feature = "relational-layer"))]
    pub fn join_nodes_by_column(
        &self,
        nodes: &[bkndb_core::graph::NodeId],
        schema: &bkndb_core::relational::RelSchema,
        foreign_key_col: &str,
    ) -> Result<Vec<bkndb_core::hybrid::JoinedNodeRows>, BknError> {
        self.read_tx(|tx| tx.join_nodes_by_column(nodes, schema, foreign_key_col))
    }

    #[cfg(feature = "graph")]
    pub fn sync_batch<'a>(&self, batch: SyncBatch<'a>) -> Result<SyncBatchResult, BknError> {
        self.inner.sync_batch(batch)
    }
}

#[cfg(all(not(feature = "lsm-backend"), feature = "redb-backend"))]
impl std::ops::Deref for BknDb {
    type Target = Db<RedbStorageBackend>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

#[cfg(all(not(feature = "lsm-backend"), not(feature = "redb-backend"), feature = "mem-backend"))]
pub struct BknDb;

#[cfg(all(not(feature = "lsm-backend"), not(feature = "redb-backend"), feature = "mem-backend"))]
impl BknDb {
    /// Creates an ephemeral in-memory database instance for testing or caching.
    pub fn in_memory() -> Db<MemoryStorageBackend> {
        Db::new(MemoryStorageBackend::new())
    }
}
