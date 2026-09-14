use std::io::{Read, Write};

use bkndb_core::{StorageBackend, StorageReadTx, StorageWriteTx, TableSpec};
use bkndb_storage_lsm::{LsmOptions, LsmStorageBackend};

const T: TableSpec = TableSpec("t");

fn tiny_flush_options() -> LsmOptions {
    LsmOptions {
        memtable_flush_bytes: 64,
        compaction_trigger_files: 1000, // compaction triggered manually in these tests
        sparse_index_interval: 2,
    }
}

#[test]
fn database_is_exactly_one_file_no_sibling_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.bkndb");
    let backend = LsmStorageBackend::open_with_options(&path, tiny_flush_options()).unwrap();

    for i in 0u32..20 {
        let mut w = backend.begin_write().unwrap();
        w.put(T, &i.to_be_bytes(), format!("v{i}").as_bytes()).unwrap();
        w.commit().unwrap();
    }

    assert!(path.is_file(), "the database must be a single regular file, not a directory");

    let siblings: Vec<_> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(siblings, vec![path.file_name().unwrap().to_owned()], "no sibling files (manifest/wal/sst) should exist alongside the single .bkndb file");
}

#[test]
fn compaction_shrinks_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.bkndb");
    let backend = LsmStorageBackend::open_with_options(&path, tiny_flush_options()).unwrap();

    // Enough churn (overwrites across many small flushes) to guarantee
    // dead bytes accumulate before compaction.
    for round in 0..5 {
        for i in 0u32..20 {
            let mut w = backend.begin_write().unwrap();
            w.put(T, &i.to_be_bytes(), format!("round{round}-v{i}-padding-to-grow-the-value").as_bytes()).unwrap();
            w.commit().unwrap();
        }
    }
    assert!(backend.sstable_count() > 1, "the tiny flush threshold should have produced multiple SSTable generations");

    let size_before = std::fs::metadata(&path).unwrap().len();
    backend.force_compact().unwrap();
    let size_after = std::fs::metadata(&path).unwrap().len();

    assert!(
        size_after < size_before,
        "compaction must shrink the file by reclaiming dead WAL/SSTable bytes: before={size_before}, after={size_after}"
    );
}

#[test]
fn concurrent_reader_survives_compactions_rename() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.bkndb");
    let backend = LsmStorageBackend::open_with_options(&path, tiny_flush_options()).unwrap();

    for i in 0u32..20 {
        let mut w = backend.begin_write().unwrap();
        w.put(T, &i.to_be_bytes(), format!("v{i}").as_bytes()).unwrap();
        w.commit().unwrap();
    }
    assert!(backend.sstable_count() > 1);

    // Hold a read snapshot open across the compaction.
    let old_reader = backend.begin_read().unwrap();

    backend.force_compact().unwrap();
    assert_eq!(backend.sstable_count(), 1);

    // The old snapshot must still see correct data — its `SstableHandle`s
    // hold their own already-open file handles, independent of the path
    // that just got renamed over.
    for i in 0u32..20 {
        assert_eq!(old_reader.get(T, &i.to_be_bytes()).unwrap(), Some(format!("v{i}").into_bytes()));
    }

    // A fresh read tx after compaction sees the same, now-merged state.
    let new_reader = backend.begin_read().unwrap();
    for i in 0u32..20 {
        assert_eq!(new_reader.get(T, &i.to_be_bytes()).unwrap(), Some(format!("v{i}").into_bytes()));
    }
}

#[test]
fn truncated_copy_of_a_post_flush_file_never_panics_or_returns_wrong_data() {
    // The double-buffered header lives at a *fixed, low* offset (bytes
    // 0..128), not appended at the growing end of the file — so unlike a
    // WAL frame, truncating bytes off the *tail* of an already-fully-
    // written file can never revert the header back to an older value
    // (the header bytes it copies are already the final, fully-updated
    // ones). What a tail truncation *can* do is cut into the SSTable or
    // manifest blob the (already-updated) header points at. This test's
    // guarantee is therefore narrower than "always recovers a valid
    // state": opening such a truncated copy must either (a) succeed and
    // show a fully self-consistent state (every pre-flush key present —
    // since a flush always serializes the *whole* memtable, including
    // every pre-flush key, into one SSTable atomically, there is no
    // partial-flush state to observe), or (b) fail cleanly with an `Err`
    // — never panic, hang, or return silently-wrong data.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.bkndb");
    let backend = LsmStorageBackend::open_with_options(&path, tiny_flush_options()).unwrap();

    for i in 0u32..3 {
        let mut w = backend.begin_write().unwrap();
        w.put(T, &i.to_be_bytes(), b"before").unwrap();
        w.commit().unwrap();
    }
    let len_before = std::fs::metadata(&path).unwrap().len();

    // One more commit large enough to push the memtable over the tiny
    // flush threshold, forcing a flush (new SSTable blob + manifest blob +
    // header write) inside this single commit.
    {
        let mut w = backend.begin_write().unwrap();
        w.put(T, b"trigger", b"this-value-is-padded-to-force-a-flush-threshold-crossing").unwrap();
        w.commit().unwrap();
    }
    drop(backend);
    let len_after = std::fs::metadata(&path).unwrap().len();
    assert!(len_after > len_before, "the flush-triggering commit must have grown the file");

    let mut full_bytes = Vec::new();
    std::fs::File::open(&path).unwrap().read_to_end(&mut full_bytes).unwrap();

    let cut_points = [len_before + 1, (len_before + len_after) / 2, len_after - 1];
    for &cut in &cut_points {
        let truncated_path = dir.path().join(format!("truncated-{cut}.bkndb"));
        {
            let mut f = std::fs::File::create(&truncated_path).unwrap();
            f.write_all(&full_bytes[..cut as usize]).unwrap();
        }

        match LsmStorageBackend::open(&truncated_path) {
            Ok(recovered) => {
                let r = recovered.begin_read().unwrap();
                for i in 0u32..3 {
                    assert_eq!(
                        r.get(T, &i.to_be_bytes()).unwrap(),
                        Some(b"before".to_vec()),
                        "if open() succeeds at all, every pre-flush key must be present, cut at {cut}"
                    );
                }
            }
            Err(_) => {
                // A cut that lands inside the blob the (already-updated)
                // header points at is expected to fail cleanly here — see
                // the test's doc comment above.
            }
        }
    }
}
