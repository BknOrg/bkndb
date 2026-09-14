use bkndb_core::test_util::{graph_write_tx_conformance_suite, relational_reserved_name_conformance_suite};
use bkndb_storage_mem::MemoryStorageBackend;

#[test]
fn mem_backend_rejects_reserved_relational_schema_name() {
    relational_reserved_name_conformance_suite(MemoryStorageBackend::new());
}

#[test]
fn mem_backend_satisfies_graph_write_tx_conformance_suite() {
    graph_write_tx_conformance_suite(MemoryStorageBackend::new());
}
