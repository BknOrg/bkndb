use bkndb_core::test_util::{
    batch_sync_bulk_conformance_suite, db_batch_read_your_own_writes_suite, db_conformance_suite,
    db_read_tx_conformance_suite, db_write_tx_conformance_suite,
};
use bkndb_storage_redb::RedbStorageBackend;

#[test]
fn redb_backend_satisfies_db_conformance_suite() {
    let dir = tempfile::tempdir().unwrap();
    let backend = RedbStorageBackend::open(dir.path().join("db_test.bkndb")).unwrap();
    db_conformance_suite(backend);
}

#[test]
fn redb_backend_satisfies_db_write_tx_conformance_suite() {
    let dir = tempfile::tempdir().unwrap();
    let backend = RedbStorageBackend::open(dir.path().join("db_write_tx_test.bkndb")).unwrap();
    db_write_tx_conformance_suite(backend);
}

#[test]
fn redb_backend_satisfies_db_read_tx_conformance_suite() {
    let dir = tempfile::tempdir().unwrap();
    let backend = RedbStorageBackend::open(dir.path().join("db_read_tx_test.bkndb")).unwrap();
    db_read_tx_conformance_suite(backend);
}

#[test]
fn redb_backend_satisfies_db_ryow_conformance_suite() {
    let dir = tempfile::tempdir().unwrap();
    let backend = RedbStorageBackend::open(dir.path().join("db_ryow_test.bkndb")).unwrap();
    db_batch_read_your_own_writes_suite(backend);
}

#[test]
fn redb_backend_satisfies_batch_sync_bulk_conformance_suite() {
    let dir = tempfile::tempdir().unwrap();
    let backend = RedbStorageBackend::open(dir.path().join("batch_sync_test.bkndb")).unwrap();
    batch_sync_bulk_conformance_suite(backend);
}

#[test]
fn redb_backend_satisfies_sync_batch_upsert_suite() {
    let dir = tempfile::tempdir().unwrap();
    bkndb_core::test_util::sync_batch_upsert_suite(RedbStorageBackend::open(dir.path().join("sync_upsert.redb")).unwrap());
}

#[test]
fn redb_backend_satisfies_hybrid_query_suite() {
    let dir = tempfile::tempdir().unwrap();
    bkndb_core::test_util::hybrid_query_suite(RedbStorageBackend::open(dir.path().join("hybrid_query.redb")).unwrap());
}

#[test]
fn redb_backend_satisfies_db_stats_suite() {
    let dir = tempfile::tempdir().unwrap();
    bkndb_core::test_util::db_stats_suite(RedbStorageBackend::open(dir.path().join("stats.bkndb")).unwrap());
}

#[test]
fn redb_backend_satisfies_value_types_suite() {
    let dir = tempfile::tempdir().unwrap();
    bkndb_core::test_util::value_types_suite(bkndb_storage_redb::RedbStorageBackend::open(dir.path().join("types.bkndb")).unwrap());
}

#[test]
fn redb_backend_satisfies_sql_suite() {
    let dir = tempfile::tempdir().unwrap();
    bkndb_core::test_util::sql_suite(bkndb_storage_redb::RedbStorageBackend::open(dir.path().join("sql.bkndb")).unwrap());
}

#[test]
fn redb_backend_satisfies_graph_query_suite() {
    let dir = tempfile::tempdir().unwrap();
    bkndb_core::test_util::graph_query_suite(bkndb_storage_redb::RedbStorageBackend::open(dir.path().join("gq.bkndb")).unwrap());
}

#[test]
fn redb_backend_satisfies_search_suite() {
    let dir = tempfile::tempdir().unwrap();
    bkndb_core::test_util::search_suite(bkndb_storage_redb::RedbStorageBackend::open(dir.path().join("search.bkndb")).unwrap());
}
