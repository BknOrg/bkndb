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
    let db_path = temp_dir.path().join("mobile_test.redb");
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
