use bkndb_core::test_util::{graph_write_tx_conformance_suite, relational_reserved_name_conformance_suite};
use bkndb_storage_redb::RedbStorageBackend;

#[test]
fn redb_backend_rejects_reserved_relational_schema_name() {
    let dir = tempfile::tempdir().unwrap();
    let backend = RedbStorageBackend::open(dir.path().join("reserved_test.bkndb")).unwrap();
    relational_reserved_name_conformance_suite(backend);
}

#[test]
fn redb_backend_satisfies_graph_write_tx_conformance_suite() {
    let dir = tempfile::tempdir().unwrap();
    let backend = RedbStorageBackend::open(dir.path().join("graph_write_tx_test.bkndb")).unwrap();
    graph_write_tx_conformance_suite(backend);
}
