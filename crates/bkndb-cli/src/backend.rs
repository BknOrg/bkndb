//! Dynamic dispatch at the CLI boundary; storage algorithms remain in libraries.
use bkndb::{BknError, KvIter, KvPairs, StorageBackend, StorageReadTx, StorageWriteTx, TableSpec};
use bkndb_storage_lsm::{LsmReadTx, LsmStorageBackend, LsmWriteTx};
use bkndb_storage_redb::{RedbReadTx, RedbStorageBackend, RedbWriteTx};
use std::ops::Bound;
pub enum Backend {
    Redb(RedbStorageBackend),
    Lsm(LsmStorageBackend),
}
pub enum Read<'a> {
    Redb(RedbReadTx<'a>),
    Lsm(LsmReadTx<'a>),
}
pub enum Write<'a> {
    Redb(RedbWriteTx<'a>),
    Lsm(LsmWriteTx<'a>),
}
impl StorageBackend for Backend {
    type ReadTx<'a> = Read<'a>;
    type WriteTx<'a> = Write<'a>;
    fn begin_read(&self) -> Result<Read<'_>, BknError> {
        match self {
            Self::Redb(b) => b.begin_read().map(Read::Redb),
            Self::Lsm(b) => b.begin_read().map(Read::Lsm),
        }
    }
    fn begin_write(&self) -> Result<Write<'_>, BknError> {
        match self {
            Self::Redb(b) => b.begin_write().map(Write::Redb),
            Self::Lsm(b) => b.begin_write().map(Write::Lsm),
        }
    }
}
macro_rules! impl_read {
    ($ty:ident) => {
        impl StorageReadTx for $ty<'_> {
            fn get(&self, t: TableSpec, k: &[u8]) -> Result<Option<Vec<u8>>, BknError> {
                match self {
                    Self::Redb(x) => x.get(t, k),
                    Self::Lsm(x) => x.get(t, k),
                }
            }
            fn range(
                &self,
                t: TableSpec,
                s: Bound<&[u8]>,
                e: Bound<&[u8]>,
            ) -> Result<KvPairs, BknError> {
                match self {
                    Self::Redb(x) => x.range(t, s, e),
                    Self::Lsm(x) => x.range(t, s, e),
                }
            }
            fn scan<'a>(
                &'a self,
                t: TableSpec,
                s: Bound<&[u8]>,
                e: Bound<&[u8]>,
            ) -> Result<KvIter<'a>, BknError> {
                match self {
                    Self::Redb(x) => x.scan(t, s, e),
                    Self::Lsm(x) => x.scan(t, s, e),
                }
            }
        }
    };
}
impl_read!(Read);
impl_read!(Write);
impl StorageWriteTx for Write<'_> {
    fn put(&mut self, t: TableSpec, k: &[u8], v: &[u8]) -> Result<(), BknError> {
        match self {
            Self::Redb(x) => x.put(t, k, v),
            Self::Lsm(x) => x.put(t, k, v),
        }
    }
    fn delete(&mut self, t: TableSpec, k: &[u8]) -> Result<(), BknError> {
        match self {
            Self::Redb(x) => x.delete(t, k),
            Self::Lsm(x) => x.delete(t, k),
        }
    }
    fn commit(self) -> Result<(), BknError> {
        match self {
            Self::Redb(x) => x.commit().map_err(|e| match e {
                BknError::Backend(message) => BknError::CommitUnknown(message),
                other => other,
            }),
            Self::Lsm(x) => x.commit(),
        }
    }
}
