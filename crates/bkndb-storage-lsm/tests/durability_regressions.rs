use bkndb_core::{StorageBackend, StorageReadTx, StorageWriteTx, TableSpec};
use bkndb_storage_lsm::{LsmOptions, LsmStorageBackend};
use std::io::{Read, Seek, SeekFrom, Write};
const T: TableSpec = TableSpec("test");

#[test]
fn nonempty_short_file_is_rejected_without_modification() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("foreign.bkndb");
    let bytes = b"not a bkndb database";
    std::fs::write(&path, bytes).unwrap();
    assert!(LsmStorageBackend::open(&path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

#[test]
fn corruption_in_a_committed_wal_is_not_silently_discarded() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wal.bkndb");
    {
        let db = LsmStorageBackend::open(&path).unwrap();
        let mut tx = db.begin_write().unwrap();
        tx.put(T, b"key", b"acknowledged").unwrap();
        tx.commit().unwrap();
    }
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    file.seek(SeekFrom::End(-1)).unwrap();
    let mut byte = [0];
    file.read_exact(&mut byte).unwrap();
    file.seek(SeekFrom::End(-1)).unwrap();
    file.write_all(&[byte[0] ^ 1]).unwrap();
    file.sync_all().unwrap();
    drop(file);
    let damaged = std::fs::read(&path).unwrap();
    assert!(LsmStorageBackend::open(&path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), damaged);
}

#[test]
fn maintenance_failure_does_not_report_durable_commit_as_aborted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("maintenance.bkndb");
    let db = LsmStorageBackend::open_with_options(
        &path,
        LsmOptions {
            memtable_flush_bytes: 1,
            compaction_trigger_files: 2,
            block_size_bytes: 128,
            compression: false,
        },
    )
    .unwrap();
    let mut tx = db.begin_write().unwrap();
    tx.put(T, b"first", b"1").unwrap();
    tx.commit().unwrap();
    std::fs::create_dir(dir.path().join("maintenance.bkndb.compact.tmp")).unwrap();
    let mut tx = db.begin_write().unwrap();
    tx.put(T, b"second", b"2").unwrap();
    assert!(
        tx.commit().is_ok(),
        "post-commit maintenance must not report an acknowledged WAL commit as aborted"
    );
    drop(db);
    let reopened = LsmStorageBackend::open(&path).unwrap();
    assert_eq!(
        reopened.begin_read().unwrap().get(T, b"second").unwrap(),
        Some(b"2".to_vec())
    );
}

#[test]
fn a_damaged_header_slot_cannot_hide_commits_after_a_flush() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("roots.bkndb");
    {
        let db = LsmStorageBackend::open_with_options(
            &path,
            LsmOptions {
                memtable_flush_bytes: 128,
                compaction_trigger_files: 100,
                block_size_bytes: 128,
                compression: false,
            },
        )
        .unwrap();
        let mut tx = db.begin_write().unwrap();
        tx.put(T, b"flush", &[1; 256]).unwrap();
        tx.commit().unwrap();
        let mut tx = db.begin_write().unwrap();
        tx.put(T, b"later", b"ack").unwrap();
        tx.commit().unwrap();
    }
    for slot in 0..2 {
        let damaged = dir.path().join(format!("slot-{slot}"));
        std::fs::copy(&path, &damaged).unwrap();
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .open(&damaged)
            .unwrap();
        f.seek(SeekFrom::Start(slot * 64)).unwrap();
        f.write_all(&[0xff; 64]).unwrap();
        f.sync_all().unwrap();
        drop(f);
        let db = LsmStorageBackend::open(&damaged).unwrap();
        assert_eq!(
            db.begin_read().unwrap().get(T, b"later").unwrap(),
            Some(b"ack".to_vec())
        );
    }
}
#[test]
fn graph_id_exhaustion_is_an_error_instead_of_wrapping() {
    let dir = tempfile::tempdir().unwrap();
    let db = LsmStorageBackend::open(dir.path().join("ids")).unwrap();
    let mut tx = db.begin_write().unwrap();
    tx.put(TableSpec("meta"), b"next_node_id", &u64::MAX.to_be_bytes())
        .unwrap();
    tx.commit().unwrap();
    let db = bkndb_core::Db::new(db);
    assert!(matches!(
        db.graph().create_node("overflow", Default::default()),
        Err(bkndb_core::BknError::ResourceLimit(_))
    ));
}

#[cfg(unix)]
#[test]
fn symlink_aliases_share_the_same_writer_lock() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let db = LsmStorageBackend::open(&path).unwrap();
    let alias = dir.path().join("alias");
    std::os::unix::fs::symlink(&path, &alias).unwrap();
    assert!(matches!(
        LsmStorageBackend::open(alias),
        Err(bkndb_core::BknError::DatabaseLocked(_))
    ));
    drop(db);
    let alias = dir.path().join("hardlink");
    std::fs::hard_link(&path, &alias).unwrap();
    assert!(LsmStorageBackend::open(alias).is_err());
}

#[test]
fn hot_key_overwrites_also_trigger_a_wal_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let db = LsmStorageBackend::open_with_options(
        dir.path().join("hot"),
        LsmOptions {
            memtable_flush_bytes: 256,
            compaction_trigger_files: 100,
            block_size_bytes: 128,
            compression: false,
        },
    )
    .unwrap();
    for _ in 0..100 {
        let mut tx = db.begin_write().unwrap();
        tx.put(T, b"same", b"v").unwrap();
        tx.commit().unwrap();
    }
    assert!(db.sstable_count() > 0);
    assert!(db.stats().unwrap().wal_bytes < 512);
    db.verify_integrity().unwrap();
}
