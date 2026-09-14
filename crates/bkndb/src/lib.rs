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

