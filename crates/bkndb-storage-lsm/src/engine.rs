use std::collections::BTreeMap;
use std::fs::{self, File};
use std::ops::Bound;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use bkndb_core::{BknError, KvIter, StorageBackend, StorageReadTx, StorageWriteTx, TableSpec};

use crate::compaction::{EntryIter, MergeIter};
use crate::container;
use crate::keys;
use crate::manifest::{Manifest, SstableRef};
use crate::memtable::{memtable_byte_size, LsmValue, Memtable};
use crate::sstable::{SstableHandle, WriteOptions};
use crate::wal;
use crate::wal::WalRecord;

#[derive(Debug, Clone)]
pub struct LsmOptions {
    /// Flush the memtable to a new SSTable once it holds this many bytes.
    pub memtable_flush_bytes: usize,
    /// Merge every SSTable into one as soon as there are this many.
    pub compaction_trigger_files: usize,
    /// Target (uncompressed) size of one SSTable block — the unit that is
    /// checksummed, compressed, and read per point lookup.
    pub block_size_bytes: usize,
    /// lz4-compress SSTable blocks written from now on. Blocks are flagged
    /// individually, so files mixing both kinds read fine either way.
    pub compression: bool,
}

impl Default for LsmOptions {
    fn default() -> Self {
        Self {
            memtable_flush_bytes: 16 * 1024 * 1024,
            compaction_trigger_files: 16,
            block_size_bytes: 4096,
            compression: true,
        }
    }
}

/// Point-in-time size figures for one database file — see
/// [`LsmStorageBackend::stats`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LsmStats {
    /// Size of the `.bkndb` file on disk.
    pub file_bytes: u64,
    pub sstable_count: usize,
    /// SSTables still in the pre-checksum v1 format (rewritten by the next
    /// compaction).
    pub legacy_sstable_count: usize,
    pub sstable_bytes: u64,
    /// Entries across all SSTables, counting every stored version and
    /// tombstone (estimated for legacy SSTables).
    pub sstable_entries: u64,
    /// Entries/bytes buffered in memory, not yet flushed to an SSTable
    /// (they are durable in the WAL).
    pub memtable_entries: usize,
    pub memtable_bytes: usize,
    /// Bytes of write-ahead log covering the memtable.
    pub wal_bytes: u64,
    /// Dead space (superseded SSTables, manifests and old WAL frames) that
    /// [`force_compact`](LsmStorageBackend::force_compact) would give back.
    pub reclaimable_bytes: u64,
}

/// What [`LsmStorageBackend::verify_integrity`] checked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IntegrityReport {
    pub sstables_checked: usize,
    /// Blocks whose checksum was recomputed and matched.
    pub blocks_verified: u64,
    /// Blocks from legacy v1 SSTables, which carry no checksum: fully
    /// decoded, but corruption that still decodes can't be detected there.
    pub legacy_blocks_unchecked: u64,
    pub entries: u64,
    pub wal_records: u64,
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
    /// Byte length of the live manifest blob (for `stats`).
    manifest_len: u64,
}

/// A cheap, point-in-time view of "current memtable + immutable memtables +
/// SSTable set" — just `Arc` clones (refcount bumps), not deep copies.
/// Because SSTable blobs are immutable once written, and both flush and
/// compaction only ever replace `Arc` pointers in `EngineState` rather than
/// mutate shared data in place, a snapshot's data never changes under it —
/// a long-running read is never blocked by, and never blocks, concurrent
/// writers or compaction.
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
    /// The container handle also carries this backend's exclusive OS file
    /// lock (released when the last `Arc<File>` to it is dropped). Compaction
    /// locks its replacement file *before* renaming it over `path`, so the
    /// path is never left unlocked in between.
    container: Mutex<ContainerWriter>,
}

fn io_err(e: std::io::Error) -> BknError {
    BknError::Backend(e.to_string())
}

fn manifest_corrupt(e: BknError) -> BknError {
    BknError::Corruption(format!("manifest undecodable: {e}"))
}

/// Takes an exclusive, non-blocking OS lock on an open container file,
/// failing fast with `DatabaseLocked` if another handle already holds it.
fn lock_exclusive(file: &File, path: &Path) -> Result<(), BknError> {
    match file.try_lock() {
        Ok(()) => Ok(()),
        Err(fs::TryLockError::WouldBlock) => Err(BknError::DatabaseLocked(path.display().to_string())),
        Err(fs::TryLockError::Error(e)) => Err(io_err(e)),
    }
}

/// Makes a just-completed `rename` inside `dir` durable. POSIX only persists
/// directory entries once the directory itself is fsynced; Windows has no
/// equivalent (and can't open a directory as a `File`), so it's a no-op there.
#[cfg(unix)]
fn sync_parent_dir(path: &Path) -> Result<(), BknError> {
    let dir = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    File::open(dir).and_then(|d| d.sync_all()).map_err(io_err)
}

#[cfg(not(unix))]
fn sync_parent_dir(_path: &Path) -> Result<(), BknError> {
    Ok(())
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

fn compaction_tmp_path(path: &Path) -> PathBuf {
    with_suffix(path, ".compact.tmp")
}

/// A complete, fsynced single-SSTable container file (see
/// [`write_fresh_container`]).
struct FreshContainer {
    file: Arc<File>,
    handle: SstableHandle,
    manifest: Manifest,
    manifest_len: u64,
}

/// Writes `entries` (ascending, live values only) into a brand-new file at
/// `path` as a complete database: header + one SSTable + a manifest
/// referencing it + an empty WAL region, fsynced before returning. Shared
/// by compaction (which then renames it over the live file) and backup.
/// On error the partial file is removed.
fn write_fresh_container(
    path: &Path,
    generation: u64,
    entries: MergeIter<'_>,
    expected_items: usize,
    opts: WriteOptions,
) -> Result<FreshContainer, BknError> {
    let result = (|| {
        let file = Arc::new(container::create_container_file(path)?);
        file.set_len(container::BODY_START).map_err(io_err)?;

        let entries = entries.map(|r| r.map(|(k, v)| (k, LsmValue::Value(v))));
        let handle = SstableHandle::write(&file, generation, entries, expected_items, opts)?;

        let placeholder = Manifest {
            next_sstable_id: generation + 1,
            sstables: vec![SstableRef {
                generation,
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
        container::write_blob_at(&file, manifest_offset, &bytes)?;
        file.sync_data().map_err(io_err)?;
        container::write_header(&file, 0, 1, manifest_offset, &bytes)?;
        file.sync_all().map_err(io_err)?; // final durability checkpoint of the whole new file before it goes live
        Ok(FreshContainer {
            file,
            handle,
            manifest,
            manifest_len: bytes.len() as u64,
        })
    })();
    if result.is_err() {
        let _ = fs::remove_file(path);
    }
    result
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
        let file = Arc::new(container::open_container_file(&path)?);
        lock_exclusive(&file, &path)?;

        // Clean up a stale temp file left by a crash mid-compaction — the
        // live database at `path` is untouched either way (the temp file
        // is only ever renamed over `path` after it's fully valid and
        // fsynced), this just avoids leaking disk space indefinitely. Done
        // only once we hold the lock, so it can't race a live backend's
        // in-progress compaction.
        let _ = fs::remove_file(compaction_tmp_path(&path));

        let (manifest, manifest_len, header_seq, header_slot) = match container::read_header(&file)? {
            Some(slot) => {
                let bytes = container::read_manifest(&file, &slot)?;
                (Manifest::decode(&bytes).map_err(manifest_corrupt)?, slot.manifest_len, slot.seq, slot.slot)
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
                let (seq, slot) = container::write_header(&file, 0, 1, container::BODY_START, &bytes)?;
                (manifest, bytes.len() as u64, seq, slot)
            }
        };

        // Recovery: replay whatever the active WAL region still holds
        // (data committed but not yet flushed to an SSTable) back into a
        // fresh memtable. Replay stops at the first truncated/corrupt frame;
        // everything from there on (a torn commit, or SSTable/manifest bytes
        // from a flush that crashed before its header write) is cut off so
        // new commits are appended directly after the last good frame.
        //
        // This runs before any SSTable is opened: each one mmaps the whole
        // file, and Windows refuses to shrink a file with a live mapping.
        let (records, valid_end) = wal::replay_from(&file, manifest.wal_region_start)?;
        if file.metadata().map_err(io_err)?.len() > valid_end {
            file.set_len(valid_end).map_err(io_err)?;
            file.sync_all().map_err(io_err)?;
        }
        let mut active_memtable = Memtable::new();
        for record in records {
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

        let mut sstables = Vec::new();
        for r in &manifest.sstables {
            sstables.push(Arc::new(SstableHandle::open(&file, r.generation, r.offset, r.length)?));
        }
        sstables.sort_by_key(|s| s.generation);

        let state = EngineState {
            active_memtable: Arc::new(active_memtable),
            immutable_memtables: Vec::new(),
            sstables,
            next_sstable_id: manifest.next_sstable_id,
            wal_region_start: manifest.wal_region_start,
            manifest_len,
        };

        Ok(Self {
            path,
            options,
            state: RwLock::new(state),
            writer_lock: Mutex::new(()),
            container: Mutex::new(ContainerWriter { file, header_seq, header_slot }),
        })
    }

    fn write_options(&self) -> WriteOptions {
        WriteOptions {
            block_size: self.options.block_size_bytes,
            compress: self.options.compression,
        }
    }

    fn snapshot(&self) -> ReadSnapshot {
        let state = self.state.read().unwrap_or_else(|e| e.into_inner());
        ReadSnapshot {
            active_memtable: state.active_memtable.clone(),
            immutable_memtables: state.immutable_memtables.clone(),
            sstables: state.sstables.clone(),
        }
    }

    fn commit_pending(&self, pending: Pending) -> Result<(), BknError> {
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
        let new_manifest_len;
        {
            let mut cw = self.container.lock().unwrap_or_else(|e| e.into_inner());

            let entries = memtable_to_flush.iter().map(|(k, v)| Ok((k.clone(), v.clone())));
            let handle = SstableHandle::write(&cw.file, generation, entries, memtable_to_flush.len(), self.write_options())?;

            let (mut sstable_refs, next_sstable_id) = {
                let state = self.state.read().unwrap_or_else(|e| e.into_inner());
                let refs: Vec<SstableRef> = state
                    .sstables
                    .iter()
                    .map(|s| SstableRef {
                        generation: s.generation,
                        offset: s.base_offset,
                        length: s.blob_len,
                    })
                    .collect();
                (refs, state.next_sstable_id)
            };
            sstable_refs.push(SstableRef {
                generation: handle.generation,
                offset: handle.base_offset,
                length: handle.blob_len,
            });

            let manifest_offset = handle.base_offset + handle.blob_len;
            let placeholder = Manifest {
                next_sstable_id,
                sstables: sstable_refs,
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
            let (seq, slot) = container::write_header(&cw.file, cw.header_seq, cw.header_slot, manifest_offset, &bytes)?;
            cw.header_seq = seq;
            cw.header_slot = slot;

            new_handle = handle;
            new_wal_region_start = manifest.wal_region_start;
            new_manifest_len = bytes.len() as u64;
        }

        let should_compact;
        {
            let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
            state.sstables.push(Arc::new(new_handle));
            if let Some(pos) = state.immutable_memtables.iter().position(|m| Arc::ptr_eq(m, &memtable_to_flush)) {
                state.immutable_memtables.remove(pos);
            }
            state.wal_region_start = new_wal_region_start;
            state.manifest_len = new_manifest_len;
            should_compact = state.sstables.len() >= self.options.compaction_trigger_files;
        }

        if should_compact {
            self.compact_all(false)?;
        }
        Ok(())
    }

    /// Merges every current SSTable generation into one new SSTable blob,
    /// writing it into a **brand-new file** (header + merged blob + a
    /// manifest referencing just that one SSTable + an empty WAL region)
    /// rather than appending to the live file, then atomically renaming
    /// that new file over the live path. This is also the vacuum step: the
    /// new file only contains live data, so it's smaller than the file it
    /// replaces whenever there was dead space to reclaim — and it's written
    /// in the current SSTable format, which upgrades legacy files.
    ///
    /// The merge streams: each input SSTable is read one block at a time
    /// and the output is written block by block, so memory stays flat no
    /// matter how large the database is.
    ///
    /// Because this always merges *all* generations at once (size-tiered,
    /// "compact everything past N files" rather than a partial/leveled
    /// merge), no older generation is ever left outside the batch that
    /// could still need a tombstone to shadow it — so dropping tombstones
    /// in the merge is always safe here.
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
    ///
    /// The automatic call from `flush` (`force == false`) only merges once
    /// there are at least two SSTables. `force_compact` rewrites even a lone
    /// SSTable: the point there is also to reclaim dead WAL/manifest space
    /// and upgrade legacy-format data, which one SSTable can still carry.
    fn compact_all(&self, force: bool) -> Result<(), BknError> {
        let sstables_to_merge;
        {
            let state = self.state.read().unwrap_or_else(|e| e.into_inner());
            let worth_it = state.sstables.len() >= 2 || (force && !state.sstables.is_empty());
            if !worth_it {
                return Ok(());
            }
            sstables_to_merge = state.sstables.clone();
        }

        // Newest generation first, as `MergeIter` expects.
        let sources: Vec<EntryIter<'_>> = sstables_to_merge
            .iter()
            .rev()
            .map(|sst| Box::new(sst.cursor(Bound::Unbounded, Bound::Unbounded)) as EntryIter<'_>)
            .collect();
        let expected: u64 = sstables_to_merge.iter().map(|s| s.estimated_entries()).sum();

        let new_generation = {
            let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
            let id = state.next_sstable_id;
            state.next_sstable_id += 1;
            id
        };

        let tmp_path = compaction_tmp_path(&self.path);
        let fresh = write_fresh_container(&tmp_path, new_generation, MergeIter::new(sources), expected as usize, self.write_options())?;

        lock_exclusive(&fresh.file, &self.path)?;
        fs::rename(&tmp_path, &self.path).map_err(io_err)?;
        sync_parent_dir(&self.path)?;

        {
            let mut cw = self.container.lock().unwrap_or_else(|e| e.into_inner());
            cw.file = fresh.file.clone();
            cw.header_seq = 1;
            cw.header_slot = 0;
        }
        {
            let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
            state.sstables = vec![Arc::new(fresh.handle)];
            state.wal_region_start = fresh.manifest.wal_region_start;
            state.next_sstable_id = fresh.manifest.next_sstable_id;
            state.manifest_len = fresh.manifest_len;
        }

        Ok(())
    }

    /// Forces an immediate full compaction regardless of
    /// `compaction_trigger_files`. Unlike the internal auto-compact call
    /// from `flush()` (which inherits `writer_lock` from the enclosing
    /// write-tx guard), this is an external entry point and must acquire
    /// the lock itself.
    ///
    /// Flushes the active memtable first: `compact_all` rebuilds the file
    /// from SSTables alone, starting a fresh empty WAL region, so any commit
    /// still living only in the memtable/WAL would otherwise be lost on the
    /// next reopen.
    pub fn force_compact(&self) -> Result<(), BknError> {
        let _guard = self.writer_lock.lock().unwrap_or_else(|e| e.into_inner());
        self.flush()?;
        self.compact_all(true)
    }

    pub fn sstable_count(&self) -> usize {
        self.state.read().unwrap_or_else(|e| e.into_inner()).sstables.len()
    }

    /// Path of the database file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Writes a consistent, compacted copy of the database — everything
    /// committed when the call starts — to a new file at `dest`, which opens
    /// like any other `.bkndb` file.
    ///
    /// Online: it reads from a snapshot, so readers and writers carry on
    /// meanwhile (commits made after the call started just aren't in the
    /// copy). The copy is written to `dest` + `.backup.tmp`, fsynced, then
    /// renamed into place, so a crash never leaves a half-written file
    /// under `dest`. Fails if `dest` already exists.
    pub fn backup_to(&self, dest: impl AsRef<Path>) -> Result<(), BknError> {
        let dest = dest.as_ref();
        if dest.exists() {
            return Err(BknError::Backend(format!("backup target '{}' already exists", dest.display())));
        }
        if let Some(parent) = dest.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent).map_err(io_err)?;
        }
        let snapshot = self.snapshot();
        let expected = snapshot.active_memtable.len() as u64
            + snapshot.immutable_memtables.iter().map(|m| m.len() as u64).sum::<u64>()
            + snapshot.sstables.iter().map(|s| s.estimated_entries()).sum::<u64>();
        let sources = scan_sources(&snapshot, None, &Bound::Unbounded, &Bound::Unbounded);

        let tmp = with_suffix(dest, ".backup.tmp");
        drop(write_fresh_container(&tmp, 1, MergeIter::new(sources), expected as usize, self.write_options())?);
        fs::rename(&tmp, dest).map_err(io_err)?;
        sync_parent_dir(dest)
    }

    /// Current size figures for the file and the in-memory write buffer.
    pub fn stats(&self) -> Result<LsmStats, BknError> {
        // Lock order matches `flush`: container, then state.
        let cw = self.container.lock().unwrap_or_else(|e| e.into_inner());
        let file_bytes = cw.file.metadata().map_err(io_err)?.len();
        let state = self.state.read().unwrap_or_else(|e| e.into_inner());
        let (memtable_entries, memtable_bytes) = std::iter::once(&state.active_memtable)
            .chain(&state.immutable_memtables)
            .fold((0, 0), |(n, b), m| (n + m.len(), b + memtable_byte_size(m)));
        let sstable_bytes: u64 = state.sstables.iter().map(|s| s.blob_len).sum();
        let wal_bytes = file_bytes.saturating_sub(state.wal_region_start);
        Ok(LsmStats {
            file_bytes,
            sstable_count: state.sstables.len(),
            legacy_sstable_count: state.sstables.iter().filter(|s| s.is_legacy()).count(),
            sstable_bytes,
            sstable_entries: state.sstables.iter().map(|s| s.estimated_entries()).sum(),
            memtable_entries,
            memtable_bytes,
            wal_bytes,
            reclaimable_bytes: file_bytes.saturating_sub(container::BODY_START + sstable_bytes + state.manifest_len + wal_bytes),
        })
    }

    /// Re-reads every stored byte and checks it: the header's manifest
    /// checksum, every SSTable block's checksum and encoding, and every WAL
    /// frame. Returns what was checked, or [`BknError::Corruption`] naming
    /// the first bad structure. SSTables are checked from a snapshot, so
    /// only the (short) WAL re-read briefly holds up commits.
    pub fn verify_integrity(&self) -> Result<IntegrityReport, BknError> {
        let snapshot = self.snapshot();
        let mut report = IntegrityReport::default();
        for sst in &snapshot.sstables {
            let counts = sst.verify()?;
            report.sstables_checked += 1;
            report.blocks_verified += counts.blocks_verified;
            report.legacy_blocks_unchecked += counts.blocks_unchecked;
            report.entries += counts.entries;
        }

        let cw = self.container.lock().unwrap_or_else(|e| e.into_inner());
        let slot = container::read_header(&cw.file)?.ok_or_else(|| BknError::Corruption("database header is missing".to_string()))?;
        Manifest::decode(&container::read_manifest(&cw.file, &slot)?).map_err(manifest_corrupt)?;
        let wal_start = self.state.read().unwrap_or_else(|e| e.into_inner()).wal_region_start;
        let (records, valid_end) = wal::replay_from(&cw.file, wal_start)?;
        let file_len = cw.file.metadata().map_err(io_err)?.len();
        if valid_end != file_len {
            return Err(BknError::Corruption(format!(
                "write-ahead log has {} trailing bytes that are not a valid frame",
                file_len - valid_end
            )));
        }
        report.wal_records = records.len() as u64;
        Ok(report)
    }
}

type Pending = BTreeMap<Vec<u8>, Option<Vec<u8>>>;

fn lookup(snapshot: &ReadSnapshot, pending: Option<&Pending>, key: &[u8]) -> Result<Option<Vec<u8>>, BknError> {
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
fn bounds_empty(lo: &Bound<Vec<u8>>, hi: &Bound<Vec<u8>>) -> bool {
    match (lo, hi) {
        (Bound::Included(a), Bound::Included(b)) => a > b,
        (Bound::Included(a) | Bound::Excluded(a), Bound::Included(b) | Bound::Excluded(b)) => a >= b,
        _ => false,
    }
}

/// Every source visible to a read, newest first — the order `MergeIter`
/// expects: the transaction's own pending writes, the active memtable, the
/// immutable memtables, then SSTables from the newest generation down.
fn scan_sources<'a>(snapshot: &'a ReadSnapshot, pending: Option<&'a Pending>, lo: &Bound<Vec<u8>>, hi: &Bound<Vec<u8>>) -> Vec<EntryIter<'a>> {
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
fn scan_table<'a>(snapshot: &'a ReadSnapshot, pending: Option<&'a Pending>, table: TableSpec, start: Bound<&[u8]>, end: Bound<&[u8]>) -> KvIter<'a> {
    let (lo, hi) = table_scan_bounds(table, start, end);
    let prefix_len = keys::table_prefix(table).len();
    let merged = MergeIter::new(scan_sources(snapshot, pending, &lo, &hi));
    Box::new(merged.map(move |r| r.map(|(k, v)| (k[prefix_len..].to_vec(), v))))
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

pub struct LsmReadTx<'a> {
    snapshot: ReadSnapshot,
    _marker: std::marker::PhantomData<&'a LsmStorageBackend>,
}

impl StorageReadTx for LsmReadTx<'_> {
    fn get(&self, table: TableSpec, key: &[u8]) -> Result<Option<Vec<u8>>, BknError> {
        lookup(&self.snapshot, None, &keys::encode_key(table, key))
    }

    fn range(&self, table: TableSpec, start: Bound<&[u8]>, end: Bound<&[u8]>) -> Result<Vec<(Vec<u8>, Vec<u8>)>, BknError> {
        scan_table(&self.snapshot, None, table, start, end).collect()
    }

    fn scan<'a>(&'a self, table: TableSpec, start: Bound<&[u8]>, end: Bound<&[u8]>) -> Result<KvIter<'a>, BknError> {
        Ok(scan_table(&self.snapshot, None, table, start, end))
    }
}

pub struct LsmWriteTx<'a> {
    backend: &'a LsmStorageBackend,
    snapshot: ReadSnapshot,
    pending: Pending,
    _guard: std::sync::MutexGuard<'a, ()>,
}

impl StorageReadTx for LsmWriteTx<'_> {
    fn get(&self, table: TableSpec, key: &[u8]) -> Result<Option<Vec<u8>>, BknError> {
        lookup(&self.snapshot, Some(&self.pending), &keys::encode_key(table, key))
    }

    fn range(&self, table: TableSpec, start: Bound<&[u8]>, end: Bound<&[u8]>) -> Result<Vec<(Vec<u8>, Vec<u8>)>, BknError> {
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

#[cfg(test)]
mod tests {
    use super::*;

    const T: TableSpec = TableSpec("t");

    /// Builds a complete database file exactly as format v1 laid it out:
    /// one uncompressed, checksum-free SSTable, a manifest, a v1 header.
    fn write_legacy_database(path: &Path, rows: &[(Vec<u8>, Vec<u8>)]) {
        let file = container::open_container_file(path).unwrap();
        file.set_len(container::BODY_START).unwrap();
        let entries: Vec<(Vec<u8>, LsmValue)> = rows.iter().map(|(k, v)| (keys::encode_key(T, k), LsmValue::Value(v.clone()))).collect();
        let (offset, length) = crate::sstable::write_v1(&file, &entries, 4);
        let placeholder = Manifest {
            next_sstable_id: 2,
            sstables: vec![SstableRef { generation: 1, offset, length }],
            wal_region_start: 0,
        };
        let manifest_offset = offset + length;
        let len = placeholder.encode().unwrap().len() as u64;
        let bytes = Manifest { wal_region_start: manifest_offset + len, ..placeholder }.encode().unwrap();
        container::write_blob_at(&file, manifest_offset, &bytes).unwrap();
        container::write_legacy_header(&file, 1, manifest_offset, bytes.len() as u64);
        file.sync_all().unwrap();
    }

    #[test]
    fn legacy_v1_database_opens_and_is_upgraded_by_compaction() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.bkndb");
        let rows: Vec<(Vec<u8>, Vec<u8>)> = (0u32..40).map(|i| (i.to_be_bytes().to_vec(), format!("v{i}").into_bytes())).collect();
        write_legacy_database(&path, &rows);

        {
            let db = LsmStorageBackend::open(&path).unwrap();
            let stats = db.stats().unwrap();
            assert_eq!((stats.sstable_count, stats.legacy_sstable_count), (1, 1));
            let r = db.begin_read().unwrap();
            assert_eq!(r.get(T, &7u32.to_be_bytes()).unwrap(), Some(b"v7".to_vec()));
            assert_eq!(r.range(T, Bound::Unbounded, Bound::Unbounded).unwrap(), rows);
            let report = db.verify_integrity().unwrap();
            assert_eq!((report.entries, report.blocks_verified), (40, 0));
            assert!(report.legacy_blocks_unchecked > 0);

            // New commits land in a v2 SSTable next to the legacy one.
            let mut w = db.begin_write().unwrap();
            w.put(T, &100u32.to_be_bytes(), b"new").unwrap();
            w.delete(T, &0u32.to_be_bytes()).unwrap();
            w.commit().unwrap();
            db.force_compact().unwrap();
            let stats = db.stats().unwrap();
            assert_eq!((stats.sstable_count, stats.legacy_sstable_count), (1, 0));
        }

        let db = LsmStorageBackend::open(&path).unwrap();
        let r = db.begin_read().unwrap();
        let all = r.range(T, Bound::Unbounded, Bound::Unbounded).unwrap();
        assert_eq!(all.len(), 40);
        assert_eq!(all[0].0, 1u32.to_be_bytes().to_vec());
        assert_eq!(r.get(T, &100u32.to_be_bytes()).unwrap(), Some(b"new".to_vec()));
        let report = db.verify_integrity().unwrap();
        assert_eq!((report.legacy_blocks_unchecked, report.entries), (0, 40));
    }

    #[test]
    fn force_compact_upgrades_a_lone_legacy_sstable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.bkndb");
        write_legacy_database(&path, &[(b"k".to_vec(), b"v".to_vec())]);
        let db = LsmStorageBackend::open(&path).unwrap();
        db.force_compact().unwrap();
        assert_eq!(db.stats().unwrap().legacy_sstable_count, 0);
        assert_eq!(db.begin_read().unwrap().get(T, b"k").unwrap(), Some(b"v".to_vec()));
    }
}
