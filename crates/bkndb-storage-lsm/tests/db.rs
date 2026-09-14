use bkndb_core::test_util::{
    db_batch_read_your_own_writes_suite, db_conformance_suite, db_read_tx_conformance_suite,
    db_write_tx_conformance_suite,
};
use bkndb_storage_lsm::LsmStorageBackend;

#[test]
fn lsm_backend_satisfies_db_conformance_suite() {
    let dir = tempfile::tempdir().unwrap();
    let backend = LsmStorageBackend::open(dir.path().join("db_test.bkndb")).unwrap();
    db_conformance_suite(backend);
}

#[test]
fn lsm_backend_satisfies_db_write_tx_conformance_suite() {
    let dir = tempfile::tempdir().unwrap();
    let backend = LsmStorageBackend::open(dir.path().join("db_write_tx_test.bkndb")).unwrap();
    db_write_tx_conformance_suite(backend);
}

#[test]
fn lsm_backend_satisfies_db_read_tx_conformance_suite() {
    let dir = tempfile::tempdir().unwrap();
    let backend = LsmStorageBackend::open(dir.path().join("db_read_tx_test.bkndb")).unwrap();
    db_read_tx_conformance_suite(backend);
}

#[test]
fn lsm_backend_satisfies_db_ryow_conformance_suite() {
    let dir = tempfile::tempdir().unwrap();
    let backend = LsmStorageBackend::open(dir.path().join("db_ryow_test.bkndb")).unwrap();
    db_batch_read_your_own_writes_suite(backend);
}

