pub use bkndb_core::*;

#[cfg(feature = "redb-backend")]
pub use bkndb_storage_redb::{IntegrityReport, RedbStats, RedbStorageBackend};

#[cfg(feature = "lsm-backend")]
pub use bkndb_storage_lsm::{
    IntegrityReport as LsmIntegrityReport, LsmOptions, LsmStats, LsmStorageBackend,
};

#[cfg(all(not(feature = "redb-backend"), feature = "lsm-backend"))]
pub use bkndb_storage_lsm::IntegrityReport;

#[cfg(feature = "mem-backend")]
pub use bkndb_storage_mem::MemoryStorageBackend;

use std::path::Path;

macro_rules! impl_bkndb_common {
    ($backend:ty, $stats:ty) => {
        /// Creates an ephemeral in-memory database instance for testing or caching.
        #[cfg(feature = "mem-backend")]
        pub fn in_memory() -> Db<MemoryStorageBackend> {
            Db::new(MemoryStorageBackend::new())
        }

        /// Access the underlying [`Db<$backend>`].
        pub fn inner(&self) -> &Db<$backend> {
            &self.inner
        }

        /// Compacts the on-disk database and reclaims free space.
        pub fn compact(&self) -> Result<(), BknError> {
            self.inner.backend().force_compact()
        }

        /// Writes a consistent snapshot copy of everything committed so far to
        /// a new `.bkndb` file at `dest` (which must not exist yet), while the
        /// database stays open for reads and writes.
        pub fn backup_to<P: AsRef<Path>>(&self, dest: P) -> Result<(), BknError> {
            self.inner.backend().backup_to(dest)
        }

        /// File-level and table storage statistics. For logical counts (nodes,
        /// edges, rows) see [`Db::stats`].
        pub fn storage_stats(&self) -> Result<$stats, BknError> {
            self.inner.backend().stats()
        }

        /// Verifies database integrity and scans all tables/entries; returns
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
    };
}

macro_rules! impl_deref_for_bkndb {
    ($backend:ty) => {
        impl std::ops::Deref for BknDb {
            type Target = Db<$backend>;

            fn deref(&self) -> &Self::Target {
                &self.inner
            }
        }

        impl std::ops::DerefMut for BknDb {
            fn deref_mut(&mut self) -> &mut Self::Target {
                &mut self.inner
            }
        }
    };
}

/// The primary user-facing database handle for BknDb, backed by
/// [`RedbStorageBackend`] for single-file ACID on-disk persistence (`.bkndb`).
#[cfg(feature = "redb-backend")]
#[derive(Clone)]
pub struct BknDb {
    inner: Db<RedbStorageBackend>,
}

#[cfg(feature = "redb-backend")]
impl BknDb {
    /// Opens or creates a single-file BknDb database (`.bkndb`) at `path`
    /// backed by [`RedbStorageBackend`].
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, BknError> {
        let backend = RedbStorageBackend::open(path)?;
        Ok(Self {
            inner: Db::new(backend),
        })
    }

    /// Compatibility alias for [`BknDb::open`] backed by [`RedbStorageBackend`].
    pub fn open_redb<P: AsRef<Path>>(path: P) -> Result<Self, BknError> {
        Self::open(path)
    }

    /// Opens or creates an on-disk database backed by [`LsmStorageBackend`].
    #[cfg(feature = "lsm-backend")]
    pub fn open_lsm<P: AsRef<Path>>(path: P) -> Result<Db<LsmStorageBackend>, BknError> {
        let backend = LsmStorageBackend::open(path)?;
        Ok(Db::new(backend))
    }

    /// Opens or creates an on-disk database backed by [`LsmStorageBackend`]
    /// with custom [`LsmOptions`].
    #[cfg(feature = "lsm-backend")]
    pub fn open_lsm_with_options<P: AsRef<Path>>(
        path: P,
        options: LsmOptions,
    ) -> Result<Db<LsmStorageBackend>, BknError> {
        let backend = LsmStorageBackend::open_with_options(path, options)?;
        Ok(Db::new(backend))
    }

    impl_bkndb_common!(RedbStorageBackend, RedbStats);
}

#[cfg(feature = "redb-backend")]
impl_deref_for_bkndb!(RedbStorageBackend);

/// Fallback BknDb when redb-backend is disabled but lsm-backend is enabled.
#[cfg(all(not(feature = "redb-backend"), feature = "lsm-backend"))]
#[derive(Clone)]
pub struct BknDb {
    inner: Db<LsmStorageBackend>,
}

#[cfg(all(not(feature = "redb-backend"), feature = "lsm-backend"))]
impl BknDb {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, BknError> {
        let backend = LsmStorageBackend::open(path)?;
        Ok(Self {
            inner: Db::new(backend),
        })
    }

    pub fn open_with_options<P: AsRef<Path>>(path: P, options: LsmOptions) -> Result<Self, BknError> {
        let backend = LsmStorageBackend::open_with_options(path, options)?;
        Ok(Self {
            inner: Db::new(backend),
        })
    }

    pub fn open_lsm<P: AsRef<Path>>(path: P) -> Result<Db<LsmStorageBackend>, BknError> {
        let backend = LsmStorageBackend::open(path)?;
        Ok(Db::new(backend))
    }

    pub fn open_lsm_with_options<P: AsRef<Path>>(
        path: P,
        options: LsmOptions,
    ) -> Result<Db<LsmStorageBackend>, BknError> {
        let backend = LsmStorageBackend::open_with_options(path, options)?;
        Ok(Db::new(backend))
    }

    impl_bkndb_common!(LsmStorageBackend, LsmStats);
}

#[cfg(all(not(feature = "redb-backend"), feature = "lsm-backend"))]
impl_deref_for_bkndb!(LsmStorageBackend);

#[cfg(all(not(feature = "lsm-backend"), not(feature = "redb-backend"), feature = "mem-backend"))]
pub struct BknDb;

#[cfg(all(not(feature = "lsm-backend"), not(feature = "redb-backend"), feature = "mem-backend"))]
impl BknDb {
    /// Creates an ephemeral in-memory database instance for testing or caching.
    pub fn in_memory() -> Db<MemoryStorageBackend> {
        Db::new(MemoryStorageBackend::new())
    }
}
