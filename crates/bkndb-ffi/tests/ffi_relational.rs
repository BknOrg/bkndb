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
