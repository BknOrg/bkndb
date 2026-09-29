//! Operational features: online backup, size statistics, integrity
//! verification and on-disk corruption detection.
use std::io::{Read, Seek, SeekFrom, Write};
use std::ops::Bound;

use bkndb_core::{BknError, StorageBackend, StorageReadTx, StorageWriteTx, TableSpec};
use bkndb_storage_lsm::{LsmOptions, LsmStorageBackend};

const T: TableSpec = TableSpec("t");

fn small_options() -> LsmOptions {
    LsmOptions {
        memtable_flush_bytes: 2048,
        compaction_trigger_files: 100,
        block_size_bytes: 256,
        compression: true,
    }
}

fn key(i: u32) -> Vec<u8> {
    i.to_be_bytes().to_vec()
}

fn put_range(db: &LsmStorageBackend, range: std::ops::Range<u32>, tag: &str) {
    let mut w = db.begin_write().unwrap();
    for i in range {
        w.put(T, &key(i), format!("{tag}-{i}-{}", "p".repeat(40)).as_bytes()).unwrap();
    }
    w.commit().unwrap();
}

fn all_rows(db: &LsmStorageBackend) -> Vec<(Vec<u8>, Vec<u8>)> {
    db.begin_read().unwrap().range(T, Bound::Unbounded, Bound::Unbounded).unwrap()
}

#[test]
fn backup_is_a_consistent_openable_copy() {
    let dir = tempfile::tempdir().unwrap();
    let db = LsmStorageBackend::open_with_options(dir.path().join("live.bkndb"), small_options()).unwrap();
    // Data spread over several SSTables, plus deletes/overwrites, plus
    // commits still only in the memtable/WAL.
    for batch in 0..5 {
        put_range(&db, batch * 50..batch * 50 + 50, "a");
    }
    put_range(&db, 10..20, "b");
    let mut w = db.begin_write().unwrap();
    w.delete(T, &key(0)).unwrap();
    w.commit().unwrap();
    assert!(db.sstable_count() > 1);
    assert!(db.stats().unwrap().memtable_entries > 0);

    let expected = all_rows(&db);
    // A reader snapshot taken before a later commit doesn't matter to the
    // backup: it captures everything committed when it starts.
    let backup_path = dir.path().join("nested").join("copy.bkndb");
    db.backup_to(&backup_path).unwrap();
    put_range(&db, 1000..1001, "after");

    // The live database is untouched and stays writable.
    assert_eq!(all_rows(&db).len(), expected.len() + 1);
    let copy = LsmStorageBackend::open(&backup_path).unwrap();
    assert_eq!(all_rows(&copy), expected);
    let stats = copy.stats().unwrap();
    assert_eq!((stats.sstable_count, stats.memtable_entries, stats.reclaimable_bytes), (1, 0, 0));
    copy.verify_integrity().unwrap();
    assert!(!dir.path().join("nested").join("copy.bkndb.backup.tmp").exists());

    // Never overwrites an existing file (including the live one).
    assert!(matches!(db.backup_to(&backup_path), Err(BknError::Backend(_))));
    assert!(db.backup_to(db.path()).is_err());
}

#[test]
fn stats_track_memtable_wal_and_reclaimable_space() {
    let dir = tempfile::tempdir().unwrap();
    let db = LsmStorageBackend::open_with_options(dir.path().join("s.bkndb"), small_options()).unwrap();
    let empty = db.stats().unwrap();
    assert_eq!((empty.sstable_count, empty.memtable_entries, empty.wal_bytes, empty.reclaimable_bytes), (0, 0, 0, 0));

    put_range(&db, 0..5, "a");
    let s = db.stats().unwrap();
    assert_eq!(s.memtable_entries, 5);
    assert!(s.wal_bytes > 0 && s.memtable_bytes > 0);

    for round in 0..4 {
        put_range(&db, 0..60, &format!("r{round}")); // overwrite the same keys
    }
    let s = db.stats().unwrap();
    assert!(s.sstable_count >= 2);
    assert!(s.reclaimable_bytes > 0, "flushed WAL frames are dead space");
    assert!(s.sstable_entries >= 60);

    db.force_compact().unwrap();
    let s = db.stats().unwrap();
    assert_eq!((s.sstable_count, s.memtable_entries, s.wal_bytes, s.reclaimable_bytes), (1, 0, 0, 0));
    assert_eq!(s.sstable_entries, 60, "compaction keeps one version per key");
    assert!(s.file_bytes > s.sstable_bytes);
}

#[test]
fn force_compact_vacuums_even_a_single_sstable() {
    let dir = tempfile::tempdir().unwrap();
    let db = LsmStorageBackend::open_with_options(dir.path().join("one.bkndb"), small_options()).unwrap();
    put_range(&db, 0..200, "x"); // one big commit: one flush, one SSTable, dead WAL behind it
    let before = db.stats().unwrap();
    assert_eq!(before.sstable_count, 1);
    assert!(before.reclaimable_bytes > 0);
    db.force_compact().unwrap();
    let after = db.stats().unwrap();
    assert_eq!(after.reclaimable_bytes, 0);
    assert!(after.file_bytes < before.file_bytes);
    assert_eq!(all_rows(&db).len(), 200);
}

#[test]
fn verify_integrity_passes_on_a_healthy_database() {
    let dir = tempfile::tempdir().unwrap();
    let db = LsmStorageBackend::open_with_options(dir.path().join("v.bkndb"), small_options()).unwrap();
    for batch in 0..4 {
        put_range(&db, batch * 40..batch * 40 + 40, "x");
    }
    put_range(&db, 500..502, "tail");
    let report = db.verify_integrity().unwrap();
    assert!(report.sstables_checked >= 1);
    assert!(report.blocks_verified > report.sstables_checked as u64);
    assert_eq!(report.legacy_blocks_unchecked, 0);
    assert!(report.wal_records >= 1);
}

#[test]
fn flipped_byte_on_disk_is_reported_as_corruption_not_wrong_data() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("c.bkndb");
    {
        let db = LsmStorageBackend::open_with_options(&path, small_options()).unwrap();
        put_range(&db, 0..200, "x");
        db.force_compact().unwrap();
    }
    // After compaction the file is header (128 bytes) + one SSTable whose
    // first block starts right at the body. Damage a byte inside it.
    {
        let mut f = std::fs::OpenOptions::new().read(true).write(true).open(&path).unwrap();
        let mut b = [0u8];
        f.seek(SeekFrom::Start(128 + 20)).unwrap();
        f.read_exact(&mut b).unwrap();
        f.seek(SeekFrom::Start(128 + 20)).unwrap();
        f.write_all(&[b[0] ^ 0x5A]).unwrap();
    }
    let db = LsmStorageBackend::open_with_options(&path, small_options()).unwrap();
    let v = db.verify_integrity();
    assert!(matches!(v, Err(BknError::Corruption(_))), "{v:?}");
    let r = db.begin_read().unwrap();
    assert!(matches!(r.get(T, &key(0)), Err(BknError::Corruption(_))));
    assert!(matches!(r.range(T, Bound::Unbounded, Bound::Unbounded), Err(BknError::Corruption(_))));
    // Keys in undamaged blocks are still readable.
    assert!(r.get(T, &key(199)).unwrap().is_some());
}

#[test]
fn damaged_manifest_fails_open_with_corruption() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m.bkndb");
    {
        let db = LsmStorageBackend::open_with_options(&path, small_options()).unwrap();
        put_range(&db, 0..100, "x");
        db.force_compact().unwrap();
    }
    // The manifest sits right before the (now empty) WAL region, i.e. at
    // the very end of a freshly compacted file.
    let len = std::fs::metadata(&path).unwrap().len();
    {
        let mut f = std::fs::OpenOptions::new().read(true).write(true).open(&path).unwrap();
        f.seek(SeekFrom::Start(len - 3)).unwrap();
        f.write_all(&[0xEE]).unwrap();
    }
    assert!(matches!(LsmStorageBackend::open(&path), Err(BknError::Corruption(_))));
}

#[test]
fn uncompressed_and_compressed_blocks_mix_in_one_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mix.bkndb");
    {
        let opts = LsmOptions { compression: false, ..small_options() };
        let db = LsmStorageBackend::open_with_options(&path, opts).unwrap();
        put_range(&db, 0..100, "raw");
    }
    let db = LsmStorageBackend::open_with_options(&path, small_options()).unwrap();
    put_range(&db, 50..150, "lz4");
    let rows = all_rows(&db);
    assert_eq!(rows.len(), 150);
    assert!(rows[10].1.starts_with(b"raw-10"));
    assert!(rows[60].1.starts_with(b"lz4-60"));
    db.force_compact().unwrap();
    assert_eq!(all_rows(&db), rows);
    db.verify_integrity().unwrap();
}

#[test]
fn compression_shrinks_repetitive_data() {
    let dir = tempfile::tempdir().unwrap();
    let size_with = |compression: bool, name: &str| {
        let opts = LsmOptions {
            compression,
            block_size_bytes: 4096,
            ..small_options()
        };
        let db = LsmStorageBackend::open_with_options(dir.path().join(name), opts).unwrap();
        put_range(&db, 0..500, "row");
        db.force_compact().unwrap();
        db.stats().unwrap().sstable_bytes
    };
    let raw = size_with(false, "raw.bkndb");
    let packed = size_with(true, "lz4.bkndb");
    assert!(packed * 2 < raw, "lz4 should at least halve highly repetitive rows ({packed} vs {raw})");
}

