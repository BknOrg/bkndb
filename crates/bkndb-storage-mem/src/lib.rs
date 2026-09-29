use std::collections::BTreeMap;
use std::ops::Bound;
use std::sync::{Arc, Mutex, MutexGuard, RwLock};

use bkndb_core::{BknError, KvIter, KvPairs, StorageBackend, StorageReadTx, StorageWriteTx, TableSpec};

type Table = BTreeMap<Vec<u8>, Vec<u8>>;
/// Each table sits behind its own `Arc`, so a commit that clones the map
/// (because a reader snapshot still shares it) copies only table pointers,
/// plus the tables it actually writes to.
type Tables = BTreeMap<&'static str, Arc<Table>>;

/// In-memory backend for tests and WASM targets without filesystem access.
///
/// Same transaction semantics as the on-disk backends:
/// - a read transaction sees a fixed snapshot of the data as of
///   `begin_read`, unaffected by later commits;
/// - only one write transaction is open at a time (`begin_write` waits for
///   the previous one to end), so read-modify-write sequences can't lose
///   updates;
/// - a write transaction buffers its `put`/`delete` calls in an overlay
///   (`pending`) that is applied all at once inside `commit()`. Dropping it
///   without committing just drops the overlay — a real no-op rollback,
///   which multi-table batch operations (`RelationalDb::write_tx`) rely on.
#[derive(Default)]
pub struct MemoryStorageBackend {
    tables: RwLock<Arc<Tables>>,
    writer: Mutex<()>,
}

impl MemoryStorageBackend {
    pub fn new() -> Self {
        Self::default()
    }

    fn snapshot(&self) -> Arc<Tables> {
        self.tables.read().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

pub struct MemReadTx {
    snapshot: Arc<Tables>,
}

/// `None` in `pending`'s value means a buffered delete; `Some(v)` means a
/// buffered put. Read-your-own-writes (`get`/`range` within the same open
/// write tx) consults this overlay first, falling back to the snapshot the
/// transaction started from — which is also the latest committed state,
/// since no other writer can commit while this one holds `_guard`.
pub struct MemWriteTx<'a> {
    backend: &'a MemoryStorageBackend,
    snapshot: Arc<Tables>,
    pending: BTreeMap<(&'static str, Vec<u8>), Option<Vec<u8>>>,
    _guard: MutexGuard<'a, ()>,
}

impl StorageBackend for MemoryStorageBackend {
    type ReadTx<'a> = MemReadTx;
    type WriteTx<'a> = MemWriteTx<'a>;

    fn begin_read(&self) -> Result<Self::ReadTx<'_>, BknError> {
        Ok(MemReadTx { snapshot: self.snapshot() })
    }

    fn begin_write(&self) -> Result<Self::WriteTx<'_>, BknError> {
        let guard = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        Ok(MemWriteTx {
            backend: self,
            snapshot: self.snapshot(),
            pending: BTreeMap::new(),
            _guard: guard,
        })
    }
}

fn owned_bound(b: Bound<&[u8]>) -> Bound<Vec<u8>> {
    match b {
        Bound::Unbounded => Bound::Unbounded,
        Bound::Included(k) => Bound::Included(k.to_vec()),
        Bound::Excluded(k) => Bound::Excluded(k.to_vec()),
    }
}

fn snapshot_get(snapshot: &Tables, table: TableSpec, key: &[u8]) -> Option<Vec<u8>> {
    snapshot.get(table.0).and_then(|t| t.get(key).cloned())
}

/// True when no key can fall within `(lo, hi)` — `BTreeMap::range` panics
/// on such bounds, which a filter like `x > 10 AND x < 5` produces.
fn bounds_empty(lo: &Bound<Vec<u8>>, hi: &Bound<Vec<u8>>) -> bool {
    match (lo, hi) {
        (Bound::Included(a), Bound::Included(b)) => a > b,
        (Bound::Included(a) | Bound::Excluded(a), Bound::Included(b) | Bound::Excluded(b)) => a >= b,
        _ => false,
    }
}

fn snapshot_scan<'a>(snapshot: &'a Tables, table: TableSpec, start: Bound<&[u8]>, end: Bound<&[u8]>) -> KvIter<'a> {
    let (lo, hi) = (owned_bound(start), owned_bound(end));
    if bounds_empty(&lo, &hi) {
        return Box::new(std::iter::empty());
    }
    match snapshot.get(table.0) {
        Some(t) => Box::new(
            t.range::<Vec<u8>, _>((lo, hi))
                .map(|(k, v)| Ok((k.clone(), v.clone()))),
        ),
        None => Box::new(std::iter::empty()),
    }
}

impl StorageReadTx for MemReadTx {
    fn get(&self, table: TableSpec, key: &[u8]) -> Result<Option<Vec<u8>>, BknError> {
        Ok(snapshot_get(&self.snapshot, table, key))
    }

    fn range(&self, table: TableSpec, start: Bound<&[u8]>, end: Bound<&[u8]>) -> Result<KvPairs, BknError> {
        snapshot_scan(&self.snapshot, table, start, end).collect()
    }

    fn scan<'a>(&'a self, table: TableSpec, start: Bound<&[u8]>, end: Bound<&[u8]>) -> Result<KvIter<'a>, BknError> {
        Ok(snapshot_scan(&self.snapshot, table, start, end))
    }
}

impl StorageReadTx for MemWriteTx<'_> {
    fn get(&self, table: TableSpec, key: &[u8]) -> Result<Option<Vec<u8>>, BknError> {
        if let Some(overlay) = self.pending.get(&(table.0, key.to_vec())) {
            return Ok(overlay.clone());
        }
        Ok(snapshot_get(&self.snapshot, table, key))
    }

    fn range(&self, table: TableSpec, start: Bound<&[u8]>, end: Bound<&[u8]>) -> Result<KvPairs, BknError> {
        let mut merged: BTreeMap<Vec<u8>, Vec<u8>> = snapshot_scan(&self.snapshot, table, start, end).collect::<Result<_, _>>()?;

        let (lo, hi) = (owned_bound(start), owned_bound(end));
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
        if self.pending.is_empty() {
            return Ok(());
        }
        let mut guard = self.backend.tables.write().unwrap_or_else(|e| e.into_inner());
        // Clone-on-write: copies only if a reader snapshot still shares it.
        let tables = Arc::make_mut(&mut guard);
        for ((table, key), value) in self.pending {
            let t = Arc::make_mut(tables.entry(table).or_default());
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

#[cfg(test)]
mod tests {
    use super::*;

    const T: TableSpec = TableSpec("t");

    #[test]
    fn read_tx_sees_a_fixed_snapshot() {
        let db = MemoryStorageBackend::new();
        let mut w = db.begin_write().unwrap();
        w.put(T, b"a", b"1").unwrap();
        w.commit().unwrap();

        let r = db.begin_read().unwrap();
        let mut w = db.begin_write().unwrap();
        w.put(T, b"a", b"2").unwrap();
        w.put(T, b"b", b"3").unwrap();
        w.commit().unwrap();

        assert_eq!(r.get(T, b"a").unwrap(), Some(b"1".to_vec()));
        assert_eq!(r.range(T, Bound::Unbounded, Bound::Unbounded).unwrap().len(), 1);
        let fresh = db.begin_read().unwrap();
        assert_eq!(fresh.get(T, b"a").unwrap(), Some(b"2".to_vec()));
        assert_eq!(fresh.scan(T, Bound::Unbounded, Bound::Unbounded).unwrap().count(), 2);
    }

    #[test]
    fn writers_are_serialized() {
        let db = Arc::new(MemoryStorageBackend::new());
        let mut handles = Vec::new();
        for _ in 0..8 {
            let db = db.clone();
            handles.push(std::thread::spawn(move || {
                for _ in 0..100 {
                    let mut w = db.begin_write().unwrap();
                    let n = w.get(T, b"n").unwrap().map_or(0, |v| u64::from_be_bytes(v.try_into().unwrap()));
                    w.put(T, b"n", &(n + 1).to_be_bytes()).unwrap();
                    w.commit().unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        let n = db.begin_read().unwrap().get(T, b"n").unwrap().unwrap();
        assert_eq!(u64::from_be_bytes(n.try_into().unwrap()), 800, "no increment may be lost");
    }
}
