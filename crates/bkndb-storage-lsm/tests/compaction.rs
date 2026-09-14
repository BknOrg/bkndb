use std::collections::BTreeMap;
use std::ops::Bound;

use bkndb_core::{StorageBackend, StorageReadTx, StorageWriteTx, TableSpec};
use bkndb_storage_lsm::{LsmOptions, LsmStorageBackend};

const T: TableSpec = TableSpec("t");

fn tiny_flush_options() -> LsmOptions {
    LsmOptions {
        memtable_flush_bytes: 64, // forces a flush after just a couple of small commits
        compaction_trigger_files: 1000, // effectively disabled; compaction triggered manually
        sparse_index_interval: 2,
    }
}

fn full_scan(backend: &LsmStorageBackend) -> BTreeMap<Vec<u8>, Vec<u8>> {
    let r = backend.begin_read().unwrap();
    r.range(T, Bound::Unbounded, Bound::Unbounded).unwrap().into_iter().collect()
}

#[test]
fn compaction_preserves_the_live_key_set_and_collapses_files() {
    let dir = tempfile::tempdir().unwrap();
    let backend = LsmStorageBackend::open_with_options(dir.path().join("test.bkndb"), tiny_flush_options()).unwrap();

    // Enough small commits to force several flushes (several SSTable
    // generations) given the tiny flush threshold above.
    for i in 0u32..40 {
        let mut w = backend.begin_write().unwrap();
        w.put(T, &i.to_be_bytes(), format!("v{i}").as_bytes()).unwrap();
        w.commit().unwrap();
    }
    // Overwrite some keys and delete others across further small commits.
    for i in 0u32..10 {
        let mut w = backend.begin_write().unwrap();
        w.put(T, &i.to_be_bytes(), b"overwritten").unwrap();
        w.commit().unwrap();
    }
    for i in 10u32..20 {
        let mut w = backend.begin_write().unwrap();
        w.delete(T, &i.to_be_bytes()).unwrap();
        w.commit().unwrap();
    }

    assert!(backend.sstable_count() > 1, "the tiny flush threshold should have produced multiple SSTable generations");

    let before = full_scan(&backend);
    backend.force_compact().unwrap();
    let after = full_scan(&backend);

    assert_eq!(before, after, "compaction must not change the observable key set");
    assert_eq!(backend.sstable_count(), 1, "a full compaction collapses every generation into one file");

    // Deleted keys must be truly gone (not merely shadowed), and
    // overwritten keys must show only their newest value.
    for i in 10u32..20 {
        assert_eq!(after.get(i.to_be_bytes().as_slice()), None);
    }
    for i in 0u32..10 {
        assert_eq!(after.get(i.to_be_bytes().as_slice()), Some(&b"overwritten".to_vec()));
    }
}
