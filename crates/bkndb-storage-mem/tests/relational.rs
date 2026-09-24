use bkndb_core::test_util::{
    relational_conformance_suite, relational_indexing_and_hybrid_suite,
    relational_write_tx_conformance_suite,
};
use bkndb_storage_mem::MemoryStorageBackend;

#[test]
fn mem_backend_satisfies_relational_conformance_suite() {
    let backend = MemoryStorageBackend::new();
    relational_conformance_suite(backend);
}

#[test]
fn mem_backend_satisfies_relational_write_tx_conformance_suite() {
    let backend = MemoryStorageBackend::new();
    relational_write_tx_conformance_suite(backend);
}

#[test]
fn mem_backend_satisfies_relational_indexing_and_hybrid_suite() {
    let backend = MemoryStorageBackend::new();
    relational_indexing_and_hybrid_suite(backend);
}

