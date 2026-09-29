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


#[test]
fn mem_backend_satisfies_sync_batch_upsert_suite() {
    bkndb_core::test_util::sync_batch_upsert_suite(MemoryStorageBackend::new());
}

#[test]
fn mem_backend_satisfies_hybrid_query_suite() {
    bkndb_core::test_util::hybrid_query_suite(MemoryStorageBackend::new());
}

#[test]
fn mem_backend_satisfies_db_stats_suite() {
    bkndb_core::test_util::db_stats_suite(MemoryStorageBackend::new());
}

#[test]
fn mem_backend_satisfies_value_types_suite() {
    bkndb_core::test_util::value_types_suite(bkndb_storage_mem::MemoryStorageBackend::new());
}

#[test]
fn mem_backend_satisfies_sql_suite() {
    bkndb_core::test_util::sql_suite(bkndb_storage_mem::MemoryStorageBackend::new());
}

#[test]
fn mem_backend_satisfies_graph_query_suite() {
    bkndb_core::test_util::graph_query_suite(bkndb_storage_mem::MemoryStorageBackend::new());
}

#[test]
fn mem_backend_satisfies_search_suite() {
    bkndb_core::test_util::search_suite(bkndb_storage_mem::MemoryStorageBackend::new());
}

#[test]
fn mem_backend_satisfies_ann_suite() {
    bkndb_core::test_util::ann_suite(bkndb_storage_mem::MemoryStorageBackend::new());
}
