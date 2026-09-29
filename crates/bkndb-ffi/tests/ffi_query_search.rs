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
    let near = db.search_vector("d".into(), "emb".into(), vec![0.0, 1.0], 1, FfiVectorMetric::Cosine, en_only.clone(), false, None).unwrap();
    assert_eq!(near[0].row.pk, FfiPropValue::Int(2));
    assert!((near[0].score - 1.0).abs() < 1e-6);
    assert!(db.drop_fulltext_index("d".into(), "body".into()).unwrap());

    // Approximate index: same answers on a tiny table, listed with its parameters.
    assert!(db.create_vector_index("d".into(), "emb".into(), FfiVectorMetric::Cosine, 8, 32).unwrap());
    assert!(!db.create_vector_index("d".into(), "emb".into(), FfiVectorMetric::Cosine, 8, 32).unwrap());
    let info = db.list_vector_indexes("d".into()).unwrap();
    assert_eq!((info[0].column.as_str(), info[0].metric, info[0].m, info[0].dimensions, info[0].vectors), ("emb", FfiVectorMetric::Cosine, 8, Some(2), 3));
    let approx = db.search_vector("d".into(), "emb".into(), vec![0.5, 0.9], 3, FfiVectorMetric::Cosine, vec![], false, Some(16)).unwrap();
    let exact = db.search_vector("d".into(), "emb".into(), vec![0.5, 0.9], 3, FfiVectorMetric::Cosine, vec![], true, None).unwrap();
    assert_eq!(approx, exact);
    assert!(db.drop_vector_index("d".into(), "emb".into()).unwrap());
    assert!(db.list_vector_indexes("d".into()).unwrap().is_empty());
}
