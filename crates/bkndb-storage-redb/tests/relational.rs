use bkndb_core::test_util::{relational_conformance_suite, relational_write_tx_conformance_suite};
use bkndb_storage_redb::RedbStorageBackend;

#[test]
fn redb_backend_satisfies_relational_conformance_suite() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("relational_test.bkndb");
    let backend = RedbStorageBackend::open(&path).unwrap();
    relational_conformance_suite(backend);
}

#[test]
fn redb_backend_satisfies_relational_write_tx_conformance_suite() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("relational_write_tx_test.bkndb");
    let backend = RedbStorageBackend::open(&path).unwrap();
    relational_write_tx_conformance_suite(backend);
}
