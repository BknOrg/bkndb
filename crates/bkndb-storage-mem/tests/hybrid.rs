//! Proves the graph and relational layers interoperate over one shared
//! backend instance: a relational row's primary key is set to an existing
//! graph node's id, queried back through the relational layer, and used to
//! drive a graph traversal from that same node — the concrete mechanism
//! behind BknDb being "hybrid" rather than two unrelated engines.
//!
//! This lives in `bkndb-storage-mem`'s test suite (not `bkndb-core`'s)
//! because `bkndb-core` cannot depend on any of its own backend crates
//! (that would be a dependency cycle) but still needs a concrete
//! `StorageBackend` to run against.

use std::sync::Arc;

use bkndb_core::graph::{GraphDb, Properties as GraphProperties};
use bkndb_core::relational::{ColumnDef, ColumnKind, RelSchema, RelationalDb};
use bkndb_core::value::{PropValue, Properties};
use bkndb_storage_mem::MemoryStorageBackend;

static FILES: RelSchema = RelSchema {
    name: "files",
    columns: &[
        ColumnDef {
            name: "node_id",
            kind: ColumnKind::Int,
        },
        ColumnDef {
            name: "path",
            kind: ColumnKind::Str,
        },
    ],
    primary_key: "node_id",
    auto_increment_pk: false,
    indexed_columns: &["path"],
};

#[test]
fn relational_row_and_graph_node_share_one_id_over_one_backend() {
    let backend = Arc::new(MemoryStorageBackend::new());
    let graph = GraphDb::from_arc(backend.clone());
    let relational = RelationalDb::from_arc(backend);

    // Create the graph nodes first (this is where the shared id space comes
    // from), then attach a relational row to the same id — the documented
    // "explicit two-step" hybrid convention, no implicit dual-write.
    let a_id = graph.create_node("File", GraphProperties::new()).unwrap();
    let b_id = graph.create_node("File", GraphProperties::new()).unwrap();
    graph.create_edge(a_id, "imports", b_id, GraphProperties::new()).unwrap();

    let table = relational.table(&FILES);
    let mut a_row = Properties::new();
    a_row.insert("path".to_string(), PropValue::Str("src/a.rs".to_string()));
    table.insert_with_pk(PropValue::Int(a_id.0 as i64), a_row).unwrap();

    let mut b_row = Properties::new();
    b_row.insert("path".to_string(), PropValue::Str("src/b.rs".to_string()));
    table.insert_with_pk(PropValue::Int(b_id.0 as i64), b_row).unwrap();

    // Query the relational layer to recover a node id from a path...
    let found = table
        .select()
        .where_eq("path", PropValue::Str("src/a.rs".to_string()))
        .run()
        .unwrap();
    assert_eq!(found.len(), 1);
    let PropValue::Int(recovered_id) = found[0].pk else {
        panic!("expected an Int primary key");
    };

    // ...and use that recovered id to drive a graph traversal, proving the
    // id really is the same `NodeId` `create_node` returned.
    let recovered_node_id = bkndb_core::graph::NodeId(recovered_id as u64);
    assert_eq!(recovered_node_id, a_id);

    let traversal = graph
        .traversal()
        .start(recovered_node_id)
        .outgoing("imports")
        .max_depth(1)
        .run()
        .unwrap();
    let neighbors: Vec<_> = traversal.iter().map(|n| n.node).collect();
    assert_eq!(neighbors, vec![a_id, b_id]);
}

#[test]
fn relational_and_graph_in_single_unified_transaction() {
    use bkndb_core::db::Db;

    let db = Db::new(MemoryStorageBackend::new());

    // Single atomic write transaction creating graph nodes + edges + relational metadata
    let (a_id, b_id, _edge_id) = db
        .write_tx(|batch| {
            let a = batch.graph().create_node("File", GraphProperties::new())?;
            let b = batch.graph().create_node("File", GraphProperties::new())?;
            let e = batch.graph().create_edge(a, "imports", b, GraphProperties::new())?;

            let mut a_row = Properties::new();
            a_row.insert("path".to_string(), PropValue::Str("src/main.rs".to_string()));
            batch
                .relational()
                .table(&FILES)
                .insert_with_pk(PropValue::Int(a.0 as i64), a_row)?;

            let mut b_row = Properties::new();
            b_row.insert("path".to_string(), PropValue::Str("src/lib.rs".to_string()));
            batch
                .relational()
                .table(&FILES)
                .insert_with_pk(PropValue::Int(b.0 as i64), b_row)?;

            Ok::<_, bkndb_core::BknError>((a, b, e))
        })
        .unwrap();

    // Single consistent read transaction querying relational and traversing graph
    db.read_tx(|tx| {
        let found = tx
            .relational()
            .table(&FILES)
            .select_eq("path", &PropValue::Str("src/main.rs".to_string()))?;
        assert_eq!(found.len(), 1);
        let PropValue::Int(recovered_id) = found[0].pk else {
            panic!("expected Int pk");
        };
        let node_id = bkndb_core::graph::NodeId(recovered_id as u64);
        assert_eq!(node_id, a_id);

        let neighbors = tx.graph().neighbors_out(node_id, "imports")?;
        assert_eq!(neighbors.len(), 1);
        assert_eq!(neighbors[0].0, b_id);

        assert_eq!(tx.graph().out_degree(node_id)?, 1);
        assert_eq!(tx.graph().in_degree(b_id)?, 1);

        Ok::<_, bkndb_core::BknError>(())
    })
    .unwrap();
}

