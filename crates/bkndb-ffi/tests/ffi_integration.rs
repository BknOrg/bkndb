use bkndb_ffi::{
    BknDbEngine, FfiDirection, FfiEdgeInput, FfiNodeInput, FfiPropValue, FfiSyncBatch,
};
use std::collections::HashMap;

#[test]
fn test_in_memory_crud_and_traversal() {
    let engine = BknDbEngine::in_memory().expect("failed to init in-memory engine");

    let mut props1 = HashMap::new();
    props1.insert("name".to_string(), FfiPropValue::Str("Alice".to_string()));
    props1.insert("age".to_string(), FfiPropValue::Int(30));
    props1.insert("active".to_string(), FfiPropValue::Bool(true));

    let n1 = engine.create_node("Person".to_string(), props1).unwrap();

    let mut props2 = HashMap::new();
    props2.insert("name".to_string(), FfiPropValue::Str("Bob".to_string()));
    let n2 = engine.create_node("Person".to_string(), props2).unwrap();

    // Get nodes
    let node1 = engine.get_node(n1).unwrap().expect("node 1 not found");
    assert_eq!(node1.id, n1);
    assert_eq!(node1.label, "Person");
    assert_eq!(
        node1.properties.get("name"),
        Some(&FfiPropValue::Str("Alice".to_string()))
    );
    assert_eq!(node1.properties.get("age"), Some(&FfiPropValue::Int(30)));
    assert_eq!(
        node1.properties.get("active"),
        Some(&FfiPropValue::Bool(true))
    );

    // Create edge
    let mut e_props = HashMap::new();
    e_props.insert("since".to_string(), FfiPropValue::Int(2024));
    let e1 = engine
        .create_edge(n1, "KNOWS".to_string(), n2, e_props)
        .unwrap();

    let edge1 = engine.get_edge(e1).unwrap().expect("edge 1 not found");
    assert_eq!(edge1.id, e1);
    assert_eq!(edge1.from, n1);
    assert_eq!(edge1.to, n2);
    assert_eq!(edge1.edge_type, "KNOWS");
    assert_eq!(
        edge1.properties.get("since"),
        Some(&FfiPropValue::Int(2024))
    );

    // Neighbors
    let out_neighbors = engine.neighbors_out(n1, "KNOWS".to_string()).unwrap();
    assert_eq!(out_neighbors.len(), 1);
    assert_eq!(out_neighbors[0].node_id, n2);
    assert_eq!(out_neighbors[0].edge_id, e1);

    let in_neighbors = engine.neighbors_in(n2, "KNOWS".to_string()).unwrap();
    assert_eq!(in_neighbors.len(), 1);
    assert_eq!(in_neighbors[0].node_id, n1);
    assert_eq!(in_neighbors[0].edge_id, e1);

    // Delete node n2
    engine.delete_node(n2).unwrap();
    assert!(engine.get_node(n2).unwrap().is_none());
    assert!(engine.get_edge(e1).unwrap().is_none());
}

#[test]
fn test_bulk_and_bfs_and_hubs() {
    let engine = BknDbEngine::in_memory().unwrap();

    // Create 5 nodes: A, B, C, D, E
    let nodes = vec![
        FfiNodeInput {
            label: "A".to_string(),
            properties: HashMap::new(),
        },
        FfiNodeInput {
            label: "B".to_string(),
            properties: HashMap::new(),
        },
        FfiNodeInput {
            label: "C".to_string(),
            properties: HashMap::new(),
        },
        FfiNodeInput {
            label: "D".to_string(),
            properties: HashMap::new(),
        },
        FfiNodeInput {
            label: "E".to_string(),
            properties: HashMap::new(),
        },
    ];
    let nids = engine.create_nodes_bulk(nodes).unwrap();
    assert_eq!(nids.len(), 5);

    let (a, b, c, d, e) = (nids[0], nids[1], nids[2], nids[3], nids[4]);

    // Edges:
    // A -> B -> C -> D (3 hops)
    // A -> E -> D      (2 hops)
    let edges = vec![
        FfiEdgeInput {
            from: a,
            edge_type: "CONNECTS".to_string(),
            to: b,
            properties: HashMap::new(),
        },
        FfiEdgeInput {
            from: b,
            edge_type: "CONNECTS".to_string(),
            to: c,
            properties: HashMap::new(),
        },
        FfiEdgeInput {
            from: c,
            edge_type: "CONNECTS".to_string(),
            to: d,
            properties: HashMap::new(),
        },
        FfiEdgeInput {
            from: a,
            edge_type: "SHORTCUT".to_string(),
            to: e,
            properties: HashMap::new(),
        },
        FfiEdgeInput {
            from: e,
            edge_type: "SHORTCUT".to_string(),
            to: d,
            properties: HashMap::new(),
        },
    ];
    let eids = engine.create_edges_bulk(edges).unwrap();
    assert_eq!(eids.len(), 5);

    // Shortest path with all edge types: should take shortcut of length 2
    let path = engine
        .find_shortest_path(a, d, FfiDirection::Out, None)
        .unwrap()
        .expect("path should exist");
    assert_eq!(path.node_ids, vec![a, e, d]);
    assert_eq!(path.steps.len(), 3);
    assert_eq!(path.steps[0].node_id, a);
    assert_eq!(path.steps[1].node_id, e);
    assert_eq!(path.steps[2].node_id, d);

    // Shortest path filtering only CONNECTS: must take 3 hops
    let path_connects = engine
        .find_shortest_path(a, d, FfiDirection::Out, Some(vec!["CONNECTS".to_string()]))
        .unwrap()
        .expect("path should exist");
    assert_eq!(path_connects.node_ids, vec![a, b, c, d]);

    // Top hubs
    let hubs = engine.top_hubs(2, FfiDirection::Both, None).unwrap();
    assert_eq!(hubs.len(), 2);
    // a has 2 outgoing, d has 2 incoming. Degrees should be at least 2.
    assert!(hubs[0].degree >= 2);
}

#[test]
fn test_cascade_delete() {
    let engine = BknDbEngine::in_memory().unwrap();

    let root = engine
        .create_node("Dir".to_string(), HashMap::new())
        .unwrap();
    let sub1 = engine
        .create_node("Dir".to_string(), HashMap::new())
        .unwrap();
    let sub2 = engine
        .create_node("File".to_string(), HashMap::new())
        .unwrap();
    let leaf = engine
        .create_node("File".to_string(), HashMap::new())
        .unwrap();

    engine
        .create_edge(root, "CONTAINS".to_string(), sub1, HashMap::new())
        .unwrap();
    engine
        .create_edge(root, "CONTAINS".to_string(), sub2, HashMap::new())
        .unwrap();
    engine
        .create_edge(sub1, "CONTAINS".to_string(), leaf, HashMap::new())
        .unwrap();

    let deleted = engine.cascade_delete(root, "CONTAINS".to_string()).unwrap();
    assert_eq!(deleted.len(), 4);
    assert!(engine.get_node(root).unwrap().is_none());
    assert!(engine.get_node(sub1).unwrap().is_none());
    assert!(engine.get_node(sub2).unwrap().is_none());
    assert!(engine.get_node(leaf).unwrap().is_none());
}

#[test]
fn test_sync_batch() {
    let engine = BknDbEngine::in_memory().unwrap();

    let mut batch = FfiSyncBatch::default();
    batch.nodes.push(FfiNodeInput {
        label: "SyncNode1".to_string(),
        properties: HashMap::new(),
    });
    batch.nodes.push(FfiNodeInput {
        label: "SyncNode2".to_string(),
        properties: HashMap::new(),
    });

    let res = engine.sync_batch(batch).unwrap();
    assert_eq!(res.node_ids.len(), 2);

    let n1 = res.node_ids[0];
    let n2 = res.node_ids[1];

    let mut batch2 = FfiSyncBatch::default();
    batch2.edges.push(FfiEdgeInput {
        from: n1,
        edge_type: "SYNC_EDGE".to_string(),
        to: n2,
        properties: HashMap::new(),
    });

    let res2 = engine.sync_batch(batch2).unwrap();
    assert_eq!(res2.edge_ids.len(), 1);
    assert!(engine.get_edge(res2.edge_ids[0]).unwrap().is_some());
}

#[test]
fn test_on_disk_persistence() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("mobile_test.bkndb");
    let path_str = db_path.to_str().unwrap().to_string();

    let node_id;
    let edge_id;
    {
        let engine = BknDbEngine::open(path_str.clone()).unwrap();
        let mut props = HashMap::new();
        props.insert(
            "title".to_string(),
            FfiPropValue::Str("Mobile Persistence".to_string()),
        );
        let n1 = engine.create_node("Document".to_string(), props).unwrap();
        let n2 = engine
            .create_node("Tag".to_string(), HashMap::new())
            .unwrap();

        let e = engine
            .create_edge(n1, "TAGGED".to_string(), n2, HashMap::new())
            .unwrap();
        node_id = n1;
        edge_id = e;
    }

    // Reopen and verify persistence
    {
        let engine = BknDbEngine::open(path_str).unwrap();
        let node = engine.get_node(node_id).unwrap().expect("node persisted");
        assert_eq!(node.label, "Document");
        assert_eq!(
            node.properties.get("title"),
            Some(&FfiPropValue::Str("Mobile Persistence".to_string()))
        );

        let edge = engine.get_edge(edge_id).unwrap().expect("edge persisted");
        assert_eq!(edge.edge_type, "TAGGED");
    }
}

#[test]
fn test_second_open_of_same_file_reports_database_locked() {
    let temp_dir = tempfile::tempdir().unwrap();
    let path = temp_dir.path().join("locked.bkndb").to_str().unwrap().to_string();
    let first = BknDbEngine::open(path.clone()).unwrap();
    match BknDbEngine::open(path.clone()) {
        Err(bkndb_ffi::FfiBknError::DatabaseLocked { .. }) => {}
        Err(e) => panic!("expected DatabaseLocked, got {e}"),
        Ok(_) => panic!("second open must fail while the first engine is alive"),
    }
    drop(first);
    BknDbEngine::open(path).expect("reopen after the first engine is dropped");
}

#[test]
fn test_shortest_path_from_missing_node_to_itself_is_none() {
    let engine = BknDbEngine::in_memory().unwrap();
    assert!(engine.find_shortest_path(999, 999, FfiDirection::Out, None).unwrap().is_none());
}

mod phase2 {
    use std::collections::HashMap;

    use bkndb_ffi::{
        BknDbEngine, FfiAgg, FfiAggFunc, FfiBknError, FfiColumn, FfiColumnKind, FfiDirection, FfiExprNode, FfiExprOp,
        FfiOrder, FfiPropValue, FfiQuery, FfiSyncBatch, FfiTableRows, FfiTableSchema,
    };

    fn col(name: &str, kind: FfiColumnKind) -> FfiColumn {
        FfiColumn { name: name.into(), kind, nullable: true, unique: false, default_value: None }
    }

    fn people_schema() -> FfiTableSchema {
        FfiTableSchema {
            name: "people".into(),
            columns: vec![
                col("id", FfiColumnKind::Int),
                FfiColumn { unique: true, nullable: false, ..col("email", FfiColumnKind::Str) },
                col("city", FfiColumnKind::Str),
                FfiColumn { default_value: Some(FfiPropValue::Int(0)), ..col("age", FfiColumnKind::Int) },
            ],
            primary_key: "id".into(),
            auto_increment: true,
            indexed_columns: vec!["city".into()],
        }
    }

    fn row(pairs: &[(&str, FfiPropValue)]) -> HashMap<String, FfiPropValue> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }

    fn s(v: &str) -> FfiPropValue {
        FfiPropValue::Str(v.into())
    }

    fn leaf(op: FfiExprOp, column: &str, values: Vec<FfiPropValue>) -> FfiExprNode {
        FfiExprNode { op, column: Some(column.into()), values, children: vec![] }
    }

    fn seeded() -> std::sync::Arc<BknDbEngine> {
        let db = BknDbEngine::in_memory().unwrap();
        assert!(db.create_table(people_schema()).unwrap());
        for (email, city, age) in [("a@x", "Jakarta", 30), ("b@x", "Bandung", 25), ("c@x", "Jakarta", 41)] {
            db.insert("people".into(), row(&[("email", s(email)), ("city", s(city)), ("age", FfiPropValue::Int(age))]))
                .unwrap();
        }
        db
    }

    #[test]
    fn relational_crud_queries_and_aggregates() {
        let db = seeded();
        // The stored schema is normalized: the pk is NOT NULL and UNIQUE
        // columns are indexed.
        let tables = db.list_tables().unwrap();
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].indexed_columns, vec!["city".to_string(), "email".to_string()]);
        assert!(!tables[0].columns[0].nullable);

        // city = Jakarta AND age >= 35  (post-order: 0, 1, And[0,1])
        let q = FfiQuery {
            filter: vec![
                leaf(FfiExprOp::Eq, "city", vec![s("Jakarta")]),
                leaf(FfiExprOp::Ge, "age", vec![FfiPropValue::Int(35)]),
                FfiExprNode { op: FfiExprOp::And, column: None, values: vec![], children: vec![0, 1] },
            ],
            ..Default::default()
        };
        let rows = db.select("people".into(), q).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].values.get("email"), Some(&s("c@x")));

        let ordered = db
            .select(
                "people".into(),
                FfiQuery {
                    order_by: vec![FfiOrder { column: "age".into(), descending: true }],
                    limit: Some(2),
                    columns: Some(vec!["age".into()]),
                    ..Default::default()
                },
            )
            .unwrap();
        let ages: Vec<_> = ordered.iter().map(|r| r.values.get("age").cloned()).collect();
        assert_eq!(ages, vec![Some(FfiPropValue::Int(41)), Some(FfiPropValue::Int(30))]);
        assert!(!ordered[0].values.contains_key("email"), "projection");

        let groups = db
            .aggregate(
                "people".into(),
                FfiQuery::default(),
                vec!["city".into()],
                vec![FfiAgg { func: FfiAggFunc::Count, column: None }, FfiAgg { func: FfiAggFunc::Avg, column: Some("age".into()) }],
            )
            .unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[1].group, vec![s("Jakarta")]);
        assert_eq!(groups[1].values, vec![FfiPropValue::Int(2), FfiPropValue::Float(35.5)]);

        let jakarta = FfiQuery { filter: vec![leaf(FfiExprOp::Eq, "city", vec![s("Jakarta")])], ..Default::default() };
        assert_eq!(db.update_rows("people".into(), jakarta.clone(), row(&[("city", s("Depok"))])).unwrap(), 2);
        assert_eq!(db.count("people".into(), jakarta).unwrap(), 0);
        let depok = FfiQuery { filter: vec![leaf(FfiExprOp::Eq, "city", vec![s("Depok")])], ..Default::default() };
        assert_eq!(db.delete_rows("people".into(), depok).unwrap(), 2);
        assert_eq!(db.count("people".into(), FfiQuery::default()).unwrap(), 1);

        // Constraint and argument errors come back typed.
        let dup = db.insert("people".into(), row(&[("email", s("b@x"))]));
        assert!(matches!(dup, Err(FfiBknError::ConstraintViolation { .. })), "{dup:?}");
        let bad_tree = FfiQuery {
            filter: vec![FfiExprNode { op: FfiExprOp::Not, column: None, values: vec![], children: vec![] }],
            ..Default::default()
        };
        assert!(matches!(db.select("people".into(), bad_tree), Err(FfiBknError::InvalidArgument { .. })));
        assert!(matches!(db.select("nope".into(), FfiQuery::default()), Err(FfiBknError::TableNotFound { .. })));

        db.create_index("people".into(), "age".into()).unwrap();
        assert!(db.table_schema("people".into()).unwrap().unwrap().indexed_columns.contains(&"age".to_string()));
        assert!(db.drop_table("people".into()).unwrap());
        assert!(db.list_tables().unwrap().is_empty());
    }

    #[test]
    fn transactions_commit_rollback_and_abort() {
        let db = seeded();

        // Commit: reads inside the transaction see its own writes; outside
        // readers see the old state until commit.
        let tx = db.begin_transaction().unwrap();
        let n = tx.create_node("Person".into(), HashMap::new()).unwrap();
        let pk = tx.insert("people".into(), row(&[("email", s("d@x")), ("city", s("Medan"))])).unwrap();
        assert!(tx.get_node(n).unwrap().is_some());
        assert!(tx.get_row("people".into(), pk.clone()).unwrap().is_some());
        assert!(db.get_node(n).unwrap().is_none(), "uncommitted write must be invisible outside");
        assert!(matches!(db.create_node("X".into(), HashMap::new()), Err(FfiBknError::TransactionInProgress)));
        assert!(matches!(db.begin_transaction(), Err(FfiBknError::TransactionInProgress)));
        tx.commit().unwrap();
        assert!(!tx.is_active());
        assert!(matches!(tx.commit(), Err(FfiBknError::TransactionClosed)));
        assert!(db.get_node(n).unwrap().is_some());
        assert!(db.get_row("people".into(), pk).unwrap().is_some());

        // Rollback discards everything.
        let tx = db.begin_transaction().unwrap();
        let gone = tx.create_node("Ghost".into(), HashMap::new()).unwrap();
        tx.rollback().unwrap();
        assert!(db.get_node(gone).unwrap().is_none());

        // A failed operation aborts the transaction: commit refuses and the
        // earlier successful write is rolled back too.
        let tx = db.begin_transaction().unwrap();
        let partial = tx.create_node("Partial".into(), HashMap::new()).unwrap();
        assert!(tx.insert("people".into(), row(&[("email", s("a@x"))])).is_err());
        assert!(matches!(tx.create_node("More".into(), HashMap::new()), Err(FfiBknError::TransactionAborted { .. })));
        assert!(matches!(tx.commit(), Err(FfiBknError::TransactionAborted { .. })));
        assert!(db.get_node(partial).unwrap().is_none());

        // Dropping an unfinished transaction rolls it back and frees the engine.
        {
            let tx = db.begin_transaction().unwrap();
            tx.create_node("Dropped".into(), HashMap::new()).unwrap();
        }
        db.create_node("AfterDrop".into(), HashMap::new()).unwrap();
    }

    #[test]
    fn transaction_on_disk_is_durable_and_close_releases_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tx.bkndb").to_str().unwrap().to_string();
        let node;
        {
            let db = BknDbEngine::open(path.clone()).unwrap();
            let tx = db.begin_transaction().unwrap();
            node = tx.create_node("Doc".into(), HashMap::new()).unwrap();
            tx.commit().unwrap();
            db.close().unwrap();
            assert!(db.is_closed());
            assert!(matches!(db.get_node(node), Err(FfiBknError::DatabaseClosed)));
            // The file lock is released by close(), even while `db` is alive.
            let again = BknDbEngine::open(path.clone()).unwrap();
            assert!(again.get_node(node).unwrap().is_some());
            again.compact().unwrap();
            assert!(again.get_node(node).unwrap().is_some());
        }
    }

    #[test]
    fn graph_additions_and_sync_rows() {
        let db = seeded();
        let a = db.create_node("File".into(), row(&[("path", s("a.rs")), ("tmp", FfiPropValue::Bool(true))])).unwrap();
        let b = db.create_node("File".into(), HashMap::new()).unwrap();
        let c = db.create_node("Dir".into(), HashMap::new()).unwrap();
        let ab = db.create_edge(a, "IMPORTS".into(), b, HashMap::new()).unwrap();
        db.create_edge(c, "CONTAINS".into(), a, HashMap::new()).unwrap();

        db.update_node_properties(a, row(&[("lines", FfiPropValue::Int(10))]), vec!["tmp".into()]).unwrap();
        let props = db.get_node(a).unwrap().unwrap().properties;
        assert_eq!(props.get("lines"), Some(&FfiPropValue::Int(10)));
        assert!(!props.contains_key("tmp"));
        assert!(matches!(db.update_node_properties(999, HashMap::new(), vec![]), Err(FfiBknError::NotFound)));

        let both = db.neighbors(a, FfiDirection::Both, None).unwrap();
        assert_eq!(both.len(), 2);
        assert_eq!(db.degree(a, FfiDirection::Out, Some("IMPORTS".into())).unwrap(), 1);
        let hits = db.traverse(c, FfiDirection::Out, 5, None, None).unwrap();
        assert_eq!(hits.iter().map(|h| h.node_id).collect::<Vec<_>>(), vec![c, a, b]);
        assert_eq!(hits[2].depth, 2);

        assert!(db.delete_edge(ab).unwrap());
        assert!(!db.delete_edge(ab).unwrap());
        assert_eq!(db.degree(a, FfiDirection::Out, None).unwrap(), 0);

        let batch = FfiSyncBatch {
            rows: vec![FfiTableRows {
                table: "people".into(),
                rows: vec![row(&[("id", FfiPropValue::Int(1)), ("email", s("a@x")), ("city", s("Bogor"))])],
            }],
            ..Default::default()
        };
        let res = db.sync_batch(batch.clone()).unwrap();
        assert_eq!(res.row_pks, vec![vec![FfiPropValue::Int(1)]]);
        db.sync_batch(batch).unwrap(); // re-applying is an upsert, not a duplicate
        let r = db.get_row("people".into(), FfiPropValue::Int(1)).unwrap().unwrap();
        assert_eq!(r.values.get("city"), Some(&s("Bogor")));
        assert_eq!(db.count("people".into(), FfiQuery::default()).unwrap(), 3);
    }
}

mod phase3 {
    use std::collections::HashMap;

    use bkndb_ffi::{
        BknDbEngine, FfiBknError, FfiDirection, FfiLinkedEdgeInput, FfiNodeInput, FfiNodeRef, FfiPropValue,
        FfiPropertyIndex, FfiSyncBatch,
    };

    fn props(pairs: &[(&str, FfiPropValue)]) -> HashMap<String, FfiPropValue> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }

    #[test]
    fn label_and_property_indexes() {
        let db = BknDbEngine::in_memory().unwrap();
        let a = db.create_node("Person".into(), props(&[("age", FfiPropValue::Int(30))])).unwrap();
        let b = db.create_node("Person".into(), props(&[("age", FfiPropValue::Int(40))])).unwrap();
        db.create_node("City".into(), HashMap::new()).unwrap();

        assert_eq!(db.nodes_by_label("Person".into()).unwrap(), vec![a, b]);
        assert_eq!(db.count_nodes("City".into()).unwrap(), 1);
        assert_eq!(db.find_nodes("Person".into(), "age".into(), FfiPropValue::Int(40)).unwrap(), vec![b]);

        assert!(db.create_node_index("Person".into(), "age".into()).unwrap());
        assert_eq!(
            db.list_node_indexes().unwrap(),
            vec![FfiPropertyIndex { label: "Person".into(), property: "age".into() }]
        );
        db.update_node_properties(a, props(&[("age", FfiPropValue::Int(40))]), vec![]).unwrap();
        assert_eq!(db.find_nodes("Person".into(), "age".into(), FfiPropValue::Int(40)).unwrap(), vec![a, b]);

        let tx = db.begin_transaction().unwrap();
        let c = tx.create_node("Person".into(), props(&[("age", FfiPropValue::Int(40))])).unwrap();
        assert_eq!(tx.find_nodes("Person".into(), "age".into(), FfiPropValue::Int(40)).unwrap(), vec![a, b, c]);
        assert_eq!(tx.nodes_by_label("Person".into()).unwrap().len(), 3);
        tx.rollback().unwrap();

        db.rebuild_graph_indexes().unwrap();
        assert!(db.drop_node_index("Person".into(), "age".into()).unwrap());
        assert!(db.list_node_indexes().unwrap().is_empty());
    }

    #[test]
    fn weighted_paths() {
        let db = BknDbEngine::in_memory().unwrap();
        let n: Vec<u64> = (0..3).map(|_| db.create_node("N".into(), HashMap::new()).unwrap()).collect();
        let w = |v: f64| props(&[("km", FfiPropValue::Float(v))]);
        db.create_edge(n[0], "R".into(), n[1], w(1.0)).unwrap();
        db.create_edge(n[1], "R".into(), n[2], w(1.0)).unwrap();
        db.create_edge(n[0], "R".into(), n[2], w(9.0)).unwrap();
        let best = db
            .find_weighted_path(n[0], n[2], FfiDirection::Out, None, "km".into(), 1.0)
            .unwrap()
            .unwrap();
        assert_eq!(best.cost, 2.0);
        assert_eq!(best.path.node_ids, n);
        let bad = db.find_weighted_path(n[0], n[2], FfiDirection::Out, None, "km".into(), -1.0);
        assert!(matches!(bad, Err(FfiBknError::Encoding { .. })));
    }

    #[test]
    fn sync_batch_linked_edges() {
        let db = BknDbEngine::in_memory().unwrap();
        let repo = db.create_node("Repo".into(), HashMap::new()).unwrap();
        let batch = FfiSyncBatch {
            nodes: vec![
                FfiNodeInput { label: "File".into(), properties: HashMap::new() },
                FfiNodeInput { label: "Fn".into(), properties: HashMap::new() },
            ],
            edges: vec![],
            rows: vec![],
            linked_edges: vec![
                FfiLinkedEdgeInput {
                    from: FfiNodeRef::Existing { id: repo },
                    edge_type: "CONTAINS".into(),
                    to: FfiNodeRef::New { index: 0 },
                    properties: HashMap::new(),
                },
                FfiLinkedEdgeInput {
                    from: FfiNodeRef::New { index: 0 },
                    edge_type: "DEFINES".into(),
                    to: FfiNodeRef::New { index: 1 },
                    properties: HashMap::new(),
                },
            ],
        };
        let res = db.sync_batch(batch.clone()).unwrap();
        assert_eq!(res.edge_ids.len(), 2);
        let hits = db.traverse(repo, FfiDirection::Out, 5, None, None).unwrap();
        assert_eq!(hits.iter().map(|h| h.node_id).collect::<Vec<_>>(), vec![repo, res.node_ids[0], res.node_ids[1]]);

        let mut bad = batch;
        bad.linked_edges[1].to = FfiNodeRef::New { index: 7 };
        assert!(db.sync_batch(bad).is_err());
        assert_eq!(db.count_nodes("File".into()).unwrap(), 1, "failed batch rolled back");
    }
}

mod phase4 {
    use std::collections::HashMap;

    use bkndb_ffi::{
        BknDbEngine, FfiBknError, FfiColumn, FfiColumnKind, FfiLsmOptions, FfiPropValue, FfiTableCount, FfiTableSchema,
    };

    fn items_schema() -> FfiTableSchema {
        FfiTableSchema {
            name: "items".into(),
            columns: vec![
                FfiColumn { name: "id".into(), kind: FfiColumnKind::Int, nullable: true, unique: false, default_value: None },
                FfiColumn { name: "name".into(), kind: FfiColumnKind::Str, nullable: true, unique: false, default_value: None },
            ],
            primary_key: "id".into(),
            auto_increment: true,
            indexed_columns: vec![],
        }
    }

    fn seed(db: &BknDbEngine, rows: usize) {
        db.create_table(items_schema()).unwrap();
        let rows = (0..rows)
            .map(|i| HashMap::from([("name".to_string(), FfiPropValue::Str(format!("item-{i}")))]))
            .collect();
        db.insert_many("items".into(), rows).unwrap();
        let a = db.create_node("N".into(), HashMap::new()).unwrap();
        let b = db.create_node("N".into(), HashMap::new()).unwrap();
        db.create_edge(a, "E".into(), b, HashMap::new()).unwrap();
    }

    #[test]
    fn stats_backup_and_verify_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let opts = FfiLsmOptions {
            memtable_flush_bytes: 4096,
            compaction_trigger_files: 50,
            block_size_bytes: Some(512),
            compression: Some(true),
        };
        let db = BknDbEngine::open_with_options(dir.path().join("a.bkndb").to_string_lossy().into(), opts).unwrap();
        seed(&db, 300);

        let stats = db.stats().unwrap();
        assert_eq!((stats.nodes, stats.edges), (2, 1));
        assert_eq!(stats.tables, vec![FfiTableCount { table: "items".into(), rows: 300 }]);
        let storage = stats.storage.expect("on-disk databases report storage figures");
        assert!(storage.file_bytes > 0 && storage.sstable_count >= 1);

        let report = db.verify_integrity().unwrap();
        assert!(report.blocks_verified > 0);

        // Backup works while a transaction is open, and doesn't include it.
        let tx = db.begin_transaction().unwrap();
        tx.insert("items".into(), HashMap::from([("name".to_string(), FfiPropValue::Str("pending".into()))])).unwrap();
        let backup = dir.path().join("b.bkndb").to_string_lossy().to_string();
        db.backup(backup.clone()).unwrap();
        tx.commit().unwrap();
        assert!(matches!(db.backup(backup.clone()), Err(FfiBknError::Backend { .. })), "never overwrites");

        let copy = BknDbEngine::open(backup).unwrap();
        let copy_stats = copy.stats().unwrap();
        assert_eq!(copy_stats.tables[0].rows, 300);
        assert_eq!(copy_stats.storage.unwrap().reclaimable_bytes, 0);
        assert_eq!(db.stats().unwrap().tables[0].rows, 301);
    }

    #[test]
    fn in_memory_databases_report_what_applies() {
        let db = BknDbEngine::in_memory().unwrap();
        seed(&db, 3);
        let stats = db.stats().unwrap();
        assert_eq!(stats.tables[0].rows, 3);
        assert!(stats.storage.is_none());
        assert_eq!(db.verify_integrity().unwrap().sstables_checked, 0);
        assert!(matches!(db.backup("x.bkndb".into()), Err(FfiBknError::InvalidArgument { .. })));
        db.close().unwrap();
        assert!(matches!(db.stats(), Err(FfiBknError::DatabaseClosed)));
    }
}

mod phase5 {
    use std::collections::HashMap;

    use bkndb_ffi::{BknDbEngine, FfiBknError, FfiPropValue};

    #[test]
    fn new_value_kinds_round_trip() {
        let db = BknDbEngine::in_memory().unwrap();
        let nested = FfiPropValue::Map(HashMap::from([(
            "tags".to_string(),
            FfiPropValue::List(vec![FfiPropValue::Str("a".into()), FfiPropValue::Timestamp(-5)]),
        )]));
        let props = HashMap::from([
            ("at".to_string(), FfiPropValue::Timestamp(1_700_000_000_000_000)),
            ("id".to_string(), FfiPropValue::Uuid { hi: u64::MAX, lo: 7 }),
            ("doc".to_string(), nested),
        ]);
        let n = db.create_node("Doc".into(), props.clone()).unwrap();
        assert_eq!(db.get_node(n).unwrap().unwrap().properties, props);
    }

    #[test]
    fn sql_and_graph_queries() {
        let db = BknDbEngine::in_memory().unwrap();
        db.sql("CREATE TABLE t (id INT PRIMARY KEY AUTOINCREMENT, name TEXT, at TIMESTAMP)".into(), vec![], None).unwrap();
        let ins = db
            .sql(
                "INSERT INTO t (name, at) VALUES (?, :at), ('b', NULL)".into(),
                vec![FfiPropValue::Str("a".into())],
                Some(HashMap::from([("at".to_string(), FfiPropValue::Timestamp(10))])),
            )
            .unwrap();
        assert_eq!(ins.affected, 2);
        let sel = db.sql("SELECT name, at FROM t ORDER BY id".into(), vec![], None).unwrap();
        assert_eq!(sel.columns, vec!["name", "at"]);
        assert_eq!(sel.rows[0], vec![FfiPropValue::Str("a".into()), FfiPropValue::Timestamp(10)]);
        assert!(matches!(db.sql("SELEKT".into(), vec![], None), Err(FfiBknError::InvalidQuery { .. })));

        let a = db.create_node("P".into(), HashMap::from([("n".to_string(), FfiPropValue::Str("a".into()))])).unwrap();
        let b = db.create_node("P".into(), HashMap::from([("n".to_string(), FfiPropValue::Str("b".into()))])).unwrap();
        db.create_edge(a, "R".into(), b, HashMap::new()).unwrap();
        let g = db
            .graph_query(
                "MATCH (x:P {n: $n})-[:R]->(y) RETURN y.n".into(),
                vec![],
                Some(HashMap::from([("n".to_string(), FfiPropValue::Str("a".into()))])),
            )
            .unwrap();
        assert_eq!(g.rows, vec![vec![FfiPropValue::Str("b".into())]]);

        // In a transaction: sees its own writes; a SQL error aborts it.
        let tx = db.begin_transaction().unwrap();
        tx.sql("INSERT INTO t (name) VALUES ('c')".into(), vec![], None).unwrap();
        assert_eq!(tx.sql("SELECT COUNT(*) FROM t".into(), vec![], None).unwrap().rows, vec![vec![FfiPropValue::Int(3)]]);
        assert!(tx.graph_query("MATCH (x) RETURN count(*)".into(), vec![], None).is_ok());
        tx.rollback().unwrap();
        assert_eq!(db.sql("SELECT COUNT(*) FROM t".into(), vec![], None).unwrap().rows, vec![vec![FfiPropValue::Int(2)]]);
    }

    #[test]
    fn fulltext_and_vector_search() {
        use bkndb_ffi::{FfiExprNode, FfiExprOp, FfiVectorMetric};
        let db = BknDbEngine::in_memory().unwrap();
        db.sql("CREATE TABLE d (id INT PRIMARY KEY, body TEXT, lang TEXT, emb LIST)".into(), vec![], None).unwrap();
        for (id, body, lang, emb) in [(1, "graph database engine", "en", [1.0, 0.0]), (2, "basis data graf", "id", [0.0, 1.0]), (3, "graph of graphs", "en", [0.6, 0.8])] {
            db.sql(
                "INSERT INTO d VALUES (?, ?, ?, ?)".into(),
                vec![
                    FfiPropValue::Int(id),
                    FfiPropValue::Str(body.into()),
                    FfiPropValue::Str(lang.into()),
                    FfiPropValue::List(emb.iter().map(|x| FfiPropValue::Float(*x)).collect()),
                ],
                None,
            )
            .unwrap();
        }
        assert!(db.create_fulltext_index("d".into(), "body".into()).unwrap());
        assert_eq!(db.list_fulltext_indexes("d".into()).unwrap(), vec!["body"]);
        let hits = db.search_text("d".into(), "body".into(), "graph*".into(), 10, false, vec![]).unwrap();
        assert_eq!(hits.iter().map(|h| h.row.pk.clone()).collect::<Vec<_>>(), vec![FfiPropValue::Int(3), FfiPropValue::Int(1)]);
        let en_only = vec![FfiExprNode { op: FfiExprOp::Eq, column: Some("lang".into()), values: vec![FfiPropValue::Str("id".into())], children: vec![] }];
        let near = db.search_vector("d".into(), "emb".into(), vec![0.0, 1.0], 1, FfiVectorMetric::Cosine, en_only).unwrap();
        assert_eq!(near[0].row.pk, FfiPropValue::Int(2));
        assert!((near[0].score - 1.0).abs() < 1e-6);
        assert!(db.drop_fulltext_index("d".into(), "body".into()).unwrap());
    }
}
