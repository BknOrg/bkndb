use bkndb_core::test_util::conformance_suite;
use bkndb_storage_redb::RedbStorageBackend;

#[test]
fn redb_backend_satisfies_conformance_suite() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m1_test.bkndb");
    let backend = RedbStorageBackend::open(&path).unwrap();
    conformance_suite(&backend);
}
