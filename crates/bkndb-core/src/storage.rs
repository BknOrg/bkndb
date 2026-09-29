use std::ops::Bound;

use crate::BknError;

/// Identifies a logical table within a backend. M1 only deals in raw byte
/// tables; typed node/edge tables (M2+) are layered on top of this by
/// (de)serializing before calling `put`/`get`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TableSpec(pub &'static str);

/// A lazily-evaluated, ascending stream of `(key, value)` pairs from one
/// table — see [`StorageReadTx::scan`].
pub type KvIter<'a> = Box<dyn Iterator<Item = Result<(Vec<u8>, Vec<u8>), BknError>> + 'a>;

pub trait StorageBackend: Send + Sync {
    type ReadTx<'a>: StorageReadTx
    where
        Self: 'a;
    type WriteTx<'a>: StorageWriteTx
    where
        Self: 'a;

    fn begin_read(&self) -> Result<Self::ReadTx<'_>, BknError>;

    /// `&self`, not `&mut self`: backends (e.g. redb) enforce single-writer
    /// via an internal lock, not the borrow checker, which keeps
    /// `Arc<dyn StorageBackend>` usable from multiple threads later.
    fn begin_write(&self) -> Result<Self::WriteTx<'_>, BknError>;
}

pub trait StorageReadTx {
    fn get(&self, table: TableSpec, key: &[u8]) -> Result<Option<Vec<u8>>, BknError>;

    /// Range scan over raw byte keys within one table, returned as owned
    /// (key, value) pairs sorted ascending by key.
    fn range(
        &self,
        table: TableSpec,
        start: Bound<&[u8]>,
        end: Bound<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, BknError>;

    /// Same rows as [`range`](Self::range), but produced one at a time, so a
    /// caller that stops early (a `LIMIT`) or only folds over the rows (a
    /// `COUNT`) never holds the whole range in memory.
    ///
    /// The default materializes `range` up front; backends that can read
    /// incrementally override it.
    fn scan<'a>(&'a self, table: TableSpec, start: Bound<&[u8]>, end: Bound<&[u8]>) -> Result<KvIter<'a>, BknError> {
        Ok(Box::new(self.range(table, start, end)?.into_iter().map(Ok)))
    }
}

pub trait StorageWriteTx: StorageReadTx {
    fn put(&mut self, table: TableSpec, key: &[u8], value: &[u8]) -> Result<(), BknError>;

    fn delete(&mut self, table: TableSpec, key: &[u8]) -> Result<(), BknError>;

    /// Consumes self, matching the underlying backend's commit semantics.
    fn commit(self) -> Result<(), BknError>;
}
