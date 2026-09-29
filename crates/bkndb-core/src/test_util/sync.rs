//! `SyncBatch` bulk sync, upserts, linked edges and hybrid queries.
use super::*;

/// Conformance suite for bulk graph & relational operations and high-level SyncBatch execution,
/// verifying sequential ID reservation, bulk creation, and atomic rollback on failures.
#[cfg(all(feature = "graph", feature = "relational"))]
pub fn batch_sync_bulk_conformance_suite<B: StorageBackend>(backend: B) {
    use crate::BknError;
    use crate::db::{Db, SyncBatch};
    use crate::graph::{Direction, EdgeId, NodeId, Properties as GraphProperties};
    use crate::value::{PropValue, Properties};

    let db = Db::new(backend);

    // 1. Bulk Node Creation (create_nodes_bulk)
    let node_records: Vec<(&str, GraphProperties)> = (1..=100)
        .map(|i| {
            let mut props = GraphProperties::new();
            props.insert("idx".to_string(), PropValue::Int(i));
            ("Item", props)
        })
        .collect();

    let node_ids = db.graph().create_nodes_bulk(node_records).unwrap();
    assert_eq!(node_ids.len(), 100);
    // IDs must be consecutive starting from NodeId(1)
    for (i, id) in node_ids.iter().enumerate() {
        assert_eq!(*id, NodeId((i + 1) as u64));
        let record = db.graph().get_node(*id).unwrap().expect("node must exist");
        assert_eq!(
            record.properties.get("idx"),
            Some(&PropValue::Int((i + 1) as i64))
        );
    }

    // 2. Bulk Edge Creation (create_edges_bulk)
    let edge_records: Vec<(NodeId, &str, NodeId, GraphProperties)> = (0..99)
        .map(|i| {
            let mut props = GraphProperties::new();
            props.insert("seq".to_string(), PropValue::Int(i as i64));
            (node_ids[i], "next", node_ids[i + 1], props)
        })
        .collect();

    let edge_ids = db.graph().create_edges_bulk(edge_records).unwrap();
    assert_eq!(edge_ids.len(), 99);
    for (i, id) in edge_ids.iter().enumerate() {
        assert_eq!(*id, EdgeId((i + 1) as u64));
        let record = db.graph().get_edge(*id).unwrap().expect("edge must exist");
        assert_eq!(record.from, node_ids[i]);
        assert_eq!(record.to, node_ids[i + 1]);
    }

    // Traversal check: BFS from node_ids[0] to node_ids[99]
    let path = db
        .graph()
        .find_shortest_path(node_ids[0], node_ids[99], Direction::Out, Some(&["next"]))
        .unwrap()
        .expect("path should exist");
    assert_eq!(path.nodes().len(), 100);
    assert_eq!(path.edges().len(), 99);

    // 3. Bulk Relational Insert (insert_bulk) with auto-increment
    let auto_inc_rows: Vec<Properties> = (1..=50)
        .map(|i| {
            let mut props = Properties::new();
            props.insert("file_node_id".to_string(), PropValue::Int(i));
            props.insert("name".to_string(), PropValue::Str(format!("sym_{i}")));
            props
        })
        .collect();

    let rel = db.relational();
    let sym_table = rel.table(&HYBRID_SYMBOLS);
    let pks = sym_table.insert_bulk(auto_inc_rows).unwrap();
    assert_eq!(pks.len(), 50);
    for (i, pk) in pks.iter().enumerate() {
        assert_eq!(*pk, PropValue::Int((i + 1) as i64));
        let row = sym_table.get(pk).unwrap().expect("row must exist");
        assert_eq!(
            row.values.get("name"),
            Some(&PropValue::Str(format!("sym_{}", i + 1)))
        );
    }

    // 4. Bulk Relational Insert with PK (insert_with_pk_bulk)
    let explicit_rows: Vec<(PropValue, Properties)> = (1..=50)
        .map(|i| {
            let mut props = Properties::new();
            props.insert("path".to_string(), PropValue::Str(format!("file_{i}.rs")));
            props.insert("size".to_string(), PropValue::Int(i * 10));
            (PropValue::Int(i), props)
        })
        .collect();

    let file_table = rel.table(&HYBRID_FILES);
    file_table.insert_with_pk_bulk(explicit_rows).unwrap();
    for i in 1..=50 {
        let pk = PropValue::Int(i);
        let row = file_table.get(&pk).unwrap().expect("row must exist");
        assert_eq!(
            row.values.get("path"),
            Some(&PropValue::Str(format!("file_{i}.rs")))
        );
    }

    // 5. High-level SyncBatch execution
    let mut batch = SyncBatch::new();
    let start_n1 = NodeId(101);
    let start_n2 = NodeId(102);

    let mut p1 = GraphProperties::new();
    p1.insert(
        "tag".to_string(),
        PropValue::Str("batch_node_1".to_string()),
    );
    let mut p2 = GraphProperties::new();
    p2.insert(
        "tag".to_string(),
        PropValue::Str("batch_node_2".to_string()),
    );
    batch.add_node("SyncNode", p1);
    batch.add_node("SyncNode", p2);

    let mut ep = GraphProperties::new();
    ep.insert("kind".to_string(), PropValue::Str("sync_link".to_string()));
    batch.add_edge(start_n1, "sync_edge", start_n2, ep);

    let mut r1 = Properties::new();
    r1.insert("file_node_id".to_string(), PropValue::Int(101));
    r1.insert(
        "name".to_string(),
        PropValue::Str("sync_symbol".to_string()),
    );
    batch.add_row(&HYBRID_SYMBOLS, r1);

    let mut f1 = Properties::new();
    f1.insert(
        "path".to_string(),
        PropValue::Str("sync_file.rs".to_string()),
    );
    f1.insert("size".to_string(), PropValue::Int(999));
    batch.add_row_with_pk(&HYBRID_FILES, PropValue::Int(101), f1);

    let sync_res = db.sync_batch(batch).unwrap();
    assert_eq!(sync_res.node_ids, vec![start_n1, start_n2]);
    assert_eq!(sync_res.edge_ids.len(), 1);
    assert_eq!(sync_res.relational_pks.len(), 1);
    assert_eq!(sync_res.relational_pks[0].len(), 1);

    // Verify hybrid join on the newly synced items
    let joined = db
        .read_tx(|tx| tx.join_nodes_with_table(&[start_n1], &HYBRID_FILES))
        .unwrap();
    assert_eq!(joined.len(), 1);
    assert_eq!(
        joined[0].row.as_ref().unwrap().values.get("path"),
        Some(&PropValue::Str("sync_file.rs".to_string()))
    );

    // 6. Atomic Rollback Verification on SyncBatch
    let mut bad_batch = SyncBatch::new();
    bad_batch.add_node("Ghost", GraphProperties::new());
    // Referencing completely invalid destination NodeId(999999)
    bad_batch.add_edge(
        NodeId(101),
        "dangling",
        NodeId(999999),
        GraphProperties::new(),
    );
    let mut ghost_row = Properties::new();
    ghost_row.insert("file_node_id".to_string(), PropValue::Int(777));
    ghost_row.insert(
        "name".to_string(),
        PropValue::Str("ghost_symbol".to_string()),
    );
    bad_batch.add_row(&HYBRID_SYMBOLS, ghost_row);

    let err = db.sync_batch(bad_batch);
    assert!(matches!(err, Err(BknError::NotFound)));

    // Verify no ghost node leaked: NodeId(103) must NOT exist
    assert!(db.graph().get_node(NodeId(103)).unwrap().is_none());
    // Verify ghost symbol was NOT inserted
    let found = sym_table.select_prefix("name", "ghost_symbol").unwrap();
    assert_eq!(found.len(), 0);
}

/// `sync_batch` upserts relational rows, so re-applying a batch is safe.
pub fn sync_batch_upsert_suite<B: StorageBackend>(backend: B) {
    use crate::db::{Db, SyncBatch};
    use crate::relational::{ColumnKind, ColumnSchema, TableSchema};
    use crate::value::{PropValue, Properties};

    let db = Db::new(backend);
    let files = TableSchema::builder("files")
        .column(ColumnSchema::new("path", ColumnKind::Str))
        .column(ColumnSchema::new("lang", ColumnKind::Str))
        .primary_key("path")
        .index("lang")
        .build()
        .unwrap();
    let row = |path: &str, lang: &str| -> Properties {
        [("path".to_string(), PropValue::from(path)), ("lang".to_string(), PropValue::from(lang))]
            .into_iter()
            .collect()
    };

    let mut first = SyncBatch::new();
    first.add_rows(&files, [row("a.rs", "rust"), row("b.py", "python")]);
    db.sync_batch(first).unwrap();

    let mut again = SyncBatch::new();
    again.add_rows(&files, [row("a.rs", "rust"), row("b.py", "rust")]);
    again.add_row_with_pk(&files, "c.go".into(), row("c.go", "go"));
    db.sync_batch(again).unwrap();

    let rel = db.relational();
    let t = rel.table(&files);
    assert_eq!(t.select().count().unwrap(), 3);
    assert_eq!(t.select().where_eq("lang", "rust").count().unwrap(), 2);
    assert_eq!(t.select().where_eq("lang", "python").count().unwrap(), 0, "old index entry replaced");
}

/// Hybrid joins driven by relational queries, and foreign-key joins over an
/// unindexed column.
pub fn hybrid_query_suite<B: StorageBackend>(backend: B) {
    use crate::db::Db;
    use crate::relational::{col, ColumnKind, ColumnSchema, Query, TableSchema};
    use crate::value::{PropValue, Properties};

    let db = Db::new(backend);
    let (a, b) = db
        .write_tx(|tx| {
            let mut g = tx.graph();
            let a = g.create_node("File", Properties::new())?;
            let b = g.create_node("File", Properties::new())?;
            Ok((a, b))
        })
        .unwrap();

    // `file_node` is deliberately not indexed.
    let symbols = TableSchema::builder("symbols")
        .column(ColumnSchema::new("id", ColumnKind::Int))
        .column(ColumnSchema::new("name", ColumnKind::Str))
        .column(ColumnSchema::new("file_node", ColumnKind::Int))
        .primary_key("id")
        .auto_increment()
        .build()
        .unwrap();
    let rel = db.relational();
    rel.create_table(&symbols).unwrap();
    let t = rel.table_named("symbols").unwrap();
    for (name, node) in [("main", a), ("helper", a), ("parse", b)] {
        let mut p = Properties::new();
        p.insert("name".into(), name.into());
        p.insert("file_node".into(), PropValue::Int(node.0 as i64));
        t.insert(p).unwrap();
    }

    db.read_tx(|tx| {
        let joined = tx.join_nodes_by_column(&[a, b], &symbols, "file_node")?;
        assert_eq!(joined[0].rows.len(), 2);
        assert_eq!(joined[1].rows.len(), 1);
        assert!(joined[0].node.is_some());

        let hits = tx.query_rows_with_nodes(&symbols, &Query::new().filter(col("name").starts_with("p")), "file_node")?;
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0.values.get("name"), Some(&PropValue::from("parse")));
        assert!(hits[0].1.is_some(), "row should resolve to its graph node");
        Ok(())
    })
    .unwrap();
}

/// Sync batches whose edges refer to nodes created in the same batch.
pub fn sync_batch_linked_edges_suite<B: StorageBackend>(backend: B) {
    use crate::db::{Db, NodeRef, SyncBatch};
    use crate::value::Properties;

    let db = Db::new(backend);
    let existing = db.graph().create_node("Repo", Properties::new()).unwrap();

    let mut batch = SyncBatch::new();
    batch
        .add_node("File", Properties::new())
        .add_node("Function", Properties::new())
        .add_linked_edge(NodeRef::New(0), "DEFINES", NodeRef::New(1), Properties::new())
        .add_linked_edge(existing, "CONTAINS", NodeRef::New(0), Properties::new());
    let res = db.sync_batch(batch).unwrap();
    let (file, func) = (res.node_ids[0], res.node_ids[1]);
    assert_eq!(res.edge_ids.len(), 2);
    let g = db.graph();
    assert_eq!(g.neighbors_out(file, "DEFINES").unwrap()[0].0, func);
    assert_eq!(g.neighbors_out(existing, "CONTAINS").unwrap()[0].0, file);

    // A bad reference fails the whole batch atomically.
    let before = g.nodes_by_label("File").unwrap();
    let mut bad = SyncBatch::new();
    bad.add_node("File", Properties::new())
        .add_linked_edge(NodeRef::New(0), "X", NodeRef::New(5), Properties::new());
    assert!(db.sync_batch(bad).is_err());
    assert_eq!(g.nodes_by_label("File").unwrap(), before);
}
