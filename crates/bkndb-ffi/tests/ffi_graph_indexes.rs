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
