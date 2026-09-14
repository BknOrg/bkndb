use bkndb_core::{StorageBackend, StorageReadTx, StorageWriteTx, TableSpec};
use bkndb_storage_lsm::LsmStorageBackend;

const T: TableSpec = TableSpec("t");

fn db_path(dir: &tempfile::TempDir) -> std::path::PathBuf {
    dir.path().join("test.bkndb")
}

#[test]
fn committed_data_survives_reopen_without_graceful_shutdown() {
    let dir = tempfile::tempdir().unwrap();
    let path = db_path(&dir);
    {
        let backend = LsmStorageBackend::open(&path).unwrap();
        let mut w = backend.begin_write().unwrap();
        w.put(T, b"a", b"1").unwrap();
        w.put(T, b"b", b"2").unwrap();
        w.commit().unwrap();
        // `backend` is dropped here with no explicit close/shutdown call —
        // there shouldn't be one required for durability of a commit that
        // already returned `Ok`.
    }

    let reopened = LsmStorageBackend::open(&path).unwrap();
    let r = reopened.begin_read().unwrap();
    assert_eq!(r.get(T, b"a").unwrap(), Some(b"1".to_vec()));
    assert_eq!(r.get(T, b"b").unwrap(), Some(b"2".to_vec()));
}

#[test]
fn uncommitted_write_does_not_survive_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = db_path(&dir);
    {
        let backend = LsmStorageBackend::open(&path).unwrap();
        let mut w = backend.begin_write().unwrap();
        w.put(T, b"a", b"1").unwrap();
        w.commit().unwrap();

        let mut w2 = backend.begin_write().unwrap();
        w2.put(T, b"never-committed", b"x").unwrap();
        // dropped without commit()
    }

    let reopened = LsmStorageBackend::open(&path).unwrap();
    let r = reopened.begin_read().unwrap();
    assert_eq!(r.get(T, b"a").unwrap(), Some(b"1".to_vec()));
    assert_eq!(r.get(T, b"never-committed").unwrap(), None);
}

#[test]
fn corrupted_trailing_wal_frame_is_recovered_around() {
    let dir = tempfile::tempdir().unwrap();
    let path = db_path(&dir);
    {
        let backend = LsmStorageBackend::open(&path).unwrap();
        let mut w = backend.begin_write().unwrap();
        w.put(T, b"good", b"1").unwrap();
        w.commit().unwrap();
    }

    // Hand-corrupt the single container file by appending a bogus trailing
    // frame directly onto its current end-of-file, simulating a crash
    // mid-write to the log. With no separate flush having happened yet,
    // the active WAL region's tail *is* the file's current end-of-file.
    {
        use std::fs::OpenOptions;
        use std::io::Write;
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(&9999u32.to_be_bytes()).unwrap();
        f.write_all(&0u32.to_be_bytes()).unwrap();
        f.write_all(b"not enough bytes").unwrap();
    }

    let reopened = LsmStorageBackend::open(&path).unwrap();
    let r = reopened.begin_read().unwrap();
    assert_eq!(r.get(T, b"good").unwrap(), Some(b"1".to_vec()));
}
