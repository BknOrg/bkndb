use bkndb_core::test_util::{kv_conformance_suite, kv_reserved_name_conformance_suite};
use bkndb_storage_mem::MemoryStorageBackend;

#[test]
fn mem_backend_satisfies_kv_conformance_suite() {
    kv_conformance_suite(MemoryStorageBackend::new());
}

#[test]
fn mem_backend_rejects_reserved_kv_table_names() {
    kv_reserved_name_conformance_suite(MemoryStorageBackend::new());
}
