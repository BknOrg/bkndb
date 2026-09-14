use std::collections::BTreeMap;
use std::ops::Bound;
use std::sync::RwLock;

use bkndb_core::{BknError, StorageBackend, StorageReadTx, StorageWriteTx, TableSpec};

type Table = BTreeMap<Vec<u8>, Vec<u8>>;

/// In-memory backend for tests and WASM targets without filesystem access.
///
/// A write transaction buffers its `put`/`delete` calls in an in-memory
/// overlay (`pending`) rather than mutating the shared tables directly; the
/// overlay is only applied, all at once, inside `commit()`. Dropping a write
/// tx without calling `commit()` simply drops the overlay — a real no-op
/// rollback, matching what every other `StorageWriteTx` implementation
/// (redb, the LSM engine) already guarantees, which multi-table batch
/// operations (`RelationalDb::write_tx`) depend on.
#[derive(Default)]
pub struct MemoryStorageBackend {
    tables: RwLock<BTreeMap<&'static str, Table>>,
}

impl MemoryStorageBackend {
    pub fn new() -> Self {
        Self::default()
    }
}

pub struct MemReadTx<'a> {
    backend: &'a MemoryStorageBackend,
}

/// `None` in `pending`'s value means a buffered delete; `Some(v)` means a
/// buffered put. Read-your-own-writes (`get`/`range` within the same open
/// write tx) consults this overlay first, falling back to the committed
/// table underneath.
pub struct MemWriteTx<'a> {
    backend: &'a MemoryStorageBackend,
    pending: BTreeMap<(&'static str, Vec<u8>), Option<Vec<u8>>>,
}

impl StorageBackend for MemoryStorageBackend {
    type ReadTx<'a> = MemReadTx<'a>;
    type WriteTx<'a> = MemWriteTx<'a>;

    fn begin_read(&self) -> Result<Self::ReadTx<'_>, BknError> {
        Ok(MemReadTx { backend: self })
    }

    fn begin_write(&self) -> Result<Self::WriteTx<'_>, BknError> {
        Ok(MemWriteTx {
            backend: self,
            pending: BTreeMap::new(),
        })
    }
}

fn read_get(
    backend: &MemoryStorageBackend,
    table: TableSpec,
    key: &[u8],
) -> Result<Option<Vec<u8>>, BknError> {
    let tables = backend
        .tables
        .read()
        .map_err(|_| BknError::Backend("poisoned lock".into()))?;
    Ok(tables.get(table.0).and_then(|t| t.get(key).cloned()))
}

fn read_range(
    backend: &MemoryStorageBackend,
    table: TableSpec,
    start: Bound<&[u8]>,
    end: Bound<&[u8]>,
) -> Result<Vec<(Vec<u8>, Vec<u8>)>, BknError> {
    let tables = backend
        .tables
        .read()
        .map_err(|_| BknError::Backend("poisoned lock".into()))?;
    Ok(match tables.get(table.0) {
        Some(t) => t
            .range::<[u8], _>((start, end))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        None => Vec::new(),
    })
}

impl StorageReadTx for MemReadTx<'_> {
    fn get(&self, table: TableSpec, key: &[u8]) -> Result<Option<Vec<u8>>, BknError> {
        read_get(self.backend, table, key)
    }

    fn range(
        &self,
        table: TableSpec,
        start: Bound<&[u8]>,
        end: Bound<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, BknError> {
        read_range(self.backend, table, start, end)
    }
}

impl StorageReadTx for MemWriteTx<'_> {
    fn get(&self, table: TableSpec, key: &[u8]) -> Result<Option<Vec<u8>>, BknError> {
        if let Some(overlay) = self.pending.get(&(table.0, key.to_vec())) {
            return Ok(overlay.clone());
        }
        read_get(self.backend, table, key)
    }

    fn range(
        &self,
        table: TableSpec,
        start: Bound<&[u8]>,
        end: Bound<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, BknError> {
        let mut merged: BTreeMap<Vec<u8>, Vec<u8>> = read_range(self.backend, table, start, end)?.into_iter().collect();

        let key_bound = |b: Bound<&[u8]>| -> Bound<Vec<u8>> {
            match b {
                Bound::Unbounded => Bound::Unbounded,
                Bound::Included(k) => Bound::Included(k.to_vec()),
                Bound::Excluded(k) => Bound::Excluded(k.to_vec()),
            }
        };
        let (lo, hi) = (key_bound(start), key_bound(end));
        let in_range = |k: &[u8]| -> bool {
            let lo_ok = match &lo {
                Bound::Unbounded => true,
                Bound::Included(b) => k >= b.as_slice(),
                Bound::Excluded(b) => k > b.as_slice(),
            };
            let hi_ok = match &hi {
                Bound::Unbounded => true,
                Bound::Included(b) => k <= b.as_slice(),
                Bound::Excluded(b) => k < b.as_slice(),
            };
            lo_ok && hi_ok
        };

        for ((t, k), v) in &self.pending {
            if *t != table.0 || !in_range(k) {
                continue;
            }
            match v {
                Some(v) => {
                    merged.insert(k.clone(), v.clone());
                }
                None => {
                    merged.remove(k);
                }
            }
        }

        Ok(merged.into_iter().collect())
    }
}

impl StorageWriteTx for MemWriteTx<'_> {
    fn put(&mut self, table: TableSpec, key: &[u8], value: &[u8]) -> Result<(), BknError> {
        self.pending.insert((table.0, key.to_vec()), Some(value.to_vec()));
        Ok(())
    }

    fn delete(&mut self, table: TableSpec, key: &[u8]) -> Result<(), BknError> {
        self.pending.insert((table.0, key.to_vec()), None);
        Ok(())
    }

    fn commit(self) -> Result<(), BknError> {
        let mut tables = self
            .backend
            .tables
            .write()
            .map_err(|_| BknError::Backend("poisoned lock".into()))?;
        for ((table, key), value) in self.pending {
            let t = tables.entry(table).or_default();
            match value {
                Some(v) => {
                    t.insert(key, v);
                }
                None => {
                    t.remove(&key);
                }
            }
        }
        Ok(())
    }
}
