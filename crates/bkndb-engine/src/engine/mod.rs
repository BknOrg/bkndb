use crate::io::{File, OpenMode, StorageIo};
use std::collections::BTreeMap;
use std::ops::Bound;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex, RwLock,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

use crate::{BknError, KvIter, KvPairs, StorageBackend, StorageReadTx, StorageWriteTx, TableSpec};

use crate::compaction::{EntryIter, MergeIter};
use crate::container;
use crate::keys;
use crate::manifest::{Manifest, SstableRef};
use crate::memtable::{LsmValue, Memtable, memtable_byte_size};
use crate::sstable::{SstableHandle, WriteOptions};
use crate::wal;
use crate::wal::WalRecord;

mod ops;
#[cfg(test)]
mod tests;
mod tx;
mod write;

pub use tx::*;

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

#[derive(Clone)]
pub struct LsmStorageBackend {
    inner: Arc<EngineInner>,
}
#[doc(hidden)]
pub struct EngineInner {
    io: Arc<dyn StorageIo>,
    _lock_file: Arc<File>,
    failed: AtomicBool,
    last_failure: Mutex<Option<BknError>>,
    compaction_temp_peak: AtomicU64,
    compaction_disk_peak: AtomicU64,
    read_only: bool,
    recovery_tail_bytes: u64,
    format_version: u32,
    path: PathBuf,
    options: LsmOptions,
    state: RwLock<EngineState>,
    /// Held for a write tx's whole lifetime — the mechanism behind the
    /// trait's documented "backends serialize writers internally" contract.
    /// Compaction (`compact_all`) relies on this too: it must never be
    /// called without this lock already held (either inherited from a
    /// write-tx guard via `flush()`, or acquired explicitly by
    /// `force_compact()`), since it swaps out the live container file.
    writer_lock: Arc<WriterGate>,
    /// Mutable container writer. A separate, stable lock handle is retained
    /// by EngineInner across replacement of this data-file handle.
    container: Mutex<ContainerWriter>,
}

impl std::ops::Deref for LsmStorageBackend {
    type Target = EngineInner;
    fn deref(&self) -> &EngineInner {
        &self.inner
    }
}
#[derive(Default)]
struct WriterGate {
    busy: Mutex<bool>,
    wake: std::sync::Condvar,
}
struct WriterPermit(Arc<WriterGate>);
impl WriterGate {
    fn acquire(self: &Arc<Self>) -> Result<WriterPermit, BknError> {
        let mut busy = self
            .busy
            .lock()
            .map_err(|_| BknError::RecoveryRequired("writer mutex poisoned".into()))?;
        #[cfg(target_arch = "wasm32")]
        if *busy {
            return Err(BknError::DatabaseLocked(
                "write transaction already active".into(),
            ));
        }
        #[cfg(not(target_arch = "wasm32"))]
        while *busy {
            busy = self
                .wake
                .wait(busy)
                .map_err(|_| BknError::RecoveryRequired("writer mutex poisoned".into()))?;
        }
        *busy = true;
        Ok(WriterPermit(self.clone()))
    }
}
impl Drop for WriterPermit {
    fn drop(&mut self) {
        if let Ok(mut busy) = self.0.busy.lock() {
            *busy = false;
            self.0.wake.notify_one();
        }
    }
}
impl LsmStorageBackend {
    fn fail(&self, error: BknError) {
        if let Ok(mut last) = self.last_failure.lock() {
            *last = Some(error);
        }
        self.failed.store(true, Ordering::Release);
    }
    pub fn last_maintenance_error(&self) -> Option<BknError> {
        self.last_failure.lock().ok().and_then(|e| e.clone())
    }
    fn ensure_healthy(&self) -> Result<(), BknError> {
        if self.state.is_poisoned() || self.container.is_poisoned() {
            self.failed.store(true, Ordering::Release);
        }
        if self.failed.load(Ordering::Acquire) {
            return Err(BknError::RecoveryRequired(
                "an I/O or maintenance failure requires reopen".into(),
            ));
        }
        Ok(())
    }
    fn ensure_writable(&self) -> Result<(), BknError> {
        self.ensure_healthy()?;
        if self.read_only {
            return Err(BknError::UnsupportedFormat(
                "database view is read-only; open an explicit writable view or migrate legacy formats".into(),
            ));
        }
        Ok(())
    }
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
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
            Err(BknError::DatabaseLocked(path.display().to_string()))
        }
        Err(e) => Err(io_err(e)),
    }
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
    io: &dyn StorageIo,
    path: &Path,
    generation: u64,
    entries: MergeIter<'_>,
    expected_items: usize,
    opts: WriteOptions,
) -> Result<FreshContainer, BknError> {
    let file = Arc::new(io.open(path, OpenMode::CreateNew).map_err(io_err)?);
    let result = (|| {
        file.set_len(container::BODY_START).map_err(io_err)?;

        let entries = entries.map(|r| r.map(|(k, v)| (k, LsmValue::Value(v))));
        let handle = SstableHandle::write(&file, generation, entries, expected_items, opts)?;

        let placeholder = Manifest {
            next_sstable_id: generation
                .checked_add(1)
                .ok_or_else(|| BknError::ResourceLimit("generation exhausted".into()))?,
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
        let (seq, slot) = container::write_header(&file, 0, 1, manifest_offset, &bytes)?;
        container::write_header(&file, seq, slot, manifest_offset, &bytes)?;
        file.sync_all().map_err(io_err)?; // final durability checkpoint of the whole new file before it goes live
        handle.verify()?;
        Ok(FreshContainer {
            file: file.clone(),
            handle,
            manifest,
            manifest_len: bytes.len() as u64,
        })
    })();
    if result.is_err() {
        let _ = io.remove(path);
    }
    result
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
        Self::open_with_io(path, options, Arc::new(crate::io::NativeIo))
    }

    pub fn open_with_io(
        path: impl AsRef<Path>,
        options: LsmOptions,
        io: Arc<dyn StorageIo>,
    ) -> Result<Self, BknError> {
        Self::open_internal(path.as_ref(), options, false, false, false, io)
    }
    /// Opens an existing initialized database. Read-only opens replay into memory
    /// but never truncate WAL, rewrite roots or remove maintenance candidates.
    /// A stable lock sidecar may be created; it must never be deleted by callers.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn open_existing(path: impl AsRef<Path>, read_only: bool) -> Result<Self, BknError> {
        let path = path.as_ref();
        if !path.is_file() {
            return Err(BknError::InvalidArgument("database must already exist".into()));
        }
        let path = crate::io::NativeIo.database_path(path).map_err(io_err)?;
        let probe = crate::io::NativeIo.open(&path, OpenMode::ReadOnly).map_err(io_err)?;
        if container::read_header(&probe)?.is_none() {
            return Err(BknError::Corruption("database has no initialized root".into()));
        }
        Self::open_internal(&path, LsmOptions::default(), false, read_only, true,
            Arc::new(crate::io::NativeIo))
    }

    pub fn format_version(&self) -> u32 { self.format_version }
    pub fn recovery_required(&self) -> bool { self.recovery_tail_bytes != 0 }

    fn open_internal(
        path: &Path,
        options: LsmOptions,
        salvage: bool,
        inspect: bool,
        existing: bool,
        io: Arc<dyn StorageIo>,
    ) -> Result<Self, BknError> {
        if options.memtable_flush_bytes == 0
            || options.memtable_flush_bytes > 64 * 1024 * 1024
            || options.compaction_trigger_files < 2
            || options.block_size_bytes == 0
            || options.block_size_bytes > 64 * 1024 * 1024
        {
            return Err(BknError::InvalidArgument(
                "invalid flush, compaction or block size configuration".into(),
            ));
        }
        if !io.capabilities().complete() {
            return Err(BknError::UnsupportedFormat(
                "storage host lacks required durability or locking capabilities".into(),
            ));
        }
        let path = path.to_path_buf();
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            io.create_dir_all(parent).map_err(io_err)?;
        }
        let path = io.database_path(&path).map_err(io_err)?;
        let lock_file = Arc::new(
            io.open(&with_suffix(&path, ".lock"), OpenMode::OpenOrCreate)
                .map_err(io_err)?,
        );
        lock_exclusive(&lock_file, &path)?;
        let file = Arc::new(
            io.open(
                &path,
                if inspect {
                    OpenMode::ReadOnly
                } else if salvage || existing {
                    OpenMode::Existing
                } else {
                    OpenMode::OpenOrCreate
                },
            )
            .map_err(io_err)?,
        );
        let header = container::read_header(&file)?;
        let format_version = header.as_ref().map_or(3, |h| h.version);
        let read_only = inspect || salvage || header.as_ref().is_some_and(|h| h.version < 3);

        if (salvage || inspect || existing) && header.is_none() {
            return Err(BknError::Corruption(
                "repair source has no valid root".into(),
            ));
        }
        if header.is_none() && io.exists(&compaction_tmp_path(&path)).map_err(io_err)? {
            return Err(BknError::RecoveryRequired(
                "empty database has a compaction candidate; preserve and inspect it explicitly"
                    .into(),
            ));
        }

        let (manifest, manifest_len, mut header_seq, mut header_slot) = match header {
            Some(slot) => {
                let bytes = container::read_manifest(&file, &slot)?;
                let m = Manifest::decode(&bytes).map_err(manifest_corrupt)?;
                m.validate(
                    slot.manifest_offset,
                    slot.manifest_len,
                    file.metadata().map_err(io_err)?.len(),
                )?;
                (m, slot.manifest_len, slot.seq, slot.slot)
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
                let (seq, slot) =
                    container::write_header(&file, 0, 1, container::BODY_START, &bytes)?;
                file.sync_all().map_err(io_err)?;
                io.sync_parent(&path).map_err(io_err)?;
                (manifest, bytes.len() as u64, seq, slot)
            }
        };

        // Replay checked frames. Fences separate recoverable WAL from incomplete
        // maintenance output. Validate all referenced SSTables before truncating
        // an interrupted suffix; failed opens must preserve the source bytes.
        let mut active_memtable = Memtable::new();
        let mut recovered_bytes = 0usize;
        let (_, valid_end) = wal::replay_visit(
            &file,
            manifest.wal_region_start,
            salvage,
            format_version,
            |record| {
                if record.entries.windows(2).any(|w| w[0].0 >= w[1].0) {
                    return Err(BknError::Corruption(
                        "WAL keys must be unique and ordered".into(),
                    ));
                }
                // Apply replacement removals first. A bounded atomic batch can
                // otherwise exceed the recovery limit transiently before later
                // deletes/shrinks in the same frame are applied.
                for (key, _) in &record.entries {
                    keys::validate_encoded(key)?;
                    if let Some(old) = active_memtable.remove(key) {
                        recovered_bytes -= 64
                            + key.len()
                            + match old {
                                LsmValue::Value(v) => v.len(),
                                LsmValue::Tombstone => 0,
                            };
                    }
                }
                for (key, value) in record.entries {
                    keys::validate_encoded(&key)?;
                    let value = value.map_or(LsmValue::Tombstone, LsmValue::Value);
                    let size = 64
                        + key.len()
                        + match &value {
                            LsmValue::Value(v) => v.len(),
                            LsmValue::Tombstone => 0,
                        };
                    recovered_bytes = recovered_bytes
                        .checked_add(size)
                        .ok_or_else(|| BknError::ResourceLimit("recovery size overflow".into()))?;
                    if let Some(old) = active_memtable.insert(key.clone(), value) {
                        recovered_bytes -= 64
                            + key.len()
                            + match old {
                                LsmValue::Value(v) => v.len(),
                                LsmValue::Tombstone => 0,
                            };
                    }
                    if recovered_bytes > 256 * 1024 * 1024 {
                        return Err(BknError::ResourceLimit(
                            "recovered memtable exceeds 256 MiB".into(),
                        ));
                    }
                }
                Ok(())
            },
        )?;

        let mut sstables = Vec::new();
        for r in &manifest.sstables {
            sstables.push(Arc::new(SstableHandle::open(
                &file,
                r.generation,
                r.offset,
                r.length,
            )?));
        }
        sstables.sort_by_key(|s| s.generation);

        // A stale output may be the only useful recovery copy of a corrupt
        // database. Never delete it before validating all live source blocks.
        if !read_only && io.exists(&compaction_tmp_path(&path)).map_err(io_err)? {
            for sst in &sstables {
                sst.verify()?;
            }
            let _ = io.remove(&compaction_tmp_path(&path));
        }

        if !read_only && file.metadata().map_err(io_err)?.len() > valid_end {
            file.set_len(valid_end).map_err(io_err)?;
            file.sync_all().map_err(io_err)?;
        }
        if !read_only {
            let slot = container::read_header(&file)?
                .ok_or_else(|| BknError::Corruption("missing root".into()))?;
            let bytes = container::read_manifest(&file, &slot)?;
            (header_seq, header_slot) = container::write_header(
                &file,
                header_seq,
                header_slot,
                slot.manifest_offset,
                &bytes,
            )?;
        }
        let state = EngineState {
            active_memtable: Arc::new(active_memtable),
            immutable_memtables: Vec::new(),
            sstables,
            next_sstable_id: manifest.next_sstable_id,
            wal_region_start: manifest.wal_region_start,
            manifest_len,
        };

        Ok(Self {
            inner: Arc::new(EngineInner {
                io,
                _lock_file: lock_file,
                failed: AtomicBool::new(false),
                last_failure: Mutex::new(None),
                compaction_temp_peak: AtomicU64::new(0),
                compaction_disk_peak: AtomicU64::new(0),
                read_only,
                recovery_tail_bytes: if read_only { file.metadata().map_err(io_err)?.len().saturating_sub(valid_end) } else { 0 },
                format_version,
                path,
                options,
                state: RwLock::new(state),
                writer_lock: Arc::new(WriterGate::default()),
                container: Mutex::new(ContainerWriter {
                    file,
                    header_seq,
                    header_slot,
                }),
            }),
        })
    }
}

#[derive(Debug, Clone)]
pub struct RepairReport {
    pub source_bytes: u64,
    pub recovered_wal_records: u64,
    pub discarded_wal_bytes: u64,
}
impl LsmStorageBackend {
    pub fn migrate_to(&self, dest: impl AsRef<Path>) -> Result<(), BknError> {
        self.backup_to(dest)
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub fn repair_to(
        source: impl AsRef<Path>,
        dest: impl AsRef<Path>,
    ) -> Result<RepairReport, BknError> {
        if !source.as_ref().is_file() {
            return Err(BknError::InvalidArgument("repair source must exist".into()));
        }
        let engine = Self::open_internal(
            source.as_ref(),
            LsmOptions::default(),
            true,
            true,
            true,
            Arc::new(crate::io::NativeIo),
        )?;
        let cw = engine
            .container
            .lock()
            .map_err(|_| BknError::RecoveryRequired("poisoned container".into()))?;
        let start = engine
            .state
            .read()
            .map_err(|_| BknError::RecoveryRequired("engine mutex poisoned".into()))?
            .wal_region_start;
        let (records, end) =
            wal::replay_visit(&cw.file, start, true, engine.format_version, |_| Ok(()))?;
        let size = cw.file.metadata().map_err(io_err)?.len();
        let report = RepairReport {
            source_bytes: size,
            recovered_wal_records: records,
            discarded_wal_bytes: size - end,
        };
        drop(cw);
        engine.backup_to(dest)?;
        Ok(report)
    }
}

mod diagnostics;
pub use diagnostics::{Diagnostics, SstableDiagnostic};
