use bkndb_core::test_util::{graph_advanced_algorithms_suite, graph_conformance_suite};
use bkndb_storage_mem::MemoryStorageBackend;

#[test]
fn mem_backend_satisfies_graph_conformance_suite() {
    let backend = MemoryStorageBackend::new();
    graph_conformance_suite(backend);
}

#[test]
fn mem_backend_satisfies_graph_advanced_algorithms_suite() {
    let backend = MemoryStorageBackend::new();
    graph_advanced_algorithms_suite(backend);
}

