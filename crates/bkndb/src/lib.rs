pub use bkndb_core::*;

#[cfg(feature = "lsm-backend")]
pub use bkndb_storage_lsm::{IntegrityReport, LsmOptions, LsmStats, LsmStorageBackend};

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

    /// Merges every on-disk SSTable into one and reclaims dead space
    /// (overwritten/deleted data and old log regions). Blocks writers while
    /// it runs; readers are unaffected.
    pub fn compact(&self) -> Result<(), BknError> {
        self.inner.backend().force_compact()
    }

    /// Writes a consistent, compacted copy of everything committed so far to
    /// a new `.bkndb` file at `dest` (which must not exist yet), while the
    /// database stays open for reads and writes.
    pub fn backup_to<P: AsRef<Path>>(&self, dest: P) -> Result<(), BknError> {
        self.inner.backend().backup_to(dest)
    }

    /// File-level size figures (file size, SSTables, WAL, reclaimable
    /// space). For logical counts (nodes, edges, rows) see [`Db::stats`].
    pub fn storage_stats(&self) -> Result<LsmStats, BknError> {
        self.inner.backend().stats()
    }

    /// Re-reads and checksums every stored byte; returns
    /// [`BknError::Corruption`] if anything is damaged.
    pub fn verify_integrity(&self) -> Result<IntegrityReport, BknError> {
        self.inner.backend().verify_integrity()
    }

    /// Convenience helper for joining a list of graph node IDs with a relational table
    /// whose primary key is the node ID.
    #[cfg(all(feature = "graph", feature = "relational-layer"))]
    pub fn join_nodes_with_table(
        &self,
        nodes: &[bkndb_core::graph::NodeId],
        schema: impl Into<bkndb_core::relational::TableSchema>,
    ) -> Result<Vec<bkndb_core::hybrid::JoinedNode>, BknError> {
        self.read_tx(|tx| tx.join_nodes_with_table(nodes, schema))
    }

    /// Convenience helper for joining a list of graph node IDs with a relational table
    /// by a foreign-key integer column.
    #[cfg(all(feature = "graph", feature = "relational-layer"))]
    pub fn join_nodes_by_column(
        &self,
        nodes: &[bkndb_core::graph::NodeId],
        schema: impl Into<bkndb_core::relational::TableSchema>,
        foreign_key_col: &str,
    ) -> Result<Vec<bkndb_core::hybrid::JoinedNodeRows>, BknError> {
        self.read_tx(|tx| tx.join_nodes_by_column(nodes, schema, foreign_key_col))
    }

    /// Ingests a structured batch of graph nodes, edges, and relational rows
    /// in a single atomic transaction using optimized bulk primitives.
    #[cfg(all(feature = "graph", feature = "relational-layer"))]
    pub fn sync_batch(&self, batch: SyncBatch) -> Result<SyncBatchResult, BknError> {
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
        schema: impl Into<bkndb_core::relational::TableSchema>,
    ) -> Result<Vec<bkndb_core::hybrid::JoinedNode>, BknError> {
        self.read_tx(|tx| tx.join_nodes_with_table(nodes, schema))
    }

    #[cfg(all(feature = "graph", feature = "relational-layer"))]
    pub fn join_nodes_by_column(
        &self,
        nodes: &[bkndb_core::graph::NodeId],
        schema: impl Into<bkndb_core::relational::TableSchema>,
        foreign_key_col: &str,
    ) -> Result<Vec<bkndb_core::hybrid::JoinedNodeRows>, BknError> {
        self.read_tx(|tx| tx.join_nodes_by_column(nodes, schema, foreign_key_col))
    }

    #[cfg(all(feature = "graph", feature = "relational-layer"))]
    pub fn sync_batch(&self, batch: SyncBatch) -> Result<SyncBatchResult, BknError> {
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
