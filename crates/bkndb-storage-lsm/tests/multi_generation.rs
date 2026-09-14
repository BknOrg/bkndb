use bkndb_core::{StorageBackend, StorageReadTx, StorageWriteTx, TableSpec};
use bkndb_storage_lsm::{LsmOptions, LsmStorageBackend};

const T: TableSpec = TableSpec("t");

#[test]
fn reads_merge_correctly_across_several_sstable_generations() {
    let dir = tempfile::tempdir().unwrap();
    let options = LsmOptions {
        memtable_flush_bytes: 32,
        compaction_trigger_files: 1_000_000, // disabled: this test wants distinct on-disk generations, not one merged file
        sparse_index_interval: 2,
    };
    let backend = LsmStorageBackend::open_with_options(dir.path().join("test.bkndb"), options).unwrap();

    // Generation 1: keys 0..10
    for i in 0u32..10 {
        let mut w = backend.begin_write().unwrap();
        w.put(T, &i.to_be_bytes(), b"gen1").unwrap();
        w.commit().unwrap();
    }
    // Generation 2: overwrites keys 0..5, adds keys 10..15
    for i in 0u32..5 {
        let mut w = backend.begin_write().unwrap();
        w.put(T, &i.to_be_bytes(), b"gen2").unwrap();
        w.commit().unwrap();
    }
    for i in 10u32..15 {
        let mut w = backend.begin_write().unwrap();
        w.put(T, &i.to_be_bytes(), b"gen2").unwrap();
        w.commit().unwrap();
    }
    // Generation 3: deletes keys 5..8
    for i in 5u32..8 {
        let mut w = backend.begin_write().unwrap();
        w.delete(T, &i.to_be_bytes()).unwrap();
        w.commit().unwrap();
    }

    assert!(backend.sstable_count() >= 3, "expected at least 3 separate SSTable generations, got {}", backend.sstable_count());

    let r = backend.begin_read().unwrap();
    // Newest-wins for overwritten keys.
    for i in 0u32..5 {
        assert_eq!(r.get(T, &i.to_be_bytes()).unwrap(), Some(b"gen2".to_vec()));
    }
    // Untouched-since-gen1 keys still read from the oldest generation.
    for i in 8u32..10 {
        assert_eq!(r.get(T, &i.to_be_bytes()).unwrap(), Some(b"gen1".to_vec()));
    }
    // Deleted keys are gone even though an older generation still has them.
    for i in 5u32..8 {
        assert_eq!(r.get(T, &i.to_be_bytes()).unwrap(), None);
    }
    // Keys only ever written in the newest generation.
    for i in 10u32..15 {
        assert_eq!(r.get(T, &i.to_be_bytes()).unwrap(), Some(b"gen2".to_vec()));
    }

    // A full range scan must reflect the same merged, deletion-aware view.
    let all = r.range(T, std::ops::Bound::Unbounded, std::ops::Bound::Unbounded).unwrap();
    assert_eq!(all.len(), 12, "0..5 (gen2) + 8..10 (gen1) + 10..15 (gen2) = 5+2+5 = 12 live keys");
}
