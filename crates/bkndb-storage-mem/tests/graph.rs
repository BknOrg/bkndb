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


#[test]
fn mem_backend_satisfies_graph_index_suite() {
    bkndb_core::test_util::graph_index_suite(MemoryStorageBackend::new());
}

#[test]
fn mem_backend_satisfies_graph_weighted_path_suite() {
    bkndb_core::test_util::graph_weighted_path_suite(MemoryStorageBackend::new());
}

#[test]
fn mem_backend_satisfies_sync_batch_linked_edges_suite() {
    bkndb_core::test_util::sync_batch_linked_edges_suite(MemoryStorageBackend::new());
}
