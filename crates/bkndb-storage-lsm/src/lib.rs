//! BknDb adapter for the independent `bkndb-engine` storage library.
use bkndb_core::{
    BknError, KvIter, KvPairs, StorageBackend, StorageReadTx, StorageWriteTx, TableSpec,
};
pub use bkndb_engine::{IntegrityReport, LsmOptions, LsmStats, RepairReport, io};
use bkndb_engine::{StorageBackend as _, StorageReadTx as _, StorageWriteTx as _};
use std::{ops::Bound, path::Path};

fn map_error(error: bkndb_engine::Error) -> BknError {
    use bkndb_engine::Error as E;
    match error {
        E::Corruption(s) => BknError::Corruption(s),
        E::Encoding(s) => BknError::Encoding(s),
        E::DatabaseLocked(s) => BknError::DatabaseLocked(s),
        E::Backend(s) => BknError::Backend(s),
        E::ResourceLimit(s) => BknError::ResourceLimit(s),
        E::UnsupportedFormat(s) => BknError::UnsupportedFormat(s),
        E::RecoveryRequired(s) => BknError::RecoveryRequired(s),
        E::CommitUnknown(s) => BknError::CommitUnknown(s),
        E::InvalidArgument(s) => BknError::InvalidArgument(s),
    }
}
fn table(t: TableSpec) -> bkndb_engine::TableSpec<'static> {
    bkndb_engine::TableSpec(t.0)
}
#[derive(Clone)]
pub struct LsmStorageBackend {
    inner: bkndb_engine::Engine,
}
impl LsmStorageBackend {
    #[cfg(not(target_arch = "wasm32"))]
    pub fn open(path: impl AsRef<Path>) -> Result<Self, BknError> {
        Self::open_with_options(path, LsmOptions::default())
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub fn open_with_options(
        path: impl AsRef<Path>,
        options: LsmOptions,
    ) -> Result<Self, BknError> {
        Ok(Self {
            inner: bkndb_engine::Engine::open_with_options(path, options).map_err(map_error)?,
        })
    }
    pub fn open_with_io(
        path: impl AsRef<Path>,
        options: LsmOptions,
        io: std::sync::Arc<dyn io::StorageIo>,
    ) -> Result<Self, BknError> {
        Ok(Self {
            inner: bkndb_engine::Engine::open_with_io(path, options, io).map_err(map_error)?,
        })
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub fn open_existing(path: impl AsRef<Path>, read_only: bool) -> Result<Self, BknError> {
        Ok(Self { inner: bkndb_engine::Engine::open_existing(path, read_only).map_err(map_error)? })
    }
    pub fn migrate_to(&self, dest: impl AsRef<Path>) -> Result<(), BknError> {
        self.inner.migrate_to(dest).map_err(map_error)
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub fn repair_to(
        src: impl AsRef<Path>,
        dest: impl AsRef<Path>,
    ) -> Result<RepairReport, BknError> {
        bkndb_engine::Engine::repair_to(src, dest).map_err(map_error)
    }
    pub fn peak_compaction_temporary_bytes(&self)->u64{self.inner.peak_compaction_temporary_bytes()}
    pub fn peak_compaction_disk_bytes(&self)->u64{self.inner.peak_compaction_disk_bytes()}
    pub fn engine(&self) -> &bkndb_engine::Engine {
        &self.inner
    }
    pub fn last_maintenance_error(&self) -> Option<bkndb_engine::Error> {
        self.inner.last_maintenance_error()
    }
    pub fn force_compact(&self) -> Result<(), BknError> {
        self.inner.force_compact().map_err(map_error)
    }
    pub fn sstable_count(&self) -> usize {
        self.inner.sstable_count()
    }
    pub fn path(&self) -> &Path {
        self.inner.path()
    }
    pub fn backup_to(&self, dest: impl AsRef<Path>) -> Result<(), BknError> {
        self.inner.backup_to(dest).map_err(map_error)
    }
    pub fn stats(&self) -> Result<LsmStats, BknError> {
        self.inner.stats().map_err(map_error)
    }
    pub fn verify_integrity(&self) -> Result<IntegrityReport, BknError> {
        self.inner.verify_integrity().map_err(map_error)
    }
}
pub struct LsmReadTx<'a>(bkndb_engine::LsmReadTx<'a>);
pub struct LsmWriteTx<'a>(bkndb_engine::LsmWriteTx<'a>);
macro_rules! read_impl {
    ($name:ident) => {
        impl StorageReadTx for $name<'_> {
            fn get(&self, t: TableSpec, key: &[u8]) -> Result<Option<Vec<u8>>, BknError> {
                self.0.get(table(t), key).map_err(map_error)
            }
            fn range(
                &self,
                t: TableSpec,
                start: Bound<&[u8]>,
                end: Bound<&[u8]>,
            ) -> Result<KvPairs, BknError> {
                self.0.range(table(t), start, end).map_err(map_error)
            }
            fn scan<'a>(
                &'a self,
                t: TableSpec,
                start: Bound<&[u8]>,
                end: Bound<&[u8]>,
            ) -> Result<KvIter<'a>, BknError> {
                Ok(Box::new(
                    self.0
                        .scan(table(t), start, end)
                        .map_err(map_error)?
                        .map(|r| r.map_err(map_error)),
                ))
            }
        }
    };
}
read_impl!(LsmReadTx);
read_impl!(LsmWriteTx);
impl StorageWriteTx for LsmWriteTx<'_> {
    fn put(&mut self, t: TableSpec, key: &[u8], value: &[u8]) -> Result<(), BknError> {
        self.0.put(table(t), key, value).map_err(map_error)
    }
    fn delete(&mut self, t: TableSpec, key: &[u8]) -> Result<(), BknError> {
        self.0.delete(table(t), key).map_err(map_error)
    }
    fn commit(self) -> Result<(), BknError> {
        self.0.commit().map_err(map_error)
    }
}
impl StorageBackend for LsmStorageBackend {
    type ReadTx<'a> = LsmReadTx<'static>;
    type WriteTx<'a> = LsmWriteTx<'static>;
    fn begin_read(&self) -> Result<Self::ReadTx<'_>, BknError> {
        self.inner.begin_read().map(LsmReadTx).map_err(map_error)
    }
    fn begin_write(&self) -> Result<Self::WriteTx<'_>, BknError> {
        self.inner.begin_write().map(LsmWriteTx).map_err(map_error)
    }
}
