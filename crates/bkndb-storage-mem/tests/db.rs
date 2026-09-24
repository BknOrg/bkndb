use bkndb_core::test_util::{
    batch_sync_bulk_conformance_suite, db_batch_read_your_own_writes_suite, db_conformance_suite,
    db_read_tx_conformance_suite, db_write_tx_conformance_suite,
};
use bkndb_storage_mem::MemoryStorageBackend;

#[test]
fn mem_backend_satisfies_db_conformance_suite() {
    db_conformance_suite(MemoryStorageBackend::new());
}

#[test]
fn mem_backend_satisfies_db_write_tx_conformance_suite() {
    db_write_tx_conformance_suite(MemoryStorageBackend::new());
}

#[test]
fn mem_backend_satisfies_db_read_tx_conformance_suite() {
    db_read_tx_conformance_suite(MemoryStorageBackend::new());
}

#[test]
fn mem_backend_satisfies_db_ryow_conformance_suite() {
    db_batch_read_your_own_writes_suite(MemoryStorageBackend::new());
}

#[test]
fn mem_backend_satisfies_batch_sync_bulk_conformance_suite() {
    batch_sync_bulk_conformance_suite(MemoryStorageBackend::new());
}

