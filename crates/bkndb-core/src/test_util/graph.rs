//! Graph conformance suites: CRUD, traversal, algorithms, write transactions.
use super::*;

/// Shared conformance suite for the graph layer (M2 CRUD, M3 cascade
/// delete + property mutation, M4 traversal), exercised identically
/// against every `StorageBackend` implementation.
#[cfg(feature = "graph")]
pub fn graph_conformance_suite<B: StorageBackend>(backend: B) {
    use crate::BknError;
    use crate::graph::{Direction, EdgeId, GraphDb, NodeId, Properties};

    let db = GraphDb::new(backend);

    // --- M2: CRUD, neighbors, degree ---
    let a = db.create_node("Fn", Properties::new()).unwrap();
    let b = db.create_node("Fn", Properties::new()).unwrap();
    let c = db.create_node("Fn", Properties::new()).unwrap();
    let isolated = db.create_node("Fn", Properties::new()).unwrap();

    assert!(db.get_node(a).unwrap().is_some());
    assert!(db.get_node(NodeId(9999)).unwrap().is_none());

    // dangling from/to must be rejected, not silently create orphan edges
    assert!(matches!(
        db.create_edge(NodeId(9999), "calls", a, Properties::new()),
        Err(BknError::NotFound)
    ));

    let e_ab = db.create_edge(a, "calls", b, Properties::new()).unwrap();
    let e_ac = db.create_edge(a, "imports", c, Properties::new()).unwrap();

    let out_calls = db.neighbors_out(a, "calls").unwrap();
    assert_eq!(out_calls, vec![(b, e_ab)]);
    let in_calls = db.neighbors_in(b, "calls").unwrap();
    assert_eq!(in_calls, vec![(a, e_ab)]);

    assert_eq!(db.out_degree(a).unwrap(), 2);
    assert_eq!(db.in_degree(a).unwrap(), 0);
    assert_eq!(db.out_degree(isolated).unwrap(), 0);
    assert_eq!(db.in_degree(isolated).unwrap(), 0);

    // --- M3: update_edge_properties ---
    db.update_edge_properties(e_ab, |props| {
        props.insert("hit".to_string(), crate::graph::PropValue::Bool(true));
    })
    .unwrap();
    let updated = db.get_edge(e_ab).unwrap().unwrap();
    assert_eq!(
        updated.properties.get("hit"),
        Some(&crate::graph::PropValue::Bool(true))
    );
    let untouched = db.get_edge(e_ac).unwrap().unwrap();
    assert!(untouched.properties.is_empty());

    assert!(matches!(
        db.update_edge_properties(EdgeId(424242), |_| {}),
        Err(BknError::NotFound)
    ));

    // --- M3: cascade delete ---
    // chain: a --calls--> b --calls--> c, plus a self-loop on b.
    let e_bc = db.create_edge(b, "calls", c, Properties::new()).unwrap();
    let e_self = db.create_edge(b, "recurses", b, Properties::new()).unwrap();

    db.delete_node(b).unwrap();

    assert!(db.get_node(b).unwrap().is_none());
    assert!(db.get_edge(e_ab).unwrap().is_none());
    assert!(db.get_edge(e_bc).unwrap().is_none());
    assert!(db.get_edge(e_self).unwrap().is_none());
    assert!(db.neighbors_out(a, "calls").unwrap().is_empty());
    assert!(db.get_node(a).unwrap().is_some());
    assert!(db.get_node(c).unwrap().is_some());
    // a's non-calls edge to c must survive b's deletion untouched.
    assert_eq!(db.neighbors_out(a, "imports").unwrap(), vec![(c, e_ac)]);

    // --- M4: traversal ---
    let x = db.create_node("Fn", Properties::new()).unwrap();
    let y = db.create_node("Fn", Properties::new()).unwrap();
    let z = db.create_node("Fn", Properties::new()).unwrap();
    let w = db.create_node("Fn", Properties::new()).unwrap();
    let e_xy = db.create_edge(x, "calls", y, Properties::new()).unwrap();
    let e_yz = db.create_edge(y, "calls", z, Properties::new()).unwrap();
    db.create_edge(z, "calls", w, Properties::new()).unwrap();

    // depth-limited outgoing traversal
    let result = db
        .traversal()
        .start(x)
        .outgoing("calls")
        .max_depth(2)
        .run()
        .unwrap();
    let nodes: Vec<NodeId> = result.iter().map(|n| n.node).collect();
    assert_eq!(nodes, vec![x, y, z]);
    assert_eq!(result[0].depth, 0);
    assert_eq!(result[1].depth, 1);
    assert_eq!(result[2].depth, 2);

    // incoming direction
    let rev = db
        .traversal()
        .start(y)
        .incoming("calls")
        .max_depth(5)
        .run()
        .unwrap();
    let rev_nodes: Vec<NodeId> = rev.iter().map(|n| n.node).collect();
    assert_eq!(rev_nodes, vec![y, x]);

    // edge-type filter: y also has a non-"calls" edge that must be excluded
    db.create_edge(y, "imports", w, Properties::new()).unwrap();
    let filtered = db
        .traversal()
        .start(y)
        .outgoing("calls")
        .max_depth(5)
        .run()
        .unwrap();
    let filtered_nodes: Vec<NodeId> = filtered.iter().map(|n| n.node).collect();
    assert_eq!(filtered_nodes, vec![y, z, w]);

    // cycle termination: p -> q -> r -> p must not hang and must visit each once
    let p = db.create_node("Fn", Properties::new()).unwrap();
    let q = db.create_node("Fn", Properties::new()).unwrap();
    let r = db.create_node("Fn", Properties::new()).unwrap();
    db.create_edge(p, "calls", q, Properties::new()).unwrap();
    db.create_edge(q, "calls", r, Properties::new()).unwrap();
    db.create_edge(r, "calls", p, Properties::new()).unwrap();
    let cyclic = db
        .traversal()
        .start(p)
        .direction(Direction::Out)
        .outgoing("calls")
        .max_depth(1000)
        .run()
        .unwrap();
    let mut cyclic_nodes: Vec<NodeId> = cyclic.iter().map(|n| n.node).collect();
    cyclic_nodes.sort();
    let mut expected = vec![p, q, r];
    expected.sort();
    assert_eq!(cyclic_nodes, expected);

    // to_tree reconstruction on the x->y->z->w chain
    let tree = crate::graph::to_tree(&result);
    assert_eq!(tree.len(), 1);
    assert_eq!(tree[0].node, x);
    assert_eq!(tree[0].children.len(), 1);
    assert_eq!(tree[0].children[0].node, y);
    assert_eq!(tree[0].children[0].via_edge, Some(e_xy));
    assert_eq!(tree[0].children[0].children[0].node, z);
    assert_eq!(tree[0].children[0].children[0].via_edge, Some(e_yz));
}

/// Advanced graph algorithms conformance suite (Direction::Both, shortest path, top_hubs, cascade_delete).
#[cfg(feature = "graph")]
pub fn graph_advanced_algorithms_suite<B: StorageBackend>(backend: B) {
    use crate::graph::{Direction, GraphDb, Properties};

    let db = GraphDb::new(backend);

    // 1. Test Direction::Both & filter_edge_types & filter_node_label
    let n1 = db.create_node("Function", Properties::new()).unwrap();
    let n2 = db.create_node("Function", Properties::new()).unwrap();
    let n3 = db.create_node("Struct", Properties::new()).unwrap();
    let n4 = db.create_node("Module", Properties::new()).unwrap();

    let _e1 = db.create_edge(n1, "calls", n2, Properties::new()).unwrap();
    let _e2 = db
        .create_edge(n3, "defines", n1, Properties::new())
        .unwrap();
    let _e3 = db
        .create_edge(n4, "imports", n3, Properties::new())
        .unwrap();

    // Traversal from n1 with Direction::Both should see both outgoing to n2 and incoming from n3
    let both = db
        .traversal()
        .start(n1)
        .direction(Direction::Both)
        .max_depth(1)
        .run()
        .unwrap();
    let both_nodes: Vec<_> = both.iter().map(|n| n.node).collect();
    assert!(both_nodes.contains(&n1));
    assert!(both_nodes.contains(&n2));
    assert!(both_nodes.contains(&n3));
    assert!(!both_nodes.contains(&n4));

    // Traversal with filter_node_label("Function")
    let only_fn = db
        .traversal()
        .start(n1)
        .direction(Direction::Both)
        .filter_node_label("Function")
        .max_depth(10)
        .run()
        .unwrap();
    let only_fn_nodes: Vec<_> = only_fn.iter().map(|n| n.node).collect();
    assert_eq!(only_fn_nodes, vec![n1, n2]); // n3 is "Struct", so excluded

    // 2. Test Shortest Path (BFS)
    // Graph: A -> B -> C -> D, and shortcut A -> C
    let a = db.create_node("N", Properties::new()).unwrap();
    let b = db.create_node("N", Properties::new()).unwrap();
    let c = db.create_node("N", Properties::new()).unwrap();
    let d = db.create_node("N", Properties::new()).unwrap();

    let _e_ab = db.create_edge(a, "step", b, Properties::new()).unwrap();
    let _e_bc = db.create_edge(b, "step", c, Properties::new()).unwrap();
    let _e_cd = db.create_edge(c, "step", d, Properties::new()).unwrap();
    let e_ac = db.create_edge(a, "shortcut", c, Properties::new()).unwrap();

    // Shortest path A -> D without shortcut should be A -> B -> C -> D (3 hops)
    let path_no_shortcut = db
        .find_shortest_path(a, d, Direction::Out, Some(&["step"]))
        .unwrap()
        .expect("path exists");
    assert_eq!(path_no_shortcut.nodes(), vec![a, b, c, d]);

    // Shortest path A -> D with any edge type should be A -> C -> D (2 hops)
    let path_shortcut = db
        .find_shortest_path(a, d, Direction::Out, None)
        .unwrap()
        .expect("path exists");
    assert_eq!(path_shortcut.nodes(), vec![a, c, d]);
    assert_eq!(path_shortcut.edges()[0], e_ac);

    // Unreachable target returns None
    let unreach = db.create_node("Isolated", Properties::new()).unwrap();
    assert!(
        db.find_shortest_path(a, unreach, Direction::Out, None)
            .unwrap()
            .is_none()
    );

    // 3. Test Top Hubs
    // Star topology: hub connected to 5 satellite nodes
    let hub = db.create_node("Hub", Properties::new()).unwrap();
    for _ in 0..5 {
        let sat = db.create_node("Sat", Properties::new()).unwrap();
        db.create_edge(hub, "links", sat, Properties::new())
            .unwrap();
    }
    let top = db.top_hubs(1, Direction::Out, None).unwrap();
    assert_eq!(top.len(), 1);
    assert_eq!(top[0].0, hub);
    assert_eq!(top[0].1, 5);

    // 4. Test Hierarchical Cascade Delete
    // File -> Function -> Scope
    let file = db.create_node("File", Properties::new()).unwrap();
    let func = db.create_node("Function", Properties::new()).unwrap();
    let scope = db.create_node("Scope", Properties::new()).unwrap();
    let other_file = db.create_node("File", Properties::new()).unwrap();

    let e_ff = db
        .create_edge(file, "contains", func, Properties::new())
        .unwrap();
    let e_fs = db
        .create_edge(func, "contains", scope, Properties::new())
        .unwrap();
    let e_call = db
        .create_edge(func, "calls", other_file, Properties::new())
        .unwrap();

    let deleted = db.cascade_delete(file, "contains").unwrap();
    assert_eq!(deleted.len(), 3);
    assert!(deleted.contains(&file));
    assert!(deleted.contains(&func));
    assert!(deleted.contains(&scope));

    // Verify all 3 nodes and all touching edges are wiped
    assert!(db.get_node(file).unwrap().is_none());
    assert!(db.get_node(func).unwrap().is_none());
    assert!(db.get_node(scope).unwrap().is_none());
    assert!(db.get_edge(e_ff).unwrap().is_none());
    assert!(db.get_edge(e_fs).unwrap().is_none());
    assert!(db.get_edge(e_call).unwrap().is_none());
    // other_file must survive!
    assert!(db.get_node(other_file).unwrap().is_some());
}

/// Shared conformance suite for [`crate::graph::GraphDb::write_tx`] — the
/// graph-only counterpart of [`relational_write_tx_conformance_suite`],
/// proving one batch of several node/edge mutations commits or rolls back
/// as a whole.
#[cfg(feature = "graph")]
pub fn graph_write_tx_conformance_suite<B: StorageBackend>(backend: B) {
    use crate::BknError;
    use crate::graph::{GraphDb, Properties};

    let db = GraphDb::new(backend);
    let existing = db.create_node("Fn", Properties::new()).unwrap();

    // A batch that creates a node, an edge from it to a pre-existing node,
    // and then fails: neither the new node nor the new edge must exist.
    let new_node_id = std::sync::Mutex::new(None);
    let err = db.write_tx(|batch| -> Result<(), BknError> {
        let a = batch.graph().create_node("Fn", Properties::new())?;
        *new_node_id.lock().unwrap() = Some(a);
        batch
            .graph()
            .create_edge(a, "calls", existing, Properties::new())?;
        Err(BknError::NotFound)
    });
    assert!(err.is_err());
    let a = new_node_id.into_inner().unwrap().unwrap();
    assert!(
        db.get_node(a).unwrap().is_none(),
        "a failed batch must leave the new node absent"
    );
    assert!(db.neighbors_out(a, "calls").unwrap().is_empty());

    // A matching batch that succeeds: node + edge commit together.
    let (a, edge) = db
        .write_tx(|batch| {
            let a = batch.graph().create_node("Fn", Properties::new())?;
            let e = batch
                .graph()
                .create_edge(a, "calls", existing, Properties::new())?;
            Ok::<_, BknError>((a, e))
        })
        .unwrap();
    assert!(db.get_node(a).unwrap().is_some());
    assert_eq!(
        db.neighbors_out(a, "calls").unwrap(),
        vec![(existing, edge)]
    );
}
