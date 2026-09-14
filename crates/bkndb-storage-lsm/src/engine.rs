use std::collections::BTreeMap;
use std::fs::{self, File};
use std::ops::Bound;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use bkndb_core::{BknError, StorageBackend, StorageReadTx, StorageWriteTx, TableSpec};

use crate::compaction::{merge_sources, MergeSource};
use crate::container;
use crate::keys;
use crate::manifest::{Manifest, SstableRef};
use crate::memtable::{memtable_byte_size, LsmValue, Memtable};
use crate::sstable::SstableHandle;
use crate::wal;
use crate::wal::WalRecord;

#[derive(Debug, Clone)]
pub struct LsmOptions {
    pub memtable_flush_bytes: usize,
    pub compaction_trigger_files: usize,
    pub sparse_index_interval: usize,
}

impl Default for LsmOptions {
    fn default() -> Self {
        Self {
            memtable_flush_bytes: 4 * 1024 * 1024,
            compaction_trigger_files: 8,
            sparse_index_interval: 16,
        }
    }
}

struct EngineState {
    active_memtable: Arc<Memtable>,
    immutable_memtables: Vec<Arc<Memtable>>,
    /// Sorted ascending by generation — the last element is always newest.
    sstables: Vec<Arc<SstableHandle>>,
    next_sstable_id: u64,
    /// Absolute offset in the container file where the active WAL region
    /// begins — see `manifest.rs`'s field doc for why no length is needed.
    wal_region_start: u64,
}

/// A cheap, point-in-time view of "current memtable + immutable memtables +
/// SSTable set" — just `Arc` clones (refcount bumps), not deep copies.
/// Because SSTable blobs are immutable once written, and both flush and
/// compaction only ever replace `Arc` pointers in `EngineState` rather than
/// mutate shared data in place, a snapshot's data never changes under it —
/// a long-running read is never blocked by, and never blocks, concurrent
/// writers or background compaction.
struct ReadSnapshot {
    active_memtable: Arc<Memtable>,
    immutable_memtables: Vec<Arc<Memtable>>,
    sstables: Vec<Arc<SstableHandle>>,
}

/// The container file handle plus the header bookkeeping needed to write
/// the next header slot (see `container.rs`).
struct ContainerWriter {
    file: Arc<File>,
    header_seq: u64,
    header_slot: u8,
}

pub struct LsmStorageBackend {
    path: PathBuf,
    options: LsmOptions,
    state: RwLock<EngineState>,
    /// Held for a write tx's whole lifetime — the mechanism behind the
    /// trait's documented "backends serialize writers internally" contract.
    /// Compaction (`compact_all`) relies on this too: it must never be
    /// called without this lock already held (either inherited from a
    /// write-tx guard via `flush()`, or acquired explicitly by
    /// `force_compact()`), since it swaps out the live container file.
    writer_lock: Mutex<()>,
    container: Mutex<ContainerWriter>,
}

fn io_err(e: std::io::Error) -> BknError {
    BknError::Backend(e.to_string())
}

fn compaction_tmp_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".compact.tmp");
    PathBuf::from(s)
}

impl LsmStorageBackend {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, BknError> {
        Self::open_with_options(path, LsmOptions::default())
    }

    pub fn open_with_options(path: impl AsRef<Path>, options: LsmOptions) -> Result<Self, BknError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).map_err(io_err)?;
            }
        }
        // Clean up a stale temp file left by a crash mid-compaction — the
        // live database at `path` is untouched either way (the temp file
        // is only ever renamed over `path` after it's fully valid and
        // fsynced), this just avoids leaking disk space indefinitely.
        let _ = fs::remove_file(compaction_tmp_path(&path));

        let file = Arc::new(container::open_container_file(&path)?);

        let (manifest, header_seq, header_slot) = match container::read_header(&file)? {
            Some(slot) => {
                let bytes = container::read_blob(&file, slot.manifest_offset, slot.manifest_len)?;
                (Manifest::decode(&bytes)?, slot.seq, slot.slot)
            }
            None => {
                // Fresh, empty file: bootstrap a manifest with no
                // SSTables and an empty WAL region starting right after
                // itself. Two-pass encode: `wal_region_start` depends on
                // the manifest's own serialized length, but that length
                // doesn't vary with the placeholder value we fill it with
                // (bincode encodes `u64` at a fixed width) — see
                // `manifest.rs`'s test asserting exactly this.
                let placeholder = Manifest {
                    next_sstable_id: 1,
                    sstables: Vec::new(),
                    wal_region_start: 0,
                };
                let len = placeholder.encode()?.len() as u64;
                let manifest = Manifest {
                    wal_region_start: container::BODY_START + len,
                    ..placeholder
                };
                let bytes = manifest.encode()?;
                container::write_blob_at(&file, container::BODY_START, &bytes)?;
                file.sync_data().map_err(io_err)?;
                let (seq, slot) = container::write_header(&file, 0, 1, container::BODY_START, bytes.len() as u64)?;
                (manifest, seq, slot)
            }
        };

        let mut sstables = Vec::new();
        for r in &manifest.sstables {
            sstables.push(Arc::new(SstableHandle::open(&file, r.generation, r.offset, r.length)?));
        }
        sstables.sort_by_key(|s| s.generation);

        // Recovery: replay whatever the active WAL region still holds
        // (data committed but not yet flushed to an SSTable) back into a
        // fresh memtable. A truncated/corrupt trailing frame is silently
        // dropped by `wal::replay_from` itself.
        let mut active_memtable = Memtable::new();
        for record in wal::replay_from(&file, manifest.wal_region_start)? {
            for (key, value) in record.entries {
                match value {
                    Some(bytes) => {
                        active_memtable.insert(key, LsmValue::Value(bytes));
                    }
                    None => {
                        active_memtable.insert(key, LsmValue::Tombstone);
                    }
                }
            }
        }

        let state = EngineState {
            active_memtable: Arc::new(active_memtable),
            immutable_memtables: Vec::new(),
            sstables,
            next_sstable_id: manifest.next_sstable_id,
            wal_region_start: manifest.wal_region_start,
        };

        Ok(Self {
            path,
            options,
            state: RwLock::new(state),
            writer_lock: Mutex::new(()),
            container: Mutex::new(ContainerWriter { file, header_seq, header_slot }),
        })
    }

    fn snapshot(&self) -> ReadSnapshot {
        let state = self.state.read().unwrap_or_else(|e| e.into_inner());
        ReadSnapshot {
            active_memtable: state.active_memtable.clone(),
            immutable_memtables: state.immutable_memtables.clone(),
            sstables: state.sstables.clone(),
        }
    }

    fn commit_pending(&self, pending: BTreeMap<Vec<u8>, Option<Vec<u8>>>) -> Result<(), BknError> {
        if pending.is_empty() {
            return Ok(());
        }

        let record = WalRecord {
            entries: pending.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        };
        {
            let cw = self.container.lock().unwrap_or_else(|e| e.into_inner());
            wal::append(&cw.file, &record)?;
        }

        let needs_flush;
        {
            let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
            {
                // `Arc::make_mut`: cheap in-place mutation unless a reader
                // snapshot is currently outstanding, in which case it
                // clones first — clone-on-write, not clone-always.
                let map = Arc::make_mut(&mut state.active_memtable);
                for (key, value) in pending {
                    match value {
                        Some(bytes) => {
                            map.insert(key, LsmValue::Value(bytes));
                        }
                        None => {
                            map.insert(key, LsmValue::Tombstone);
                        }
                    }
                }
            }
            needs_flush = memtable_byte_size(&state.active_memtable) >= self.options.memtable_flush_bytes;
        }

        if needs_flush {
            self.flush()?;
        }
        Ok(())
    }

    /// Appends the flushed memtable as a new SSTable blob and a new
    /// manifest blob at the container file's current end, one `sync_data`
    /// covering both, then a header write pointing at the new manifest —
    /// that header write is the actual commit point for the flush. Old WAL
    /// bytes before the new `wal_region_start` are left as dead space,
    /// reclaimed only by the next compaction (see `compact_all`), per the
    /// confirmed "compaction is also the vacuum point" design.
    fn flush(&self) -> Result<(), BknError> {
        let memtable_to_flush;
        let generation;
        {
            let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
            if state.active_memtable.is_empty() {
                return Ok(());
            }
            memtable_to_flush = state.active_memtable.clone();
            state.immutable_memtables.push(memtable_to_flush.clone());
            state.active_memtable = Arc::new(Memtable::new());
            generation = state.next_sstable_id;
            state.next_sstable_id += 1;
        }

        let new_handle;
        let new_wal_region_start;
        {
            let mut cw = self.container.lock().unwrap_or_else(|e| e.into_inner());

            let entries: Vec<(Vec<u8>, LsmValue)> = memtable_to_flush.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            let count = entries.len();
            let handle = SstableHandle::write(&cw.file, generation, entries, count, self.options.sparse_index_interval)?;

            let existing_refs: Vec<SstableRef> = {
                let state = self.state.read().unwrap_or_else(|e| e.into_inner());
                state
                    .sstables
                    .iter()
                    .map(|s| SstableRef {
                        generation: s.generation,
                        offset: s.base_offset,
                        length: s.blob_len,
                    })
                    .collect()
            };
            let mut sstable_refs = existing_refs;
            sstable_refs.push(SstableRef {
                generation: handle.generation,
                offset: handle.base_offset,
                length: handle.blob_len,
            });

            let next_sstable_id = {
                let state = self.state.read().unwrap_or_else(|e| e.into_inner());
                state.next_sstable_id
            };
            let manifest_offset = handle.base_offset + handle.blob_len;
            let placeholder = Manifest {
                next_sstable_id,
                sstables: sstable_refs.clone(),
                wal_region_start: 0,
            };
            let len = placeholder.encode()?.len() as u64;
            let manifest = Manifest {
                wal_region_start: manifest_offset + len,
                ..placeholder
            };
            let bytes = manifest.encode()?;
            container::write_blob_at(&cw.file, manifest_offset, &bytes)?;
            cw.file.sync_data().map_err(io_err)?;
            let (seq, slot) = container::write_header(&cw.file, cw.header_seq, cw.header_slot, manifest_offset, bytes.len() as u64)?;
            cw.header_seq = seq;
            cw.header_slot = slot;

            new_handle = handle;
            new_wal_region_start = manifest.wal_region_start;
        }

        let should_compact;
        {
            let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
            state.sstables.push(Arc::new(new_handle));
            if let Some(pos) = state.immutable_memtables.iter().position(|m| Arc::ptr_eq(m, &memtable_to_flush)) {
                state.immutable_memtables.remove(pos);
            }
            state.wal_region_start = new_wal_region_start;
            should_compact = state.sstables.len() >= self.options.compaction_trigger_files;
        }

        if should_compact {
            self.compact_all()?;
        }
        Ok(())
    }

    /// Merges every current SSTable generation into one new SSTable blob,
    /// writing it into a **brand-new file** (header + merged blob + a
    /// manifest referencing just that one SSTable + an empty WAL region)
    /// rather than appending to the live file, then atomically renaming
    /// that new file over the live path — reusing the exact temp-write +
    /// `fs::rename` atomicity `manifest.rs` used to rely on at the
    /// single-manifest-file level, just applied to the whole database file.
    /// This is also the vacuum step: the new file only contains live data,
    /// so it's smaller than the file it replaces whenever there was dead
    /// space to reclaim.
    ///
    /// Because this always merges *all* generations at once (size-tiered,
    /// "compact everything past N files" rather than a partial/leveled
    /// merge), no older generation is ever left outside the batch that
    /// could still need a tombstone to shadow it — so `merge_sources`'s
    /// unconditional tombstone-dropping is always safe here.
    ///
    /// Must be called only while `writer_lock` is already held by the
    /// caller (either inherited from a write-tx guard via `flush()`, or
    /// acquired explicitly by `force_compact()`) — it does not acquire the
    /// lock itself, to avoid deadlocking when `flush()` calls it.
    ///
    /// A `ReadSnapshot` taken before this call holds `Arc<SstableHandle>`s
    /// that each independently own their own `Arc<File>`, captured at
    /// construction time and fully decoupled from `self.path`. Reading
    /// from them after the rename below is safe and correct: on POSIX, an
    /// open fd to an unlinked/renamed-away inode keeps working until
    /// closed; on Windows, `FILE_SHARE_DELETE` (set when every container
    /// file handle is opened, see `container.rs`) lets the rename proceed
    /// while such handles remain open, and NTFS defers freeing the old
    /// file's storage until the last handle to it closes.
    fn compact_all(&self) -> Result<(), BknError> {
        let sstables_to_merge;
        {
            let state = self.state.read().unwrap_or_else(|e| e.into_inner());
            if state.sstables.len() < 2 {
                return Ok(());
            }
            sstables_to_merge = state.sstables.clone();
        }

        let mut sources = Vec::with_capacity(sstables_to_merge.len());
        for (rank, sst) in sstables_to_merge.iter().rev().enumerate() {
            let entries = sst.range(Bound::Unbounded, Bound::Unbounded)?;
            sources.push(MergeSource { rank: rank as u32, entries });
        }
        let merged = merge_sources(sources);

        let new_generation = {
            let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
            let id = state.next_sstable_id;
            state.next_sstable_id += 1;
            id
        };

        let tmp_path = compaction_tmp_path(&self.path);
        let new_file = Arc::new(container::create_container_file(&tmp_path)?);
        new_file.set_len(container::BODY_START).map_err(io_err)?;

        let count = merged.len();
        let handle = SstableHandle::write(&new_file, new_generation, merged, count, self.options.sparse_index_interval)?;

        let placeholder = Manifest {
            next_sstable_id: new_generation + 1,
            sstables: vec![SstableRef {
                generation: new_generation,
                offset: handle.base_offset,
                length: handle.blob_len,
            }],
            wal_region_start: 0,
        };
        let len = placeholder.encode()?.len() as u64;
        let manifest_offset = handle.base_offset + handle.blob_len;
        let manifest = Manifest {
            wal_region_start: manifest_offset + len,
            ..placeholder
        };
        let bytes = manifest.encode()?;
        container::write_blob_at(&new_file, manifest_offset, &bytes)?;
        new_file.sync_data().map_err(io_err)?;
        container::write_header(&new_file, 0, 1, manifest_offset, bytes.len() as u64)?;
        new_file.sync_all().map_err(io_err)?; // final durability checkpoint of the whole new file before it goes live

        fs::rename(&tmp_path, &self.path).map_err(io_err)?;

        {
            let mut cw = self.container.lock().unwrap_or_else(|e| e.into_inner());
            cw.file = new_file.clone();
            cw.header_seq = 1;
            cw.header_slot = 0;
        }
        {
            let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
            state.sstables = vec![Arc::new(handle)];
            state.wal_region_start = manifest.wal_region_start;
            state.next_sstable_id = manifest.next_sstable_id;
        }

        Ok(())
    }

    /// Test/diagnostic hook: forces an immediate full compaction regardless
    /// of `compaction_trigger_files`. Unlike the internal auto-compact call
    /// from `flush()` (which inherits `writer_lock` from the enclosing
    /// write-tx guard), this is an external entry point and must acquire
    /// the lock itself.
    pub fn force_compact(&self) -> Result<(), BknError> {
        let _guard = self.writer_lock.lock().unwrap_or_else(|e| e.into_inner());
        self.compact_all()
    }

    pub fn sstable_count(&self) -> usize {
        self.state.read().unwrap_or_else(|e| e.into_inner()).sstables.len()
    }
}

fn lookup(snapshot: &ReadSnapshot, pending: Option<&BTreeMap<Vec<u8>, Option<Vec<u8>>>>, key: &[u8]) -> Result<Option<Vec<u8>>, BknError> {
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

fn lookup_range(
    snapshot: &ReadSnapshot,
    pending: Option<&BTreeMap<Vec<u8>, Option<Vec<u8>>>>,
    start: Bound<&[u8]>,
    end: Bound<&[u8]>,
) -> Result<Vec<(Vec<u8>, Vec<u8>)>, BknError> {
    let mut sources = Vec::new();
    let mut rank = 0u32;

    if let Some(p) = pending {
        let entries: Vec<(Vec<u8>, LsmValue)> = p
            .range::<[u8], _>((start, end))
            .map(|(k, v)| {
                (
                    k.clone(),
                    match v {
                        Some(b) => LsmValue::Value(b.clone()),
                        None => LsmValue::Tombstone,
                    },
                )
            })
            .collect();
        sources.push(MergeSource { rank, entries });
        rank += 1;
    }

    let entries: Vec<(Vec<u8>, LsmValue)> = snapshot
        .active_memtable
        .range::<[u8], _>((start, end))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    sources.push(MergeSource { rank, entries });
    rank += 1;

    for imm in snapshot.immutable_memtables.iter().rev() {
        let entries: Vec<(Vec<u8>, LsmValue)> = imm.range::<[u8], _>((start, end)).map(|(k, v)| (k.clone(), v.clone())).collect();
        sources.push(MergeSource { rank, entries });
        rank += 1;
    }

    for sst in snapshot.sstables.iter().rev() {
        let entries = sst.range(start, end)?;
        sources.push(MergeSource { rank, entries });
        rank += 1;
    }

    let merged = merge_sources(sources);
    Ok(merged
        .into_iter()
        .filter_map(|(k, v)| match v {
            LsmValue::Value(b) => Some((k, b)),
            LsmValue::Tombstone => None,
        })
        .collect())
}

fn translate_bound(table: TableSpec, bound: Bound<&[u8]>) -> Bound<Vec<u8>> {
    match bound {
        Bound::Included(k) => Bound::Included(keys::encode_key(table, k)),
        Bound::Excluded(k) => Bound::Excluded(keys::encode_key(table, k)),
        Bound::Unbounded => Bound::Unbounded,
    }
}

/// Maps a table-scoped user-key range onto the shared LSM keyspace: an
/// unbounded start/end becomes "the whole table" (its length-prefixed name
/// range), not "the whole database".
fn table_scan_bounds(table: TableSpec, start: Bound<&[u8]>, end: Bound<&[u8]>) -> (Bound<Vec<u8>>, Bound<Vec<u8>>) {
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

fn as_bound_slice(b: &Bound<Vec<u8>>) -> Bound<&[u8]> {
    match b {
        Bound::Included(v) => Bound::Included(v.as_slice()),
        Bound::Excluded(v) => Bound::Excluded(v.as_slice()),
        Bound::Unbounded => Bound::Unbounded,
    }
}

pub struct LsmReadTx<'a> {
    snapshot: ReadSnapshot,
    _marker: std::marker::PhantomData<&'a LsmStorageBackend>,
}

impl StorageReadTx for LsmReadTx<'_> {
    fn get(&self, table: TableSpec, key: &[u8]) -> Result<Option<Vec<u8>>, BknError> {
        lookup(&self.snapshot, None, &keys::encode_key(table, key))
    }

    fn range(&self, table: TableSpec, start: Bound<&[u8]>, end: Bound<&[u8]>) -> Result<Vec<(Vec<u8>, Vec<u8>)>, BknError> {
        let (lo, hi) = table_scan_bounds(table, start, end);
        let rows = lookup_range(&self.snapshot, None, as_bound_slice(&lo), as_bound_slice(&hi))?;
        Ok(rows.into_iter().map(|(k, v)| (keys::strip_table_prefix(table, &k), v)).collect())
    }
}

pub struct LsmWriteTx<'a> {
    backend: &'a LsmStorageBackend,
    snapshot: ReadSnapshot,
    pending: BTreeMap<Vec<u8>, Option<Vec<u8>>>,
    _guard: std::sync::MutexGuard<'a, ()>,
}

impl StorageReadTx for LsmWriteTx<'_> {
    fn get(&self, table: TableSpec, key: &[u8]) -> Result<Option<Vec<u8>>, BknError> {
        lookup(&self.snapshot, Some(&self.pending), &keys::encode_key(table, key))
    }

    fn range(&self, table: TableSpec, start: Bound<&[u8]>, end: Bound<&[u8]>) -> Result<Vec<(Vec<u8>, Vec<u8>)>, BknError> {
        let (lo, hi) = table_scan_bounds(table, start, end);
        let rows = lookup_range(&self.snapshot, Some(&self.pending), as_bound_slice(&lo), as_bound_slice(&hi))?;
        Ok(rows.into_iter().map(|(k, v)| (keys::strip_table_prefix(table, &k), v)).collect())
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
