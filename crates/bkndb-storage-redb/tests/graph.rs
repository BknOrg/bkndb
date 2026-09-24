use bkndb_core::test_util::{graph_advanced_algorithms_suite, graph_conformance_suite};
use bkndb_core::{StorageBackend, StorageWriteTx, TableSpec};
use bkndb_storage_redb::RedbStorageBackend;

#[test]
fn redb_backend_satisfies_graph_conformance_suite() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m2_test.bkndb");
    let backend = RedbStorageBackend::open(&path).unwrap();
    graph_conformance_suite(backend);
}

#[test]
fn redb_backend_satisfies_graph_advanced_algorithms_suite() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m2_adv_test.bkndb");
    let backend = RedbStorageBackend::open(&path).unwrap();
    graph_advanced_algorithms_suite(backend);
}

/// Cascade delete (and every other GraphDb write) relies on redb's write
/// transaction being all-or-nothing: if it's dropped without `commit()`,
/// none of its writes take effect. This can't be tested meaningfully
/// against `MemoryStorageBackend`, whose M1 write tx mutates the shared
/// map in place and treats `commit()` as a no-op with no rollback path —
/// such a test would pass there for the wrong reason. Only redb has real
/// transaction semantics to verify.
#[test]
fn uncommitted_write_tx_leaves_no_trace() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m3_rollback_test.bkndb");
    let backend = RedbStorageBackend::open(&path).unwrap();
    const T: TableSpec = TableSpec("nodes");

    {
        let mut wtx = backend.begin_write().unwrap();
        wtx.put(T, b"baseline", b"present").unwrap();
        wtx.commit().unwrap();
    }

    {
        let mut wtx = backend.begin_write().unwrap();
        wtx.put(T, b"orphan", b"should-not-persist").unwrap();
        wtx.delete(T, b"baseline").unwrap();
        // Dropped here without calling `commit()` — mirrors a cascade
        // delete that fails partway through and never reaches its own
        // `commit()` call.
    }

    let rtx = backend.begin_read().unwrap();
    use bkndb_core::StorageReadTx;
    assert_eq!(
        rtx.get(T, b"baseline").unwrap(),
        Some(b"present".to_vec()),
        "baseline write from the committed tx must survive"
    );
    assert_eq!(
        rtx.get(T, b"orphan").unwrap(),
        None,
        "put from the uncommitted tx must not persist"
    );
}
