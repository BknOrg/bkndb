//! Raw KV facade over a shared [`StorageBackend`] — the "everything else"
//! escape hatch alongside [`crate::graph::GraphDb`] and
//! [`crate::relational::RelationalDb`], for callers who want direct
//! key/value access without going through either typed layer. Unlike
//! calling `backend.begin_read()`/`begin_write()` directly, every table
//! name passed here is checked against [`crate::RESERVED_TABLE_NAMES`]
//! first, so a caller can never accidentally corrupt bkndb-core's own
//! internal tables the way an app-level `RelSchema` named `"meta"` once did.

pub mod txn;
pub use txn::{BatchKv, ReadKv};

use std::ops::Bound;
use std::sync::Arc;

use crate::{check_table_name, BknError, StorageBackend, StorageReadTx, StorageWriteTx, TableSpec};

pub struct Kv<B: StorageBackend> {
    backend: Arc<B>,
}

impl<B: StorageBackend> Kv<B> {
    pub fn new(backend: B) -> Self {
        Self::from_arc(Arc::new(backend))
    }

    pub fn from_arc(backend: Arc<B>) -> Self {
        Self { backend }
    }

    pub fn get(&self, table: TableSpec, key: &[u8]) -> Result<Option<Vec<u8>>, BknError> {
        check_table_name(table.0)?;
        self.backend.begin_read()?.get(table, key)
    }

    pub fn range(
        &self,
        table: TableSpec,
        start: Bound<&[u8]>,
        end: Bound<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, BknError> {
        check_table_name(table.0)?;
        self.backend.begin_read()?.range(table, start, end)
    }

    /// Opens its own write tx and commits immediately — the non-batch
    /// convenience path, mirroring [`crate::relational::RelTable::insert`]'s
    /// "open, mutate, commit" shape. For multiple KV ops (or KV alongside
    /// graph/relational writes) committing atomically together, use
    /// [`crate::db::Db::write_tx`]'s `.kv()` view instead.
    pub fn put(&self, table: TableSpec, key: &[u8], value: &[u8]) -> Result<(), BknError> {
        check_table_name(table.0)?;
        let mut wtx = self.backend.begin_write()?;
        wtx.put(table, key, value)?;
        wtx.commit()
    }

    pub fn delete(&self, table: TableSpec, key: &[u8]) -> Result<(), BknError> {
        check_table_name(table.0)?;
        let mut wtx = self.backend.begin_write()?;
        wtx.delete(table, key)?;
        wtx.commit()
    }
}
