use bkndb_core::test_util::conformance_suite;
use bkndb_storage_mem::MemoryStorageBackend;

#[test]
fn mem_backend_satisfies_conformance_suite() {
    let backend = MemoryStorageBackend::new();
    conformance_suite(&backend);
}
