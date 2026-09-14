use bkndb_core::test_util::{relational_conformance_suite, relational_write_tx_conformance_suite};
use bkndb_storage_lsm::LsmStorageBackend;

#[test]
fn lsm_backend_satisfies_relational_conformance_suite() {
    let dir = tempfile::tempdir().unwrap();
    let backend = LsmStorageBackend::open(dir.path().join("test.bkndb")).unwrap();
    relational_conformance_suite(backend);
}

#[test]
fn lsm_backend_satisfies_relational_write_tx_conformance_suite() {
    let dir = tempfile::tempdir().unwrap();
    let backend = LsmStorageBackend::open(dir.path().join("write_tx_test.bkndb")).unwrap();
    relational_write_tx_conformance_suite(backend);
}
