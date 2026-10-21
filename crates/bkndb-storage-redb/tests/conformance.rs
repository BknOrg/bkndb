use std::ops::Bound;

use bkndb_core::test_util::conformance_suite;
use bkndb_core::{BknError, StorageBackend, StorageReadTx, StorageWriteTx, TableSpec};
use bkndb_storage_redb::RedbStorageBackend;

#[test]
fn redb_backend_satisfies_conformance_suite() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m1_test.bkndb");
    let backend = RedbStorageBackend::open(&path).unwrap();
    conformance_suite(&backend);
}

#[test]
fn redb_backend_operational_primitives_and_locking() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ops.bkndb");
    let backend = RedbStorageBackend::open(&path).unwrap();
    assert_eq!(backend.path(), path.as_path());

    // Lock contention maps to BknError::DatabaseLocked
    assert!(matches!(
        RedbStorageBackend::open(&path),
        Err(BknError::DatabaseLocked(_))
    ));

    let t = TableSpec("items");
    let mut w = backend.begin_write().unwrap();
    w.put(t, b"k1", b"v1").unwrap();
    w.put(t, b"k2", b"v2").unwrap();
    w.commit().unwrap();

    assert!(backend.file_size().unwrap() > 0);
    let stats = backend.stats().unwrap();
    assert!(stats.file_bytes > 0);
    assert_eq!(stats.table_count, 1);
    assert_eq!(stats.entries, 2);

    let report = backend.verify_integrity().unwrap();
    assert!(report.blocks_verified >= 1);
    assert_eq!(report.entries, 2);

    let backup_path = dir.path().join("nested").join("backup.bkndb");
    backend.backup_to(&backup_path).unwrap();
    // Never overwrites an existing backup target
    assert!(matches!(
        backend.backup_to(&backup_path),
        Err(BknError::Backend(_))
    ));

    let copy = RedbStorageBackend::open(&backup_path).unwrap();
    let r = copy.begin_read().unwrap();
    assert_eq!(
        r.range(t, Bound::Unbounded, Bound::Unbounded).unwrap(),
        vec![(b"k1".to_vec(), b"v1".to_vec()), (b"k2".to_vec(), b"v2".to_vec())]
    );
    drop(r);

    backend.force_compact().unwrap();
}

#[test]
fn redb_writers_are_serialized() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("concurrent_writers.bkndb");
    let backend = std::sync::Arc::new(RedbStorageBackend::open(&path).unwrap());
    let t = TableSpec("counter");
    let mut handles = Vec::new();
    for _ in 0..8 {
        let b = backend.clone();
        handles.push(std::thread::spawn(move || {
            for _ in 0..50 {
                let mut w = b.begin_write().unwrap();
                let n = w.get(t, b"n").unwrap().map_or(0, |v| u64::from_be_bytes(v.try_into().unwrap()));
                w.put(t, b"n", &(n + 1).to_be_bytes()).unwrap();
                w.commit().unwrap();
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    let r = backend.begin_read().unwrap();
    let n = r.get(t, b"n").unwrap().unwrap();
    assert_eq!(u64::from_be_bytes(n.try_into().unwrap()), 400, "all 400 increments across 8 threads must succeed");
}

