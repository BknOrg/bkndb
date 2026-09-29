use std::collections::BTreeMap;
use std::fs::{self, File};
use std::ops::Bound;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use bkndb_core::{BknError, KvIter, KvPairs, StorageBackend, StorageReadTx, StorageWriteTx, TableSpec};

use crate::compaction::{EntryIter, MergeIter};
use crate::container;
use crate::keys;
use crate::manifest::{Manifest, SstableRef};
use crate::memtable::{memtable_byte_size, LsmValue, Memtable};
use crate::sstable::{SstableHandle, WriteOptions};
use crate::wal;
use crate::wal::WalRecord;

mod write;
mod ops;
mod tx;
#[cfg(test)]
mod tests;

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
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent).map_err(io_err)?;
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

}
