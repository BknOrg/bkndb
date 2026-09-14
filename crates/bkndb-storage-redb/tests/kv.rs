use bkndb_core::test_util::{kv_conformance_suite, kv_reserved_name_conformance_suite};
use bkndb_storage_redb::RedbStorageBackend;

#[test]
fn redb_backend_satisfies_kv_conformance_suite() {
    let dir = tempfile::tempdir().unwrap();
    let backend = RedbStorageBackend::open(dir.path().join("kv_test.bkndb")).unwrap();
    kv_conformance_suite(backend);
}

#[test]
fn redb_backend_rejects_reserved_kv_table_names() {
    let dir = tempfile::tempdir().unwrap();
    let backend = RedbStorageBackend::open(dir.path().join("kv_reserved_test.bkndb")).unwrap();
    kv_reserved_name_conformance_suite(backend);
}
