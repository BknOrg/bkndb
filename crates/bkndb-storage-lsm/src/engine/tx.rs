//! Read/write transactions and the merged point/range lookups behind them.
use super::*;

pub(super) type Pending = BTreeMap<Vec<u8>, Option<Vec<u8>>>;

pub(super) fn lookup(snapshot: &ReadSnapshot, pending: Option<&Pending>, key: &[u8]) -> Result<Option<Vec<u8>>, BknError> {
    if let Some(entry) = pending.and_then(|p| p.get(key)) {
        return Ok(entry.clone());
    }
    if let Some(v) = snapshot.active_memtable.get(key) {
        return Ok(match v {
            LsmValue::Value(b) => Some(b.clone()),
            LsmValue::Tombstone => None,
        });
    }
    for imm in snapshot.immutable_memtables.iter().rev() {
        if let Some(v) = imm.get(key) {
            return Ok(match v {
                LsmValue::Value(b) => Some(b.clone()),
                LsmValue::Tombstone => None,
            });
        }
    }
    for sst in snapshot.sstables.iter().rev() {
        if let Some(v) = sst.get(key)? {
            return Ok(match v {
                LsmValue::Value(b) => Some(b),
                LsmValue::Tombstone => None,
            });
        }
    }
    Ok(None)
}

/// True when `(lo, hi)` can't contain any key. `BTreeMap::range` panics on
/// such bounds (a filter like `x > 10 AND x < 5` produces them), so scans
/// short-circuit to an empty result instead.
pub(super) fn bounds_empty(lo: &Bound<Vec<u8>>, hi: &Bound<Vec<u8>>) -> bool {
    match (lo, hi) {
        (Bound::Included(a), Bound::Included(b)) => a > b,
        (Bound::Included(a) | Bound::Excluded(a), Bound::Included(b) | Bound::Excluded(b)) => a >= b,
        _ => false,
    }
}

/// Every source visible to a read, newest first — the order `MergeIter`
/// expects: the transaction's own pending writes, the active memtable, the
/// immutable memtables, then SSTables from the newest generation down.
pub(super) fn scan_sources<'a>(snapshot: &'a ReadSnapshot, pending: Option<&'a Pending>, lo: &Bound<Vec<u8>>, hi: &Bound<Vec<u8>>) -> Vec<EntryIter<'a>> {
    let mut sources: Vec<EntryIter<'a>> = Vec::new();
    if bounds_empty(lo, hi) {
        return sources;
    }
    let bounds = (lo.clone(), hi.clone());
    if let Some(p) = pending {
        sources.push(Box::new(p.range::<Vec<u8>, _>(bounds.clone()).map(|(k, v)| {
            let v = match v {
                Some(b) => LsmValue::Value(b.clone()),
                None => LsmValue::Tombstone,
            };
            Ok((k.clone(), v))
        })));
    }
    for mem in std::iter::once(&snapshot.active_memtable).chain(snapshot.immutable_memtables.iter().rev()) {
        sources.push(Box::new(mem.range::<Vec<u8>, _>(bounds.clone()).map(|(k, v)| Ok((k.clone(), v.clone())))));
    }
    for sst in snapshot.sstables.iter().rev() {
        sources.push(Box::new(sst.cursor(lo.clone(), hi.clone())));
    }
    sources
}

/// A streaming scan over one table, with the table prefix stripped back off.
pub(super) fn scan_table<'a>(snapshot: &'a ReadSnapshot, pending: Option<&'a Pending>, table: TableSpec, start: Bound<&[u8]>, end: Bound<&[u8]>) -> KvIter<'a> {
    let (lo, hi) = table_scan_bounds(table, start, end);
    let prefix_len = keys::table_prefix(table).len();
    let merged = MergeIter::new(scan_sources(snapshot, pending, &lo, &hi));
    Box::new(merged.map(move |r| r.map(|(k, v)| (k[prefix_len..].to_vec(), v))))
}

pub(super) fn translate_bound(table: TableSpec, bound: Bound<&[u8]>) -> Bound<Vec<u8>> {
    match bound {
        Bound::Included(k) => Bound::Included(keys::encode_key(table, k)),
        Bound::Excluded(k) => Bound::Excluded(keys::encode_key(table, k)),
        Bound::Unbounded => Bound::Unbounded,
    }
}

/// Maps a table-scoped user-key range onto the shared LSM keyspace: an
/// unbounded start/end becomes "the whole table" (its length-prefixed name
/// range), not "the whole database".
pub(super) fn table_scan_bounds(table: TableSpec, start: Bound<&[u8]>, end: Bound<&[u8]>) -> (Bound<Vec<u8>>, Bound<Vec<u8>>) {
    let lo = match start {
        Bound::Unbounded => Bound::Included(keys::table_prefix(table)),
        other => translate_bound(table, other),
    };
    let hi = match end {
        Bound::Unbounded => match keys::prefix_upper_bound(&keys::table_prefix(table)) {
            Some(b) => Bound::Excluded(b),
            None => Bound::Unbounded,
        },
        other => translate_bound(table, other),
    };
    (lo, hi)
}

pub struct LsmReadTx<'a> {
    pub(super) snapshot: ReadSnapshot,
    pub(super) _marker: std::marker::PhantomData<&'a LsmStorageBackend>,
}

impl StorageReadTx for LsmReadTx<'_> {
    fn get(&self, table: TableSpec, key: &[u8]) -> Result<Option<Vec<u8>>, BknError> {
        lookup(&self.snapshot, None, &keys::encode_key(table, key))
    }

    fn range(&self, table: TableSpec, start: Bound<&[u8]>, end: Bound<&[u8]>) -> Result<KvPairs, BknError> {
        scan_table(&self.snapshot, None, table, start, end).collect()
    }

    fn scan<'a>(&'a self, table: TableSpec, start: Bound<&[u8]>, end: Bound<&[u8]>) -> Result<KvIter<'a>, BknError> {
        Ok(scan_table(&self.snapshot, None, table, start, end))
    }
}

pub struct LsmWriteTx<'a> {
    pub(super) backend: &'a LsmStorageBackend,
    pub(super) snapshot: ReadSnapshot,
    pub(super) pending: Pending,
    pub(super) _guard: std::sync::MutexGuard<'a, ()>,
}

impl StorageReadTx for LsmWriteTx<'_> {
    fn get(&self, table: TableSpec, key: &[u8]) -> Result<Option<Vec<u8>>, BknError> {
        lookup(&self.snapshot, Some(&self.pending), &keys::encode_key(table, key))
    }

    fn range(&self, table: TableSpec, start: Bound<&[u8]>, end: Bound<&[u8]>) -> Result<KvPairs, BknError> {
        scan_table(&self.snapshot, Some(&self.pending), table, start, end).collect()
    }

    fn scan<'a>(&'a self, table: TableSpec, start: Bound<&[u8]>, end: Bound<&[u8]>) -> Result<KvIter<'a>, BknError> {
        Ok(scan_table(&self.snapshot, Some(&self.pending), table, start, end))
    }
}

impl StorageWriteTx for LsmWriteTx<'_> {
    fn put(&mut self, table: TableSpec, key: &[u8], value: &[u8]) -> Result<(), BknError> {
        self.pending.insert(keys::encode_key(table, key), Some(value.to_vec()));
        Ok(())
    }

    fn delete(&mut self, table: TableSpec, key: &[u8]) -> Result<(), BknError> {
        self.pending.insert(keys::encode_key(table, key), None);
        Ok(())
    }

    /// Everything up to this point lived only in `self.pending` — nothing
    /// touched the container file. That's what makes "drop without commit"
    /// a trivial, always-correct rollback: deallocating an unused
    /// `BTreeMap` is all it takes, no undo log or compensating writes
    /// needed anywhere.
    fn commit(self) -> Result<(), BknError> {
        self.backend.commit_pending(self.pending)
    }
}

impl StorageBackend for LsmStorageBackend {
    type ReadTx<'a> = LsmReadTx<'a>;
    type WriteTx<'a> = LsmWriteTx<'a>;

    fn begin_read(&self) -> Result<Self::ReadTx<'_>, BknError> {
        Ok(LsmReadTx {
            snapshot: self.snapshot(),
            _marker: std::marker::PhantomData,
        })
    }

    fn begin_write(&self) -> Result<Self::WriteTx<'_>, BknError> {
        let guard = self.writer_lock.lock().unwrap_or_else(|e| e.into_inner());
        Ok(LsmWriteTx {
            backend: self,
            snapshot: self.snapshot(),
            pending: BTreeMap::new(),
            _guard: guard,
        })
    }
}
