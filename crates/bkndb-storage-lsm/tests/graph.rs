use bkndb_core::test_util::graph_conformance_suite;
use bkndb_storage_lsm::LsmStorageBackend;

#[test]
fn lsm_backend_satisfies_graph_conformance_suite() {
    let dir = tempfile::tempdir().unwrap();
    let backend = LsmStorageBackend::open(dir.path().join("test.bkndb")).unwrap();
    graph_conformance_suite(backend);
}
