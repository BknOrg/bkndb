//! Shared conformance test suite, exercised identically against every
//! `StorageBackend` implementation so all backends satisfy the same contract.
#![cfg(feature = "test-util")]

use std::ops::Bound;

use crate::{StorageBackend, StorageReadTx, StorageWriteTx, TableSpec};

pub fn conformance_suite<B: StorageBackend>(backend: &B) {
    const T: TableSpec = TableSpec("t");

    // 1. read-after-write-commit round trip
    {
        let mut w = backend.begin_write().unwrap();
        w.put(T, b"k1", b"v1").unwrap();
        w.commit().unwrap();
    }
    let r = backend.begin_read().unwrap();
    assert_eq!(r.get(T, b"k1").unwrap(), Some(b"v1".to_vec()));

    // 2. missing key returns None, not an error, even on a fresh table
    assert_eq!(r.get(T, b"nope").unwrap(), None);

    // 3. delete removes the key
    {
        let mut w = backend.begin_write().unwrap();
        w.delete(T, b"k1").unwrap();
        w.commit().unwrap();
    }
    let r2 = backend.begin_read().unwrap();
    assert_eq!(r2.get(T, b"k1").unwrap(), None);

    // 4. range scan returns keys in sorted order within bounds
    {
        let mut w = backend.begin_write().unwrap();
        for k in [1u8, 2, 3, 4, 5] {
            w.put(T, &[k], &[k * 10]).unwrap();
        }
        w.commit().unwrap();
    }
    let r3 = backend.begin_read().unwrap();
    let got = r3
        .range(T, Bound::Included(&[2u8][..]), Bound::Included(&[4u8][..]))
        .unwrap();
    assert_eq!(
        got,
        vec![
            (vec![2], vec![20]),
            (vec![3], vec![30]),
            (vec![4], vec![40])
        ]
    );

    // 5. streaming scan yields exactly what range returns, can stop early,
    //    and bounds that can't contain anything are empty (never a panic)
    let all = r3.range(T, Bound::Unbounded, Bound::Unbounded).unwrap();
    let streamed: Vec<_> = r3.scan(T, Bound::Unbounded, Bound::Unbounded).unwrap().map(Result::unwrap).collect();
    assert_eq!(streamed, all);
    let first_two: Vec<_> = r3.scan(T, Bound::Excluded(&[1u8][..]), Bound::Unbounded).unwrap().take(2).map(Result::unwrap).collect();
    assert_eq!(first_two, vec![(vec![2], vec![20]), (vec![3], vec![30])]);
    for (lo, hi) in [
        (Bound::Included(&[4u8][..]), Bound::Included(&[2u8][..])),
        (Bound::Excluded(&[3u8][..]), Bound::Excluded(&[3u8][..])),
        (Bound::Included(&[3u8][..]), Bound::Excluded(&[3u8][..])),
    ] {
        assert!(r3.range(T, lo, hi).unwrap().is_empty());
        assert_eq!(r3.scan(T, lo, hi).unwrap().count(), 0);
    }
    assert!(r3.scan(TableSpec("never_written"), Bound::Unbounded, Bound::Unbounded).unwrap().next().is_none());

    // 6. a write transaction's scan sees its own uncommitted puts/deletes
    {
        let mut w = backend.begin_write().unwrap();
        w.delete(T, &[2u8]).unwrap();
        w.put(T, &[6u8], &[60]).unwrap();
        let keys: Vec<u8> = w.scan(T, Bound::Unbounded, Bound::Unbounded).unwrap().map(|kv| kv.unwrap().0[0]).collect();
        assert_eq!(keys, vec![1, 3, 4, 5, 6]);
        let (lo, hi) = (Bound::Included(&[5u8][..]), Bound::Included(&[1u8][..]));
        assert!(w.range(T, lo, hi).unwrap().is_empty());
        assert_eq!(w.scan(T, lo, hi).unwrap().count(), 0);
        // dropped without commit: rolled back
    }

    // 7. a read transaction keeps seeing its snapshot while a writer commits
    let before = backend.begin_read().unwrap();
    {
        let mut w = backend.begin_write().unwrap();
        w.put(T, &[9u8], &[90]).unwrap();
        w.delete(T, &[1u8]).unwrap();
        w.commit().unwrap();
    }
    assert_eq!(before.get(T, &[1u8]).unwrap(), Some(vec![10]));
    assert_eq!(before.get(T, &[9u8]).unwrap(), None);
    assert_eq!(before.scan(T, Bound::Unbounded, Bound::Unbounded).unwrap().count(), 5);
    let after = backend.begin_read().unwrap();
    assert_eq!(after.scan(T, Bound::Unbounded, Bound::Unbounded).unwrap().count(), 5);
    assert_eq!(after.get(T, &[9u8]).unwrap(), Some(vec![90]));
}

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

/// Shared conformance suite for the relational layer (schema/row CRUD,
/// secondary indexes, predicate-based update/delete), exercised identically
/// against every `StorageBackend` implementation.
#[cfg(feature = "relational")]
pub fn relational_conformance_suite<B: StorageBackend>(backend: B) {
    use std::ops::Bound;

    use crate::relational::{ColumnDef, ColumnKind, RelSchema, RelationalDb};
    use crate::value::{PropValue, Properties};

    static WIDGETS: RelSchema = RelSchema {
        name: "widgets",
        columns: &[
            ColumnDef {
                name: "id",
                kind: ColumnKind::Int,
            },
            ColumnDef {
                name: "name",
                kind: ColumnKind::Str,
            },
            ColumnDef {
                name: "color",
                kind: ColumnKind::Str,
            },
            ColumnDef {
                name: "price",
                kind: ColumnKind::Int,
            },
        ],
        primary_key: "id",
        auto_increment_pk: true,
        indexed_columns: &["color"],
    };

    fn props(name: &str, color: &str, price: i64) -> Properties {
        let mut p = Properties::new();
        p.insert("name".to_string(), PropValue::Str(name.to_string()));
        p.insert("color".to_string(), PropValue::Str(color.to_string()));
        p.insert("price".to_string(), PropValue::Int(price));
        p
    }

    let db = RelationalDb::new(backend);
    let table = db.table(&WIDGETS);

    let id1 = table.insert(props("bolt", "red", 10)).unwrap();
    let id2 = table.insert(props("nut", "red", 5)).unwrap();
    let id3 = table.insert(props("washer", "blue", 2)).unwrap();

    // --- insert + get roundtrip ---
    let row1 = table.get(&id1).unwrap().unwrap();
    assert_eq!(
        row1.values.get("name"),
        Some(&PropValue::Str("bolt".to_string()))
    );

    // --- where_eq on an indexed column, duplicate values ---
    let red = table
        .select()
        .where_eq("color", PropValue::Str("red".to_string()))
        .run()
        .unwrap();
    assert_eq!(red.len(), 2);
    assert!(red.iter().any(|r| r.pk == id1));
    assert!(red.iter().any(|r| r.pk == id2));

    // --- where_eq on an unindexed column (full-scan path) ---
    let cheap = table
        .select()
        .where_eq("price", PropValue::Int(2))
        .run()
        .unwrap();
    assert_eq!(cheap.len(), 1);
    assert_eq!(cheap[0].pk, id3);

    // --- update that changes an indexed column's value ---
    let changed = table
        .update()
        .where_eq("id", id2.clone())
        .set("color", PropValue::Str("green".to_string()))
        .run()
        .unwrap();
    assert_eq!(changed, 1);
    let red_after = table
        .select()
        .where_eq("color", PropValue::Str("red".to_string()))
        .run()
        .unwrap();
    assert_eq!(red_after.len(), 1);
    assert_eq!(red_after[0].pk, id1);
    let green = table
        .select()
        .where_eq("color", PropValue::Str("green".to_string()))
        .run()
        .unwrap();
    assert_eq!(green.len(), 1);
    assert_eq!(green[0].pk, id2);

    // --- where_range on an indexed column ---
    // "blue" <= color < "green": matches only id3 ("blue"); id1 ("red") and
    // id2 ("green") both fall outside the range.
    let ranged = table
        .select()
        .where_range(
            "color",
            Bound::Included(PropValue::Str("blue".to_string())),
            Bound::Excluded(PropValue::Str("green".to_string())),
        )
        .run()
        .unwrap();
    assert_eq!(ranged.len(), 1);
    assert_eq!(ranged[0].pk, id3);

    // --- delete removes row + index entries ---
    let deleted = table.delete().where_eq("id", id1.clone()).run().unwrap();
    assert_eq!(deleted, 1);
    assert!(table.get(&id1).unwrap().is_none());
    let red_final = table
        .select()
        .where_eq("color", PropValue::Str("red".to_string()))
        .run()
        .unwrap();
    assert!(red_final.is_empty());

    // --- predicate matching nothing returns Ok(empty)/Ok(0), not an error ---
    let none = table
        .select()
        .where_eq("color", PropValue::Str("purple".to_string()))
        .run()
        .unwrap();
    assert!(none.is_empty());
    let none_updated = table
        .update()
        .where_eq("color", PropValue::Str("purple".to_string()))
        .set("price", PropValue::Int(1))
        .run()
        .unwrap();
    assert_eq!(none_updated, 0);
    let none_deleted = table
        .delete()
        .where_eq("color", PropValue::Str("purple".to_string()))
        .run()
        .unwrap();
    assert_eq!(none_deleted, 0);
}

/// Proves a `RelSchema` named one of `bkndb-core`'s reserved internal table
/// names (see [`crate::RESERVED_TABLE_NAMES`]) is rejected loudly at first
/// use, not silently allowed to corrupt data — the exact bug class hit once
/// via an app-level `RelSchema` accidentally named `"meta"`.
#[cfg(feature = "relational")]
pub fn relational_reserved_name_conformance_suite<B: StorageBackend>(backend: B) {
    use crate::relational::{ColumnDef, ColumnKind, RelSchema, RelationalDb};

    static RESERVED: RelSchema = RelSchema {
        name: "meta",
        columns: &[ColumnDef {
            name: "id",
            kind: ColumnKind::Int,
        }],
        primary_key: "id",
        auto_increment_pk: true,
        indexed_columns: &[],
    };

    let db = RelationalDb::new(backend);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        db.table(&RESERVED);
    }));
    assert!(
        result.is_err(),
        "a RelSchema named 'meta' must panic at .table(), not silently corrupt the internal counter table"
    );
}

/// Shared conformance suite for [`crate::kv::Kv`] — basic get/put/delete
/// round trip through the validated raw-KV facade.
pub fn kv_conformance_suite<B: StorageBackend>(backend: B) {
    use crate::kv::Kv;

    let kv = Kv::new(backend);
    let t = TableSpec("scratch");

    assert_eq!(kv.get(t, b"k1").unwrap(), None);
    kv.put(t, b"k1", b"v1").unwrap();
    assert_eq!(kv.get(t, b"k1").unwrap(), Some(b"v1".to_vec()));
    kv.delete(t, b"k1").unwrap();
    assert_eq!(kv.get(t, b"k1").unwrap(), None);

    kv.put(t, b"a", b"1").unwrap();
    kv.put(t, b"b", b"2").unwrap();
    let all = kv.range(t, Bound::Unbounded, Bound::Unbounded).unwrap();
    assert_eq!(
        all,
        vec![
            (b"a".to_vec(), b"1".to_vec()),
            (b"b".to_vec(), b"2".to_vec())
        ]
    );
}

/// Proves every [`crate::kv::Kv`] method rejects every one of
/// `bkndb-core`'s reserved table names, while an ordinary name still works
/// end-to-end.
pub fn kv_reserved_name_conformance_suite<B: StorageBackend>(backend: B) {
    use crate::BknError;
    use crate::kv::Kv;

    let kv = Kv::new(backend);
    for &name in crate::RESERVED_TABLE_NAMES {
        let t = TableSpec(name);
        assert!(matches!(
            kv.get(t, b"k").unwrap_err(),
            BknError::ReservedTableName(_)
        ));
        assert!(matches!(
            kv.put(t, b"k", b"v").unwrap_err(),
            BknError::ReservedTableName(_)
        ));
        assert!(matches!(
            kv.delete(t, b"k").unwrap_err(),
            BknError::ReservedTableName(_)
        ));
        assert!(matches!(
            kv.range(t, Bound::Unbounded, Bound::Unbounded).unwrap_err(),
            BknError::ReservedTableName(_)
        ));
    }

    let ok = TableSpec("scratch");
    kv.put(ok, b"k", b"v").unwrap();
    assert_eq!(kv.get(ok, b"k").unwrap(), Some(b"v".to_vec()));
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

/// Proves [`crate::db::Db`] gives working `.graph()`/`.relational()`/`.kv()`
/// access over one shared backend — the ergonomic replacement for manually
/// calling `GraphDb::from_arc`/`RelationalDb::from_arc` on the same cloned
/// `Arc<B>`.
#[cfg(all(feature = "graph", feature = "relational"))]
pub fn db_conformance_suite<B: StorageBackend>(backend: B) {
    use crate::db::Db;
    use crate::relational::{ColumnDef, ColumnKind, RelSchema};
    use crate::value::{PropValue, Properties as RelProperties};

    static ROWS: RelSchema = RelSchema {
        name: "db_smoke",
        columns: &[ColumnDef {
            name: "id",
            kind: ColumnKind::Int,
        }],
        primary_key: "id",
        auto_increment_pk: false,
        indexed_columns: &[],
    };

    let db = Db::new(backend);
    let node = db
        .graph()
        .create_node("File", crate::graph::Properties::new())
        .unwrap();

    db.relational()
        .table(&ROWS)
        .insert_with_pk(PropValue::Int(node.0 as i64), RelProperties::new())
        .unwrap();

    db.kv().put(TableSpec("scratch"), b"k", b"v").unwrap();

    assert!(db.graph().get_node(node).unwrap().is_some());
    assert!(
        db.relational()
            .table(&ROWS)
            .get(&PropValue::Int(node.0 as i64))
            .unwrap()
            .is_some()
    );
    assert_eq!(
        db.kv().get(TableSpec("scratch"), b"k").unwrap(),
        Some(b"v".to_vec())
    );
}

/// Shared conformance suite for [`crate::db::Db::write_tx`] — the three-way
/// atomicity proof: a batch touching graph + relational + raw KV that fails
/// must leave all three completely untouched; a matching batch that
/// succeeds must commit all three together.
#[cfg(all(feature = "graph", feature = "relational"))]
pub fn db_write_tx_conformance_suite<B: StorageBackend>(backend: B) {
    use crate::BknError;
    use crate::db::Db;
    use crate::relational::{ColumnDef, ColumnKind, RelSchema};
    use crate::value::{PropValue, Properties as RelProperties};

    static ROWS: RelSchema = RelSchema {
        name: "db_write_tx_smoke",
        columns: &[ColumnDef {
            name: "id",
            kind: ColumnKind::Int,
        }],
        primary_key: "id",
        auto_increment_pk: false,
        indexed_columns: &[],
    };

    let db = Db::new(backend);
    let scratch = TableSpec("scratch");

    let new_node_id = std::sync::Mutex::new(None);
    let err = db.write_tx(|batch| -> Result<(), BknError> {
        let node = batch
            .graph()
            .create_node("File", crate::graph::Properties::new())?;
        *new_node_id.lock().unwrap() = Some(node);
        batch
            .relational()
            .table(&ROWS)
            .insert_with_pk(PropValue::Int(node.0 as i64), RelProperties::new())?;
        batch.kv().put(scratch, b"k", b"v")?;
        Err(BknError::NotFound)
    });
    assert!(err.is_err());
    let node = new_node_id.into_inner().unwrap().unwrap();
    assert!(
        db.graph().get_node(node).unwrap().is_none(),
        "failed batch must leave the graph node absent"
    );
    assert!(
        db.relational()
            .table(&ROWS)
            .get(&PropValue::Int(node.0 as i64))
            .unwrap()
            .is_none(),
        "failed batch must leave the relational row absent"
    );
    assert_eq!(
        db.kv().get(scratch, b"k").unwrap(),
        None,
        "failed batch must leave the kv key absent"
    );

    let node = db
        .write_tx(|batch| {
            let node = batch
                .graph()
                .create_node("File", crate::graph::Properties::new())?;
            batch
                .relational()
                .table(&ROWS)
                .insert_with_pk(PropValue::Int(node.0 as i64), RelProperties::new())?;
            batch.kv().put(scratch, b"k", b"v")?;
            Ok::<_, BknError>(node)
        })
        .unwrap();
    assert!(db.graph().get_node(node).unwrap().is_some());
    assert!(
        db.relational()
            .table(&ROWS)
            .get(&PropValue::Int(node.0 as i64))
            .unwrap()
            .is_some()
    );
    assert_eq!(db.kv().get(scratch, b"k").unwrap(), Some(b"v".to_vec()));
}

/// Shared conformance suite for [`crate::db::Db::read_tx`]
/// — proves consistent reading across graph, relational, and raw KV
/// over a single read transaction snapshot.
#[cfg(all(feature = "graph", feature = "relational"))]
pub fn db_read_tx_conformance_suite<B: StorageBackend>(backend: B) {
    use crate::BknError;
    use crate::db::Db;
    use crate::relational::{ColumnDef, ColumnKind, RelSchema};
    use crate::value::{PropValue, Properties as RelProperties};

    static ROWS: RelSchema = RelSchema {
        name: "db_read_tx_smoke",
        columns: &[ColumnDef {
            name: "id",
            kind: ColumnKind::Int,
        }],
        primary_key: "id",
        auto_increment_pk: false,
        indexed_columns: &[],
    };

    let db = Db::new(backend);
    let scratch = TableSpec("scratch_rtx");

    let (node_a, node_b, edge_id) = db
        .write_tx(|batch| {
            let a = batch
                .graph()
                .create_node("File", crate::graph::Properties::new())?;
            let b = batch
                .graph()
                .create_node("File", crate::graph::Properties::new())?;
            let e = batch
                .graph()
                .create_edge(a, "imports", b, crate::graph::Properties::new())?;
            batch
                .relational()
                .table(&ROWS)
                .insert_with_pk(PropValue::Int(a.0 as i64), RelProperties::new())?;
            batch.kv().put(scratch, b"test_key", b"test_val")?;
            Ok::<_, BknError>((a, b, e))
        })
        .unwrap();

    db.read_tx(|tx| {
        let node_a_rec = tx.graph().get_node(node_a)?.expect("node a exists");
        assert_eq!(node_a_rec.label, "File");
        let edge_rec = tx.graph().get_edge(edge_id)?.expect("edge exists");
        assert_eq!(edge_rec.edge_type, "imports");
        let neighbors = tx.graph().neighbors_out(node_a, "imports")?;
        assert_eq!(neighbors, vec![(node_b, edge_id)]);
        assert_eq!(tx.graph().out_degree(node_a)?, 1);
        assert_eq!(tx.graph().in_degree(node_b)?, 1);

        let row = tx
            .relational()
            .table(&ROWS)
            .get(&PropValue::Int(node_a.0 as i64))?
            .expect("row exists");
        assert_eq!(row.pk, PropValue::Int(node_a.0 as i64));

        let val = tx.kv().get(scratch, b"test_key")?.expect("val exists");
        assert_eq!(val, b"test_val");

        Ok::<_, BknError>(())
    })
    .unwrap();
}

/// Shared conformance suite for read-your-own-writes inside [`crate::db::Db::write_tx`].
#[cfg(all(feature = "graph", feature = "relational"))]
pub fn db_batch_read_your_own_writes_suite<B: StorageBackend>(backend: B) {
    use crate::BknError;
    use crate::db::Db;
    use crate::relational::{ColumnDef, ColumnKind, RelSchema};
    use crate::value::{PropValue, Properties as RelProperties};

    static ROWS: RelSchema = RelSchema {
        name: "db_ryow_smoke",
        columns: &[ColumnDef {
            name: "id",
            kind: ColumnKind::Int,
        }],
        primary_key: "id",
        auto_increment_pk: false,
        indexed_columns: &[],
    };

    let db = Db::new(backend);

    db.write_tx(|batch| {
        let a = batch
            .graph()
            .create_node("File", crate::graph::Properties::new())?;
        let b = batch
            .graph()
            .create_node("File", crate::graph::Properties::new())?;
        let e = batch
            .graph()
            .create_edge(a, "calls", b, crate::graph::Properties::new())?;

        assert!(batch.graph().get_node(a)?.is_some());
        assert_eq!(batch.graph().neighbors_out(a, "calls")?, vec![(b, e)]);
        assert_eq!(batch.graph().out_degree(a)?, 1);
        assert_eq!(batch.graph().in_degree(b)?, 1);

        batch
            .relational()
            .table(&ROWS)
            .insert_with_pk(PropValue::Int(a.0 as i64), RelProperties::new())?;
        assert!(
            batch
                .relational()
                .table(&ROWS)
                .get(&PropValue::Int(a.0 as i64))?
                .is_some()
        );

        Ok::<_, BknError>(())
    })
    .unwrap();
}

/// Shared conformance suite for [`crate::relational::RelationalDb::write_tx`]
/// — proves a batch spanning several tables is genuinely atomic: an `Err`
/// return must leave every table it touched completely unchanged (including
/// updates to pre-existing rows, not just new inserts), and an `Ok` return
/// must commit every table's changes together.
#[cfg(feature = "relational")]
pub fn relational_write_tx_conformance_suite<B: StorageBackend>(backend: B) {
    use crate::BknError;
    use crate::relational::{ColumnDef, ColumnKind, RelSchema, RelationalDb};
    use crate::value::{PropValue, Properties};

    static A: RelSchema = RelSchema {
        name: "wtx_a",
        columns: &[
            ColumnDef {
                name: "id",
                kind: ColumnKind::Int,
            },
            ColumnDef {
                name: "val",
                kind: ColumnKind::Str,
            },
        ],
        primary_key: "id",
        auto_increment_pk: true,
        indexed_columns: &[],
    };
    static B_SCHEMA: RelSchema = RelSchema {
        name: "wtx_b",
        columns: &[
            ColumnDef {
                name: "id",
                kind: ColumnKind::Int,
            },
            ColumnDef {
                name: "val",
                kind: ColumnKind::Str,
            },
        ],
        primary_key: "id",
        auto_increment_pk: true,
        indexed_columns: &[],
    };
    static C: RelSchema = RelSchema {
        name: "wtx_c",
        columns: &[
            ColumnDef {
                name: "id",
                kind: ColumnKind::Int,
            },
            ColumnDef {
                name: "val",
                kind: ColumnKind::Str,
            },
        ],
        primary_key: "id",
        auto_increment_pk: true,
        indexed_columns: &[],
    };

    fn row(val: &str) -> Properties {
        let mut p = Properties::new();
        p.insert("val".to_string(), PropValue::Str(val.to_string()));
        p
    }

    let db = RelationalDb::new(backend);

    // Pre-existing row in table A that a failing batch will attempt to update.
    let a_table = db.table(&A);
    let pre_id = a_table.insert(row("pre-existing")).unwrap();

    // A batch that writes to all three tables (including updating the
    // pre-existing A row) and then fails: nothing must stick.
    let err = db.write_tx(|batch| -> Result<(), BknError> {
        batch.table(&A).update(&pre_id, |v| {
            v.insert("val".to_string(), PropValue::Str("mutated".to_string()));
        })?;
        batch.table(&B_SCHEMA).insert(row("b-row"))?;
        batch.table(&C).insert(row("c-row"))?;
        Err(BknError::NotFound)
    });
    assert!(err.is_err());

    assert_eq!(
        a_table.get(&pre_id).unwrap().unwrap().values.get("val"),
        Some(&PropValue::Str("pre-existing".to_string())),
        "a failed batch must leave a pre-existing row's update unapplied"
    );
    assert!(
        db.table(&B_SCHEMA).select().run().unwrap().is_empty(),
        "a failed batch must leave table B empty"
    );
    assert!(
        db.table(&C).select().run().unwrap().is_empty(),
        "a failed batch must leave table C empty"
    );

    // A matching batch that succeeds: every table's change commits together.
    let (b_id, c_id) = db
        .write_tx(|batch| {
            batch.table(&A).update(&pre_id, |v| {
                v.insert("val".to_string(), PropValue::Str("mutated".to_string()));
            })?;
            let b_id = batch.table(&B_SCHEMA).insert(row("b-row"))?;
            let c_id = batch.table(&C).insert(row("c-row"))?;
            Ok::<_, BknError>((b_id, c_id))
        })
        .unwrap();

    assert_eq!(
        a_table.get(&pre_id).unwrap().unwrap().values.get("val"),
        Some(&PropValue::Str("mutated".to_string()))
    );
    assert_eq!(
        db.table(&B_SCHEMA)
            .get(&b_id)
            .unwrap()
            .unwrap()
            .values
            .get("val"),
        Some(&PropValue::Str("b-row".to_string()))
    );
    assert_eq!(
        db.table(&C).get(&c_id).unwrap().unwrap().values.get("val"),
        Some(&PropValue::Str("c-row".to_string()))
    );

    // delete_where_eq / update_where_eq / select_eq inside a batch.
    let d_table_pre = db.table(&A).insert(row("dup")).unwrap();
    let d_table_pre2 = db.table(&A).insert(row("dup")).unwrap();
    db.write_tx(|batch| {
        let mut t = batch.table(&A);
        let matches = t.select_eq("val", &PropValue::Str("dup".to_string()))?;
        assert_eq!(matches.len(), 2);
        let updated = t.update_where_eq(
            "val",
            &PropValue::Str("dup".to_string()),
            &[("val", PropValue::Str("deduped".to_string()))],
        )?;
        assert_eq!(updated, 2);
        Ok::<_, BknError>(())
    })
    .unwrap();
    assert_eq!(
        db.table(&A)
            .get(&d_table_pre)
            .unwrap()
            .unwrap()
            .values
            .get("val"),
        Some(&PropValue::Str("deduped".to_string()))
    );
    assert_eq!(
        db.table(&A)
            .get(&d_table_pre2)
            .unwrap()
            .unwrap()
            .values
            .get("val"),
        Some(&PropValue::Str("deduped".to_string()))
    );

    db.write_tx(|batch| {
        let deleted = batch
            .table(&A)
            .delete_where_eq("val", &PropValue::Str("deduped".to_string()))?;
        assert_eq!(deleted, 2);
        Ok::<_, BknError>(())
    })
    .unwrap();
    assert!(db.table(&A).get(&d_table_pre).unwrap().is_none());
    assert!(db.table(&A).get(&d_table_pre2).unwrap().is_none());
}

#[cfg(all(feature = "graph", feature = "relational"))]
static HYBRID_FILES: crate::relational::RelSchema = crate::relational::RelSchema {
    name: "hybrid_files",
    columns: &[
        crate::relational::ColumnDef {
            name: "file_id",
            kind: crate::relational::ColumnKind::Int,
        },
        crate::relational::ColumnDef {
            name: "path",
            kind: crate::relational::ColumnKind::Str,
        },
        crate::relational::ColumnDef {
            name: "size",
            kind: crate::relational::ColumnKind::Int,
        },
    ],
    primary_key: "file_id",
    auto_increment_pk: false,
    indexed_columns: &["path", "size"],
};

#[cfg(all(feature = "graph", feature = "relational"))]
static HYBRID_SYMBOLS: crate::relational::RelSchema = crate::relational::RelSchema {
    name: "hybrid_symbols",
    columns: &[
        crate::relational::ColumnDef {
            name: "id",
            kind: crate::relational::ColumnKind::Int,
        },
        crate::relational::ColumnDef {
            name: "file_node_id",
            kind: crate::relational::ColumnKind::Int,
        },
        crate::relational::ColumnDef {
            name: "name",
            kind: crate::relational::ColumnKind::Str,
        },
    ],
    primary_key: "id",
    auto_increment_pk: true,
    indexed_columns: &["file_node_id", "name"],
};

/// Conformance suite for secondary index prefix searching, range scanning,
/// index cleanup on update/delete, and hybrid graph-relational joins.
#[cfg(all(feature = "graph", feature = "relational"))]
pub fn relational_indexing_and_hybrid_suite<B: StorageBackend>(backend: B) {
    use crate::BknError;
    use crate::db::Db;
    use crate::graph::{Direction, NodeId, Properties as GraphProperties};
    use crate::value::{PropValue, Properties};

    let db = Db::new(backend);

    // 1. Setup graph nodes & relational rows atomically in write_tx
    let (f1, f2, f3) = db
        .write_tx(|tx| {
            let n1 = tx.graph().create_node("File", GraphProperties::new())?;
            let n2 = tx.graph().create_node("File", GraphProperties::new())?;
            let n3 = tx.graph().create_node("File", GraphProperties::new())?;

            // Edges: n1 -> n2 -> n3 (calls)
            tx.graph()
                .create_edge(n1, "calls", n2, GraphProperties::new())?;
            tx.graph()
                .create_edge(n2, "calls", n3, GraphProperties::new())?;

            // Direct PK relational table (FILES)
            let mut r1 = Properties::new();
            r1.insert(
                "path".to_string(),
                PropValue::Str("src/main.rs".to_string()),
            );
            r1.insert("size".to_string(), PropValue::Int(150));
            tx.relational()
                .table(&HYBRID_FILES)
                .insert_with_pk(PropValue::Int(n1.0 as i64), r1)?;

            let mut r2 = Properties::new();
            r2.insert("path".to_string(), PropValue::Str("src/lib.rs".to_string()));
            r2.insert("size".to_string(), PropValue::Int(300));
            tx.relational()
                .table(&HYBRID_FILES)
                .insert_with_pk(PropValue::Int(n2.0 as i64), r2)?;

            let mut r3 = Properties::new();
            r3.insert(
                "path".to_string(),
                PropValue::Str("tests/integration.rs".to_string()),
            );
            r3.insert("size".to_string(), PropValue::Int(500));
            tx.relational()
                .table(&HYBRID_FILES)
                .insert_with_pk(PropValue::Int(n3.0 as i64), r3)?;

            // Foreign Key relational table (SYMBOLS)
            let mut s1 = Properties::new();
            s1.insert("file_node_id".to_string(), PropValue::Int(n1.0 as i64));
            s1.insert("name".to_string(), PropValue::Str("parse_ast".to_string()));
            tx.relational().table(&HYBRID_SYMBOLS).insert(s1)?;

            let mut s2 = Properties::new();
            s2.insert("file_node_id".to_string(), PropValue::Int(n1.0 as i64));
            s2.insert("name".to_string(), PropValue::Str("parse_expr".to_string()));
            tx.relational().table(&HYBRID_SYMBOLS).insert(s2)?;

            let mut s3 = Properties::new();
            s3.insert("file_node_id".to_string(), PropValue::Int(n2.0 as i64));
            s3.insert("name".to_string(), PropValue::Str("eval".to_string()));
            tx.relational().table(&HYBRID_SYMBOLS).insert(s3)?;

            Ok::<_, BknError>((n1, n2, n3))
        })
        .unwrap();

    // 2. Secondary Index Prefix Search
    db.read_tx(|tx| {
        let table = tx.relational().table(&HYBRID_FILES);

        // Prefix "src/" matches src/main.rs and src/lib.rs
        let src_files = table.select_prefix("path", "src/")?;
        assert_eq!(src_files.len(), 2);

        // Prefix "tests/" matches tests/integration.rs
        let test_files = table.select_prefix("path", "tests/")?;
        assert_eq!(test_files.len(), 1);
        assert_eq!(
            test_files[0].values.get("path"),
            Some(&PropValue::Str("tests/integration.rs".to_string()))
        );

        // Prefix "nonexistent" matches 0
        let non_files = table.select_prefix("path", "nonexistent")?;
        assert_eq!(non_files.len(), 0);

        // Empty prefix matches all
        let all_files = table.select_prefix("path", "")?;
        assert_eq!(all_files.len(), 3);

        // Prefix on symbols: "parse_" matches 2 symbols
        let sym_table = tx.relational().table(&HYBRID_SYMBOLS);
        let parse_syms = sym_table.select_prefix("name", "parse_")?;
        assert_eq!(parse_syms.len(), 2);

        // SelectQuery where_prefix with limit
        let query_files = db
            .relational()
            .table(&HYBRID_FILES)
            .select()
            .where_prefix("path", "src/")
            .limit(1)
            .run()?;
        assert_eq!(query_files.len(), 1);

        Ok::<_, BknError>(())
    })
    .unwrap();

    // 3. Secondary Index Range Scan
    db.read_tx(|tx| {
        let table = tx.relational().table(&HYBRID_FILES);
        let ranged = table.select_range(
            "size",
            &Bound::Included(PropValue::Int(200)),
            &Bound::Included(PropValue::Int(600)),
        )?;
        assert_eq!(ranged.len(), 2); // 300 and 500
        Ok::<_, BknError>(())
    })
    .unwrap();

    // 4. Index Maintenance: Update and Delete cleans index
    db.write_tx(|tx| {
        let mut rel = tx.relational();
        let mut table = rel.table(&HYBRID_FILES);
        // Change path of f1
        table.update(&PropValue::Int(f1.0 as i64), |props| {
            props.insert(
                "path".to_string(),
                PropValue::Str("src/renamed_main.rs".to_string()),
            );
        })?;
        Ok::<_, BknError>(())
    })
    .unwrap();

    db.read_tx(|tx| {
        let table = tx.relational().table(&HYBRID_FILES);
        // Old prefix "src/main" should now return 0
        let old = table.select_prefix("path", "src/main")?;
        assert_eq!(old.len(), 0);

        // New prefix "src/renamed" should return 1
        let new = table.select_prefix("path", "src/renamed")?;
        assert_eq!(new.len(), 1);
        Ok::<_, BknError>(())
    })
    .unwrap();

    // 5. Hybrid Direct PK Join (join_nodes_with_table)
    db.read_tx(|tx| {
        let joined = tx.join_nodes_with_table(&[f1, f2, NodeId(8888)], &HYBRID_FILES)?;
        assert_eq!(joined.len(), 3);

        // f1
        assert_eq!(joined[0].id, f1);
        assert!(joined[0].node.is_some());
        assert_eq!(joined[0].node.as_ref().unwrap().label, "File");
        assert_eq!(
            joined[0].row.as_ref().unwrap().values.get("path"),
            Some(&PropValue::Str("src/renamed_main.rs".to_string()))
        );

        // f2
        assert_eq!(joined[1].id, f2);
        assert!(joined[1].node.is_some());
        assert_eq!(
            joined[1].row.as_ref().unwrap().values.get("path"),
            Some(&PropValue::Str("src/lib.rs".to_string()))
        );

        // invalid NodeId(8888)
        assert_eq!(joined[2].id, NodeId(8888));
        assert!(joined[2].node.is_none());
        assert!(joined[2].row.is_none());

        Ok::<_, BknError>(())
    })
    .unwrap();

    // 6. Hybrid Foreign Key Join (join_nodes_by_column)
    db.read_tx(|tx| {
        let joined = tx.join_nodes_by_column(&[f1, f2, f3], &HYBRID_SYMBOLS, "file_node_id")?;
        assert_eq!(joined.len(), 3);

        // f1 has 2 symbols
        assert_eq!(joined[0].id, f1);
        assert_eq!(joined[0].rows.len(), 2);

        // f2 has 1 symbol
        assert_eq!(joined[1].id, f2);
        assert_eq!(joined[1].rows.len(), 1);
        assert_eq!(
            joined[1].rows[0].values.get("name"),
            Some(&PropValue::Str("eval".to_string()))
        );

        // f3 has 0 symbols
        assert_eq!(joined[2].id, f3);
        assert_eq!(joined[2].rows.len(), 0);

        Ok::<_, BknError>(())
    })
    .unwrap();

    // 7. Reverse Join (join_rows_with_nodes)
    db.read_tx(|tx| {
        let sym_table = tx.relational().table(&HYBRID_SYMBOLS);
        let parse_rows = sym_table.select_prefix("name", "parse_")?;
        assert_eq!(parse_rows.len(), 2);

        let reverse_joined =
            tx.join_rows_with_nodes(&parse_rows, &HYBRID_SYMBOLS, "file_node_id")?;
        assert_eq!(reverse_joined.len(), 2);
        for (_row, node_opt) in reverse_joined {
            let node = node_opt.expect("must resolve to f1 node");
            assert_eq!(node.label, "File");
        }

        Ok::<_, BknError>(())
    })
    .unwrap();

    // 8. Graph Traversal + Hybrid Join
    db.read_tx(|tx| {
        let path = tx
            .graph()
            .find_shortest_path(f1, f3, Direction::Out, None)?
            .expect("path exists");
        let nodes = path.nodes();
        assert_eq!(nodes, vec![f1, f2, f3]);

        let joined_path = tx.join_nodes_with_table(&nodes, &HYBRID_FILES)?;
        assert_eq!(joined_path.len(), 3);
        assert_eq!(
            joined_path[0].row.as_ref().unwrap().values.get("path"),
            Some(&PropValue::Str("src/renamed_main.rs".to_string()))
        );
        assert_eq!(
            joined_path[1].row.as_ref().unwrap().values.get("path"),
            Some(&PropValue::Str("src/lib.rs".to_string()))
        );
        assert_eq!(
            joined_path[2].row.as_ref().unwrap().values.get("path"),
            Some(&PropValue::Str("tests/integration.rs".to_string()))
        );

        Ok::<_, BknError>(())
    })
    .unwrap();
}

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

/// Primary-key uniqueness, upsert/index consistency, and column-kind
/// validation — regressions for overwrites that used to leave stale index
/// entries behind and for writes that silently accepted any value.
pub fn relational_integrity_suite<B: StorageBackend>(backend: B) {
    use crate::BknError;
    use crate::relational::{ColumnDef, ColumnKind, RelSchema, RelationalDb};
    use crate::value::{PropValue, Properties};

    static PEOPLE: RelSchema = RelSchema {
        name: "people",
        columns: &[
            ColumnDef {
                name: "id",
                kind: ColumnKind::Int,
            },
            ColumnDef {
                name: "city",
                kind: ColumnKind::Str,
            },
            ColumnDef {
                name: "age",
                kind: ColumnKind::Int,
            },
        ],
        primary_key: "id",
        auto_increment_pk: false,
        indexed_columns: &["city"],
    };

    fn row(city: &str, age: i64) -> Properties {
        let mut p = Properties::new();
        p.insert("city".to_string(), PropValue::Str(city.to_string()));
        p.insert("age".to_string(), PropValue::Int(age));
        p
    }

    let db = RelationalDb::new(backend);
    let t = db.table(&PEOPLE);

    // Duplicate PK on plain insert is rejected and leaves the row untouched.
    t.insert_with_pk(PropValue::Int(1), row("Jakarta", 30))
        .unwrap();
    assert!(matches!(
        t.insert_with_pk(PropValue::Int(1), row("Bandung", 99)),
        Err(BknError::DuplicateKey { ref table, .. }) if table == "people"
    ));
    assert_eq!(
        t.get(&PropValue::Int(1)).unwrap().unwrap().values,
        row("Jakarta", 30)
    );

    // Duplicates inside one bulk insert are caught too, and roll the whole
    // batch back.
    let bulk = vec![
        (PropValue::Int(2), row("Medan", 1)),
        (PropValue::Int(2), row("Medan", 2)),
    ];
    assert!(matches!(
        t.insert_with_pk_bulk(bulk),
        Err(BknError::DuplicateKey { .. })
    ));
    assert!(t.get(&PropValue::Int(2)).unwrap().is_none());

    // Upsert replaces the row and moves its index entry.
    t.upsert_with_pk(PropValue::Int(1), row("Bandung", 31))
        .unwrap();
    assert!(
        t.select()
            .where_eq("city", PropValue::Str("Jakarta".into()))
            .run()
            .unwrap()
            .is_empty()
    );
    let hits = t
        .select()
        .where_eq("city", PropValue::Str("Bandung".into()))
        .run()
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].pk, PropValue::Int(1));
    db.write_tx(|tx| {
        let tbl = tx.table(&PEOPLE);
        assert!(
            tbl.select_eq("city", &PropValue::Str("Jakarta".into()))?
                .is_empty()
        );
        assert_eq!(
            tbl.select_eq("city", &PropValue::Str("Bandung".into()))?
                .len(),
            1
        );
        Ok(())
    })
    .unwrap();

    // On an auto-increment table an explicit pk is honored (and bumps the
    // counter past it); a taken one is a duplicate, not silently renumbered.
    static COUNTERS: RelSchema = RelSchema {
        name: "counters",
        auto_increment_pk: true,
        indexed_columns: &[],
        ..PEOPLE
    };
    let c = db.table(&COUNTERS);
    assert_eq!(c.insert(row("a", 1)).unwrap(), PropValue::Int(1));
    let mut explicit = row("b", 2);
    explicit.insert("id".to_string(), PropValue::Int(10));
    assert_eq!(c.insert(explicit.clone()).unwrap(), PropValue::Int(10));
    assert_eq!(c.insert(row("c", 3)).unwrap(), PropValue::Int(11));
    assert!(matches!(c.insert(explicit), Err(BknError::DuplicateKey { .. })));
    assert_eq!(c.get(&PropValue::Int(10)).unwrap().unwrap().values, row("b", 2));

    // Column-kind validation.
    let mut wrong_kind = row("Surabaya", 0);
    wrong_kind.insert("age".to_string(), PropValue::Str("old".into()));
    assert!(matches!(
        t.insert_with_pk(PropValue::Int(3), wrong_kind),
        Err(BknError::SchemaMismatch { ref table, .. }) if table == "people"
    ));
    let mut undeclared = row("Surabaya", 0);
    undeclared.insert("nickname".to_string(), PropValue::Str("x".into()));
    assert!(matches!(
        t.insert_with_pk(PropValue::Int(3), undeclared),
        Err(BknError::SchemaMismatch { .. })
    ));
    assert!(matches!(
        t.insert_with_pk(PropValue::Str("3".into()), row("Surabaya", 0)),
        Err(BknError::SchemaMismatch { .. })
    ));
    // Null is accepted in any non-pk column.
    let mut with_null = row("Surabaya", 0);
    with_null.insert("age".to_string(), PropValue::Null);
    t.insert_with_pk(PropValue::Int(3), with_null).unwrap();

    // Updates are validated as well, and a rejected update changes nothing.
    assert!(matches!(
        t.update()
            .where_eq("city", PropValue::Str("Bandung".into()))
            .set("age", PropValue::Bool(true))
            .run(),
        Err(BknError::SchemaMismatch { .. })
    ));
    assert_eq!(
        t.get(&PropValue::Int(1)).unwrap().unwrap().values,
        row("Bandung", 31)
    );

    // Predicate update/delete still work end to end.
    assert_eq!(
        t.update()
            .where_eq("city", PropValue::Str("Bandung".into()))
            .set("age", PropValue::Int(32))
            .run()
            .unwrap(),
        1
    );
    assert_eq!(
        t.delete()
            .where_eq("city", PropValue::Str("Surabaya".into()))
            .run()
            .unwrap(),
        1
    );
    assert!(t.get(&PropValue::Int(3)).unwrap().is_none());
}

/// Runtime schemas: catalog registration, NOT NULL / UNIQUE / DEFAULT,
/// migrations via `ensure_table`, and adopting a table previously used only
/// through a static `RelSchema`.
pub fn relational_catalog_suite<B: StorageBackend>(backend: B) {
    use crate::relational::{col, ColumnDef, ColumnKind, ColumnSchema, RelSchema, RelationalDb, TableSchema};
    use crate::value::{PropValue, Properties};
    use crate::BknError;

    fn props(pairs: &[(&str, PropValue)]) -> Properties {
        pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }

    let db = RelationalDb::new(backend);
    let users = TableSchema::builder("users")
        .column(ColumnSchema::new("id", ColumnKind::Int))
        .column(ColumnSchema::new("email", ColumnKind::Str).not_null().unique())
        .column(ColumnSchema::new("role", ColumnKind::Str).default_value("member"))
        .primary_key("id")
        .auto_increment()
        .build()
        .unwrap();

    // Catalog registration is idempotent for identical definitions.
    assert!(db.create_table(&users).unwrap());
    assert!(!db.create_table(&users).unwrap());
    let changed = users.to_builder().column(ColumnSchema::new("age", ColumnKind::Int)).build().unwrap();
    assert!(matches!(db.create_table(&changed), Err(BknError::SchemaMismatch { .. })));
    assert_eq!(db.list_tables().unwrap(), vec![users.clone()]);
    assert_eq!(db.table_schema("users").unwrap(), Some(users.clone()));
    assert!(matches!(db.table_named("nope"), Err(BknError::TableNotFound(_))));

    // DEFAULT, NOT NULL, UNIQUE.
    let t = db.table_named("users").unwrap();
    let alice = t.insert(props(&[("email", "a@x.io".into())])).unwrap();
    assert_eq!(t.get(&alice).unwrap().unwrap().values.get("role"), Some(&PropValue::from("member")));
    assert!(matches!(t.insert(props(&[("role", "admin".into())])), Err(BknError::ConstraintViolation { .. })));
    assert!(matches!(
        t.insert(props(&[("email", "a@x.io".into())])),
        Err(BknError::ConstraintViolation { .. })
    ));
    let bob = t.insert(props(&[("email", "b@x.io".into())])).unwrap();
    assert!(matches!(
        t.update().where_eq("id", bob.clone()).set("email", "a@x.io").run(),
        Err(BknError::ConstraintViolation { .. })
    ));
    // Re-setting a row's own unique value is fine.
    assert_eq!(t.update().where_eq("id", bob.clone()).set("email", "b@x.io").run().unwrap(), 1);

    // Upsert with an explicit pk on an auto-increment table moves the
    // counter past it, so later inserts never collide.
    t.upsert(props(&[("id", 100.into()), ("email", "c@x.io".into())])).unwrap();
    let next = t.insert(props(&[("email", "d@x.io".into())])).unwrap();
    assert_eq!(next, PropValue::Int(101));

    // Migration: add a column with a default (backfilled), index it, drop `role`.
    let v2 = users
        .to_builder()
        .column(ColumnSchema::new("score", ColumnKind::Int).not_null().default_value(0))
        .index("score")
        .drop_column("role")
        .build()
        .unwrap();
    db.ensure_table(&v2).unwrap();
    let t = db.table_named("users").unwrap();
    let a = t.get(&alice).unwrap().unwrap();
    assert_eq!(a.values.get("score"), Some(&PropValue::Int(0)));
    assert!(!a.values.contains_key("role"));
    assert_eq!(t.select().where_eq("score", 0).count().unwrap(), 4);

    // Failed migrations are atomic: making `score` UNIQUE fails on duplicates
    // and leaves the old definition in place.
    let bad = v2
        .to_builder()
        .column(ColumnSchema::new("score", ColumnKind::Int).not_null().unique().default_value(0))
        .build()
        .unwrap();
    assert!(matches!(db.ensure_table(&bad), Err(BknError::ConstraintViolation { .. })));
    assert_eq!(db.table_schema("users").unwrap(), Some(v2.clone()));
    let kind_change = v2.to_builder().column(ColumnSchema::new("score", ColumnKind::Str)).build().unwrap();
    assert!(matches!(db.ensure_table(&kind_change), Err(BknError::SchemaMismatch { .. })));

    db.create_index("users", "email").unwrap(); // already indexed via UNIQUE: no-op
    db.drop_index("users", "score").unwrap();
    assert!(!db.table_schema("users").unwrap().unwrap().is_indexed("score"));
    // `t` came from table_named, so it follows the migration; a handle built
    // from the now-outdated explicit schema refuses to run instead of
    // querying the dropped index.
    assert_eq!(t.select().filter(col("score").eq(0)).count().unwrap(), 4);
    assert!(matches!(db.table(&v2).select().count(), Err(BknError::SchemaMismatch { .. })));

    assert!(db.drop_table("users").unwrap());
    assert!(!db.drop_table("users").unwrap());
    assert!(db.list_tables().unwrap().is_empty());
    let recreated = db.table(&users);
    assert!(recreated.select().run().unwrap().is_empty());
    assert_eq!(recreated.insert(props(&[("email", "z@x.io".into())])).unwrap(), PropValue::Int(1));

    // Adopting a legacy static table rebuilds indexes that were added to the
    // static schema after rows existed (they were never backfilled).
    static LEGACY_V1: RelSchema = RelSchema {
        name: "legacy",
        columns: &[ColumnDef { name: "id", kind: ColumnKind::Int }, ColumnDef { name: "tag", kind: ColumnKind::Str }],
        primary_key: "id",
        auto_increment_pk: true,
        indexed_columns: &[],
    };
    static LEGACY_V2: RelSchema = RelSchema { indexed_columns: &["tag"], ..LEGACY_V1 };
    db.table(&LEGACY_V1).insert(props(&[("tag", "old".into())])).unwrap();
    assert!(db.table(&LEGACY_V2).select().where_eq("tag", "old").run().unwrap().is_empty(), "index not backfilled yet");
    db.ensure_table(&LEGACY_V2).unwrap();
    assert_eq!(db.table(&LEGACY_V2).select().where_eq("tag", "old").run().unwrap().len(), 1);
}

/// The query engine: expression filters, ordering, paging, projection and
/// aggregation, over pk, index and scan access paths.
pub fn relational_query_suite<B: StorageBackend>(backend: B) {
    use crate::relational::{col, Agg, AggregateRow, ColumnKind, ColumnSchema, Query, RelationalDb, Row, TableSchema};
    use crate::value::{PropValue, Properties};
    use crate::BknError;

    let db = RelationalDb::new(backend);
    let items = TableSchema::builder("items")
        .column(ColumnSchema::new("id", ColumnKind::Int))
        .column(ColumnSchema::new("cat", ColumnKind::Str))
        .column(ColumnSchema::new("price", ColumnKind::Int))
        .column(ColumnSchema::new("weight", ColumnKind::Float))
        .column(ColumnSchema::new("note", ColumnKind::Str))
        .primary_key("id")
        .index("cat")
        .index("price")
        .build()
        .unwrap();
    db.create_table(&items).unwrap();
    let t = db.table(&items);
    let data: [(i64, &str, i64, f64, Option<&str>); 6] = [
        (1, "fruit", 10, 0.5, Some("fresh")),
        (2, "fruit", 30, 1.5, None),
        (3, "veg", 20, 2.0, Some("organic")),
        (4, "veg", 40, 1.0, None),
        (5, "meat", 50, 3.0, Some("frozen")),
        (6, "fruit", 20, 0.25, None),
    ];
    for (id, cat, price, weight, note) in data {
        let mut p = Properties::new();
        p.insert("id".into(), id.into());
        p.insert("cat".into(), cat.into());
        p.insert("price".into(), price.into());
        p.insert("weight".into(), weight.into());
        p.insert("note".into(), note.into());
        t.insert(p).unwrap();
    }
    fn ids(rows: Vec<Row>) -> Vec<i64> {
        rows.into_iter()
            .map(|r| match r.pk {
                PropValue::Int(i) => i,
                other => panic!("unexpected pk {other:?}"),
            })
            .collect()
    }

    // Access paths: pk eq / IN / range, index eq / IN / range / prefix, scan.
    assert_eq!(ids(t.select().where_eq("id", 3).run().unwrap()), vec![3]);
    assert_eq!(ids(t.select().filter(col("id").is_in([5, 1, 5, 99])).order_by_asc("id").run().unwrap()), vec![1, 5]);
    assert_eq!(ids(t.select().filter(col("id").gt(2).and(col("id").le(4))).run().unwrap()), vec![3, 4]);
    assert_eq!(ids(t.select().where_eq("cat", "veg").order_by_asc("id").run().unwrap()), vec![3, 4]);
    assert_eq!(ids(t.select().filter(col("cat").is_in(["meat", "veg"])).order_by_asc("id").run().unwrap()), vec![3, 4, 5]);
    assert_eq!(ids(t.select().filter(col("price").between(20, 30)).order_by_asc("id").run().unwrap()), vec![2, 3, 6]);
    assert_eq!(
        ids(t
            .select()
            .where_range("price", Bound::Excluded(20.into()), Bound::Unbounded)
            .order_by_asc("id")
            .run()
            .unwrap()),
        vec![2, 4, 5]
    );
    assert_eq!(ids(t.select().where_prefix("cat", "fr").order_by_asc("id").run().unwrap()), vec![1, 2, 6]);
    // Range on an unindexed column works too (scan), comparing Float to Int.
    assert_eq!(ids(t.select().filter(col("weight").lt(1)).order_by_asc("id").run().unwrap()), vec![1, 6]);
    // Contradictory ranges (on the pk and on an index) are simply empty.
    assert!(t.select().filter(col("id").gt(4).and(col("id").lt(2))).run().unwrap().is_empty());
    assert!(t.select().filter(col("id").gt(3).and(col("id").lt(3))).run().unwrap().is_empty());
    assert_eq!(t.select().filter(col("price").ge(40).and(col("price").le(10))).count().unwrap(), 0);

    // Boolean logic and null handling.
    let q = col("cat").eq("fruit").and(col("price").ge(20)).or(col("cat").eq("meat"));
    assert_eq!(ids(t.select().filter(q).order_by_asc("id").run().unwrap()), vec![2, 5, 6]);
    assert_eq!(ids(t.select().filter(col("cat").eq("fruit").not()).order_by_asc("id").run().unwrap()), vec![3, 4, 5]);
    assert_eq!(ids(t.select().filter(col("cat").ne("fruit")).order_by_asc("id").run().unwrap()), vec![3, 4, 5]);
    assert_eq!(ids(t.select().filter(col("note").is_null()).order_by_asc("id").run().unwrap()), vec![2, 4, 6]);
    assert_eq!(ids(t.select().filter(col("note").is_not_null()).order_by_asc("id").run().unwrap()), vec![1, 3, 5]);
    assert_eq!(t.select().filter(col("note").gt("a")).count().unwrap(), 3, "null never compares");

    // Ordering (multi-key, desc), offset, limit, projection.
    assert_eq!(ids(t.select().order_by_asc("cat").order_by_desc("price").run().unwrap()), vec![2, 6, 1, 5, 4, 3]);
    assert_eq!(ids(t.select().order_by_desc("price").offset(1).limit(2).run().unwrap()), vec![4, 2]);
    assert!(t.select().order_by_desc("price").offset(10).run().unwrap().is_empty());
    assert_eq!(t.select().limit(3).run().unwrap().len(), 3);
    assert_eq!(ids(t.select().offset(4).run().unwrap()), vec![5, 6]);
    // ORDER BY pk over pk-ordered access paths (streamed, no sort) and over
    // an index path (sorted) agree.
    assert_eq!(ids(t.select().order_by_asc("id").offset(1).limit(2).run().unwrap()), vec![2, 3]);
    assert_eq!(ids(t.select().filter(col("id").gt(2)).order_by_asc("id").limit(2).run().unwrap()), vec![3, 4]);
    assert_eq!(ids(t.select().where_eq("cat", "fruit").order_by_asc("id").run().unwrap()), vec![1, 2, 6]);
    assert_eq!(ids(t.select().filter(col("weight").ge(1)).order_by_desc("id").limit(2).run().unwrap()), vec![5, 4]);
    let projected = t.select().where_eq("id", 1).columns(["price"]).run().unwrap();
    assert_eq!(projected[0].values.keys().collect::<Vec<_>>(), vec!["price"]);
    assert_eq!(projected[0].pk, PropValue::Int(1));

    // Unknown columns are reported rather than silently matching nothing.
    assert!(matches!(t.select().where_eq("colour", "red").run(), Err(BknError::SchemaMismatch { .. })));

    // Aggregates.
    assert_eq!(t.select().count().unwrap(), 6);
    assert_eq!(
        t.select()
            .where_eq("cat", "fruit")
            .aggregate(&[
                Agg::count(),
                Agg::sum("price"),
                Agg::avg("price"),
                Agg::min("weight"),
                Agg::max("price"),
                Agg::count_column("note"),
            ])
            .unwrap(),
        vec![
            PropValue::Int(3),
            PropValue::Int(60),
            PropValue::Float(20.0),
            PropValue::Float(0.25),
            PropValue::Int(30),
            PropValue::Int(1),
        ]
    );
    assert_eq!(
        t.select().where_eq("cat", "none").aggregate(&[Agg::count(), Agg::sum("price")]).unwrap(),
        vec![PropValue::Int(0), PropValue::Null]
    );
    assert_eq!(
        t.select().aggregate_by(&["cat"], &[Agg::count(), Agg::sum("weight")]).unwrap(),
        vec![
            AggregateRow { group: vec!["fruit".into()], values: vec![PropValue::Int(3), PropValue::Float(2.25)] },
            AggregateRow { group: vec!["meat".into()], values: vec![PropValue::Int(1), PropValue::Float(3.0)] },
            AggregateRow { group: vec!["veg".into()], values: vec![PropValue::Int(2), PropValue::Float(3.0)] },
        ]
    );
    assert!(matches!(t.select().aggregate(&[Agg::sum("cat")]), Err(BknError::SchemaMismatch { .. })));

    // The same Query runs inside transactions; update/delete honor it.
    db.write_tx(|tx| {
        let mut tbl = tx.table(&items);
        let cheap = Query::new().filter(col("price").lt(25));
        assert_eq!(tbl.count(&cheap)?, 3);
        assert_eq!(tbl.update_where(&cheap, &[("note", "sale".into())])?, 3);
        assert_eq!(tbl.find(&Query::new().where_eq("note", "sale"))?.len(), 3);
        assert_eq!(tbl.delete_where(&Query::new().where_eq("cat", "meat"))?, 1);
        Ok(())
    })
    .unwrap();
    assert_eq!(t.select().count().unwrap(), 5);
    assert_eq!(t.select().where_eq("note", "sale").count().unwrap(), 3);
    assert_eq!(t.delete().filter(col("price").ge(30)).run().unwrap(), 2);
    assert_eq!(ids(t.select().order_by_asc("id").run().unwrap()), vec![1, 3, 6]);
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

/// Label and property indexes: lookups, maintenance on every node write,
/// and the fallback + rebuild path for files created before the indexes
/// existed.
pub fn graph_index_suite<B: StorageBackend>(backend: B) {
    use std::sync::Arc;

    use crate::graph::{Direction, GraphDb};
    use crate::value::{PropValue, Properties};

    fn props(pairs: &[(&str, PropValue)]) -> Properties {
        pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }

    let backend = Arc::new(backend);
    let g = GraphDb::from_arc(backend.clone());
    let alice = g.create_node("Person", props(&[("name", "alice".into()), ("age", 30.into())])).unwrap();
    let bob = g.create_node("Person", props(&[("name", "bob".into()), ("age", 30.into())])).unwrap();
    let bulk = g
        .create_nodes_bulk([
            ("Person", props(&[("name", "carol".into()), ("age", 41.into())])),
            ("City", props(&[("name", "Jakarta".into())])),
        ])
        .unwrap();
    let (carol, jakarta) = (bulk[0], bulk[1]);
    g.create_edge(alice, "LIVES_IN", jakarta, Properties::new()).unwrap();
    g.create_edge(bob, "LIVES_IN", jakarta, Properties::new()).unwrap();

    assert_eq!(g.nodes_by_label("Person").unwrap(), vec![alice, bob, carol]);
    assert_eq!(g.nodes_by_label("City").unwrap(), vec![jakarta]);
    assert!(g.nodes_by_label("Pers").unwrap().is_empty(), "labels match exactly, not by prefix");

    // Without a property index: label scan + filter.
    assert_eq!(g.find_nodes("Person", "age", &30.into()).unwrap(), vec![alice, bob]);
    assert!(g.find_nodes("City", "age", &30.into()).unwrap().is_empty());

    // With one: backfilled on creation, then maintained by writes.
    assert!(g.create_property_index("Person", "age").unwrap());
    assert!(!g.create_property_index("Person", "age").unwrap());
    assert_eq!(g.property_indexes().unwrap(), vec![("Person".to_string(), "age".to_string())]);
    assert_eq!(g.find_nodes("Person", "age", &30.into()).unwrap(), vec![alice, bob]);
    g.update_node_properties(bob, |p| {
        p.insert("age".into(), 31.into());
    })
    .unwrap();
    assert_eq!(g.find_nodes("Person", "age", &30.into()).unwrap(), vec![alice]);
    assert_eq!(g.find_nodes("Person", "age", &31.into()).unwrap(), vec![bob]);
    let dave = g.create_node("Person", props(&[("age", 30.into())])).unwrap();
    assert_eq!(g.find_nodes("Person", "age", &30.into()).unwrap(), vec![alice, dave]);
    g.delete_node(alice).unwrap();
    assert_eq!(g.find_nodes("Person", "age", &30.into()).unwrap(), vec![dave]);
    assert_eq!(g.nodes_by_label("Person").unwrap(), vec![bob, carol, dave]);
    // Values the index can't hold still work (scan fallback).
    let eve = g.create_node("Person", props(&[("age", PropValue::Float(30.5))])).unwrap();
    assert_eq!(g.find_nodes("Person", "age", &PropValue::Float(30.5)).unwrap(), vec![eve]);

    // Batch writes maintain the indexes too, and see their own writes.
    g.write_tx(|tx| {
        let mut b = tx.graph();
        let frank = b.create_node("Person", props(&[("age", 30.into())]))?;
        assert_eq!(b.find_nodes("Person", "age", &30.into())?, vec![dave, frank]);
        b.delete_node(dave)?;
        assert_eq!(b.find_nodes("Person", "age", &30.into())?, vec![frank]);
        Ok(())
    })
    .unwrap();

    // top_hubs restricted to a label.
    let hubs = g.top_hubs(5, Direction::In, Some("City")).unwrap();
    assert_eq!(hubs, vec![(jakarta, 1)], "alice's edge went with her");

    assert!(g.drop_property_index("Person", "age").unwrap());
    assert!(g.property_indexes().unwrap().is_empty());
    assert_eq!(g.find_nodes("Person", "age", &31.into()).unwrap(), vec![bob]);

    // Simulate a file from before the label index existed: no marker and
    // no entries. Lookups must stay correct (full scan), and a rebuild
    // restores the index.
    {
        let mut w = backend.begin_write().unwrap();
        w.delete(TableSpec("meta"), b"graph:label_index").unwrap();
        for (k, _) in w.range(TableSpec("node_labels"), Bound::Unbounded, Bound::Unbounded).unwrap() {
            w.delete(TableSpec("node_labels"), &k).unwrap();
        }
        w.commit().unwrap();
    }
    let people = g.nodes_by_label("Person").unwrap();
    assert!(people.contains(&bob) && people.contains(&carol) && !people.contains(&alice));
    g.rebuild_indexes().unwrap();
    assert_eq!(g.nodes_by_label("Person").unwrap(), people);
    let r = backend.begin_read().unwrap();
    assert!(!r.range(TableSpec("node_labels"), Bound::Unbounded, Bound::Unbounded).unwrap().is_empty());
}

/// Dijkstra over edge weights.
pub fn graph_weighted_path_suite<B: StorageBackend>(backend: B) {
    use crate::graph::{Direction, GraphDb};
    use crate::value::{PropValue, Properties};

    fn w(v: PropValue) -> Properties {
        [("cost".to_string(), v)].into_iter().collect()
    }

    let g = GraphDb::new(backend);
    let [a, b, c, d] = ["a", "b", "c", "d"].map(|n| g.create_node(n, Properties::new()).unwrap());
    let ab = g.create_edge(a, "ROAD", b, w(1.into())).unwrap();
    let bc = g.create_edge(b, "ROAD", c, w(PropValue::Float(1.5))).unwrap();
    g.create_edge(a, "ROAD", c, w(5.into())).unwrap();
    g.create_edge(c, "FERRY", d, Properties::new()).unwrap(); // no weight: default

    // BFS picks the direct edge; Dijkstra the cheaper detour.
    assert_eq!(g.find_shortest_path(a, c, Direction::Out, None).unwrap().unwrap().nodes(), vec![a, c]);
    let best = g.find_weighted_path(a, c, Direction::Out, None, "cost", 1.0).unwrap().unwrap();
    assert_eq!(best.path.nodes(), vec![a, b, c]);
    assert_eq!(best.path.edges(), vec![ab, bc]);
    assert_eq!(best.cost, 2.5);

    let to_d = g.find_weighted_path(a, d, Direction::Out, None, "cost", 10.0).unwrap().unwrap();
    assert_eq!(to_d.cost, 12.5);
    assert!(g.find_weighted_path(a, d, Direction::Out, Some(&["ROAD"]), "cost", 1.0).unwrap().is_none());
    assert!(g.find_weighted_path(d, a, Direction::Out, None, "cost", 1.0).unwrap().is_none());
    assert_eq!(g.find_weighted_path(d, a, Direction::Both, None, "cost", 1.0).unwrap().unwrap().cost, 3.5);
    assert_eq!(g.find_weighted_path(a, a, Direction::Out, None, "cost", 1.0).unwrap().unwrap().cost, 0.0);

    let e = g.create_node("e", Properties::new()).unwrap();
    g.create_edge(d, "ROAD", e, w((-1).into())).unwrap();
    assert!(g.find_weighted_path(a, e, Direction::Out, None, "cost", 1.0).is_err(), "negative weights are rejected");
    assert!(g.find_weighted_path(a, c, Direction::Out, None, "cost", -1.0).is_err());
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

/// `Db::stats`: logical counts from one snapshot.
#[cfg(all(feature = "graph", feature = "relational"))]
pub fn db_stats_suite<B: StorageBackend>(backend: B) {
    use crate::graph::Properties;
    use crate::relational::{ColumnKind, ColumnSchema, TableSchema};
    use crate::{Db, DbStats};

    let db = Db::new(backend);
    assert_eq!(db.stats().unwrap(), DbStats::default());

    let graph = db.graph();
    let a = graph.create_node("P", Properties::new()).unwrap();
    let b = graph.create_node("P", Properties::new()).unwrap();
    let c = graph.create_node("Q", Properties::new()).unwrap();
    graph.create_edge(a, "knows", b, Properties::new()).unwrap();
    graph.create_edge(b, "knows", c, Properties::new()).unwrap();

    let rel = db.relational();
    for name in ["zeta", "alpha"] {
        let schema = TableSchema::builder(name)
            .column(ColumnSchema::new("id", ColumnKind::Int))
            .column(ColumnSchema::new("v", ColumnKind::Str))
            .primary_key("id")
            .auto_increment()
            .index("v")
            .build()
            .unwrap();
        rel.create_table(&schema).unwrap();
    }
    let alpha = rel.table_named("alpha").unwrap();
    for v in ["x", "y", "z"] {
        let mut p = Properties::new();
        p.insert("v".into(), v.into());
        alpha.insert(p).unwrap();
    }

    let stats = db.stats().unwrap();
    assert_eq!((stats.nodes, stats.edges), (3, 2));
    assert_eq!(stats.tables, vec![("alpha".to_string(), 3), ("zeta".to_string(), 0)]);

    graph.delete_node(c).unwrap(); // cascades its edge
    let stats = db.stats().unwrap();
    assert_eq!((stats.nodes, stats.edges), (2, 1));
}

/// Timestamp / Uuid / List / Map values: storage, keys, indexes, filters on
/// nested paths, and graph property lookups that mustn't confuse kinds.
#[cfg(all(feature = "graph", feature = "relational"))]
pub fn value_types_suite<B: StorageBackend>(backend: B) {
    use std::collections::BTreeMap;

    use crate::graph::Properties as GraphProps;
    use crate::relational::{col, ColumnKind, ColumnSchema, TableSchema};
    use crate::value::{PropValue, Properties};
    use crate::{BknError, Db};

    let db = Db::new(backend);
    let rel = db.relational();
    let events = TableSchema::builder("events")
        .column(ColumnSchema::new("id", ColumnKind::Uuid))
        .column(ColumnSchema::new("at", ColumnKind::Timestamp))
        .column(ColumnSchema::new("tags", ColumnKind::List))
        .column(ColumnSchema::new("meta", ColumnKind::Map))
        .column(ColumnSchema::new("title", ColumnKind::Str))
        .primary_key("id")
        .index("at")
        .build()
        .unwrap();
    rel.create_table(&events).unwrap();
    let t = rel.table_named("events").unwrap();

    let uuid = |n: u8| PropValue::Uuid([n; 16]);
    let meta = |author: &str, score: i64| {
        let mut m = BTreeMap::new();
        m.insert("author".to_string(), PropValue::Str(author.into()));
        m.insert("score".to_string(), PropValue::Int(score));
        PropValue::Map(m)
    };
    let rows = [
        (3u8, 3_000, vec!["rust", "db"], "ana", 7, "Graph Databases"),
        (1, 1_000, vec!["python"], "budi", 9, "Intro to Python"),
        (2, 2_000, vec!["rust"], "ana", 1, "rust tips"),
    ];
    for (id, at, tags, author, score, title) in rows {
        let mut p = Properties::new();
        p.insert("id".into(), uuid(id));
        p.insert("at".into(), PropValue::Timestamp(at));
        p.insert("tags".into(), PropValue::List(tags.into_iter().map(PropValue::from).collect()));
        p.insert("meta".into(), meta(author, score));
        p.insert("title".into(), title.into());
        t.insert(p).unwrap();
    }
    let ids = |rows: Vec<crate::relational::Row>| -> Vec<PropValue> { rows.into_iter().map(|r| r.pk).collect() };

    // Keys: pk get, pk order, timestamp index range.
    assert_eq!(t.get(&uuid(2)).unwrap().unwrap().values["title"], "rust tips".into());
    assert_eq!(ids(t.select().run().unwrap()), vec![uuid(1), uuid(2), uuid(3)]);
    let recent = t.select().filter(col("at").ge(PropValue::Timestamp(2_000))).order_by_asc("at").run().unwrap();
    assert_eq!(ids(recent), vec![uuid(2), uuid(3)]);
    // Kinds don't mix: an Int never matches a Timestamp column.
    assert_eq!(t.select().filter(col("at").ge(2_000)).count().unwrap(), 0);

    // List / Map / Str predicates and nested paths.
    assert_eq!(ids(t.select().filter(col("tags").contains("rust")).run().unwrap()), vec![uuid(2), uuid(3)]);
    assert_eq!(ids(t.select().filter(col("meta").contains("author")).run().unwrap()).len(), 3);
    assert_eq!(ids(t.select().filter(col("meta.author").eq("ana")).run().unwrap()), vec![uuid(2), uuid(3)]);
    assert_eq!(ids(t.select().filter(col("tags.0").eq("python")).run().unwrap()), vec![uuid(1)]);
    assert_eq!(ids(t.select().order_by_desc("meta.score").run().unwrap()), vec![uuid(1), uuid(3), uuid(2)]);
    assert_eq!(ids(t.select().filter(col("title").like("%tips")).run().unwrap()), vec![uuid(2)]);
    assert_eq!(ids(t.select().filter(col("title").ilike("graph%")).run().unwrap()), vec![uuid(3)]);
    assert_eq!(ids(t.select().filter(col("title").like("_ntro%Py%")).run().unwrap()), vec![uuid(1)]);
    assert_eq!(ids(t.select().filter(col("title").contains("to")).run().unwrap()), vec![uuid(1)]);
    assert!(matches!(t.select().filter(col("nope.x").eq(1)).run(), Err(BknError::SchemaMismatch { .. })));

    // Validation: kinds are enforced, and non-keyable kinds can't be indexed.
    let mut wrong = Properties::new();
    wrong.insert("id".into(), uuid(9));
    wrong.insert("at".into(), PropValue::Int(5));
    assert!(matches!(t.insert(wrong), Err(BknError::SchemaMismatch { .. })));
    assert!(rel.create_index("events", "tags").is_err());

    // Graph properties are untyped: an index lookup must not confuse
    // Timestamp(5) with Int(5).
    let g = db.graph();
    let mut pa = GraphProps::new();
    pa.insert("v".into(), PropValue::Int(5));
    let a = g.create_node("E", pa).unwrap();
    let mut pb = GraphProps::new();
    pb.insert("v".into(), PropValue::Timestamp(5));
    pb.insert("nested".into(), meta("x", 1));
    let b = g.create_node("E", pb).unwrap();
    g.create_property_index("E", "v").unwrap();
    assert_eq!(g.find_nodes("E", "v", &PropValue::Int(5)).unwrap(), vec![a]);
    assert_eq!(g.find_nodes("E", "v", &PropValue::Timestamp(5)).unwrap(), vec![b]);
    assert_eq!(g.get_node(b).unwrap().unwrap().properties["nested"], meta("x", 1));
}

/// The SQL subset: DDL, DML, queries, parameters, aggregates, errors, and
/// SQL inside an explicit transaction.
#[cfg(all(feature = "graph", feature = "relational"))]
pub fn sql_suite<B: StorageBackend>(backend: B) {
    use crate::lang::Params;
    use crate::relational::RelationalDb;
    use crate::value::PropValue;
    use crate::BknError;

    let db = RelationalDb::new(backend);
    let run = |sql: &str| db.sql(sql, ()).unwrap_or_else(|e| panic!("{sql}: {e}"));

    run("CREATE TABLE users (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            email TEXT NOT NULL UNIQUE,
            name VARCHAR(100),
            age INT DEFAULT 0,
            joined TIMESTAMP,
            tags LIST,
            meta JSON,
            INDEX (age)
        );");
    let schema = db.table_schema("users").unwrap().unwrap();
    assert!(schema.auto_increment_pk() && schema.is_indexed("age") && schema.is_indexed("email"));
    assert!(matches!(db.sql("CREATE TABLE users (id INT PRIMARY KEY)", ()), Err(BknError::InvalidQuery(_))));
    run("CREATE TABLE IF NOT EXISTS users (id INT PRIMARY KEY)");

    let out = run("INSERT INTO users (email, name, age, joined, tags, meta) VALUES
        ('ana@x.io', 'Ana', 30, TIMESTAMP '2026-01-15T08:00:00Z', ['admin', 'dev'], {team: 'core', level: 3}),
        ('budi@x.io', 'Budi', 25, TIMESTAMP '2026-03-01', ['dev'], {team: 'web', level: 1}),
        ('citra@x.io', 'Citra', 41, NULL, [], {team: 'core', level: 2})");
    assert_eq!(out.affected, 3);
    assert_eq!(out.columns, vec!["id"]);
    assert_eq!(out.rows, vec![vec![1.into()], vec![2.into()], vec![3.into()]]);
    let dup = db.sql("INSERT INTO users (email) VALUES ('ana@x.io')", ());
    assert!(matches!(dup, Err(BknError::ConstraintViolation { .. })), "{dup:?}");

    // Parameters: positional, numbered, named; every kind goes through.
    let out = db
        .sql(
            "INSERT INTO users (email, name, age, joined) VALUES (?, ?, ?, :when)",
            Params::positional([PropValue::from("dewi@x.io"), "Dewi".into(), PropValue::Int(19)]).with("when", PropValue::Timestamp(0)),
        )
        .unwrap();
    assert_eq!(out.rows, vec![vec![4.into()]]);

    let first_col = |sql: &str, params: Params| -> Vec<PropValue> {
        db.sql(sql, params).unwrap_or_else(|e| panic!("{sql}: {e}")).rows.into_iter().map(|r| r[0].clone()).collect()
    };
    let strs = |v: &[&str]| v.iter().map(|s| PropValue::from(*s)).collect::<Vec<_>>();
    let none = Params::none;
    assert_eq!(
        first_col("SELECT name FROM users WHERE age >= ? ORDER BY age DESC", Params::positional([25])),
        strs(&["Citra", "Ana", "Budi"])
    );
    assert_eq!(first_col("SELECT name FROM users WHERE 25 < age ORDER BY name", none()), strs(&["Ana", "Citra"]));
    assert_eq!(
        first_col("SELECT name FROM users WHERE age BETWEEN $1 AND $2 ORDER BY id", Params::positional([19, 30])),
        strs(&["Ana", "Budi", "Dewi"])
    );
    assert_eq!(
        first_col("SELECT name FROM users WHERE name IN ('Ana', 'Dewi', 'Zed') ORDER BY id", none()),
        strs(&["Ana", "Dewi"])
    );
    assert_eq!(
        first_col("SELECT name FROM users WHERE name NOT IN ('Ana') AND NOT (age < 20) ORDER BY id", none()),
        strs(&["Budi", "Citra"])
    );
    assert_eq!(first_col("SELECT name FROM users WHERE joined IS NULL", none()), strs(&["Citra"]));
    assert_eq!(first_col("SELECT name FROM users WHERE email LIKE 'b%'", none()), strs(&["Budi"]));
    assert_eq!(first_col("SELECT name FROM users WHERE name ILIKE '%I%' ORDER BY id", none()), strs(&["Budi", "Citra", "Dewi"]));
    assert_eq!(first_col("SELECT name FROM users WHERE name NOT LIKE '%i%' ORDER BY id", none()), strs(&["Ana"]));
    assert_eq!(first_col("SELECT name FROM users WHERE tags CONTAINS 'dev' ORDER BY id", none()), strs(&["Ana", "Budi"]));
    assert_eq!(
        first_col("SELECT name FROM users WHERE meta.team = 'core' ORDER BY meta.level DESC", none()),
        strs(&["Ana", "Citra"])
    );
    assert_eq!(
        first_col("SELECT name FROM users WHERE joined >= TIMESTAMP '2026-02-01' OR age = 41 ORDER BY id", none()),
        strs(&["Budi", "Citra"])
    );
    assert_eq!(first_col("SELECT name FROM users ORDER BY id LIMIT 2 OFFSET 1", none()), strs(&["Budi", "Citra"]));
    assert_eq!(first_col("SELECT name FROM users ORDER BY id LIMIT :n", Params::named([("n", 1)])), strs(&["Ana"]));

    let star = run("SELECT * FROM users WHERE id = 2");
    assert_eq!(star.columns, vec!["id", "email", "name", "age", "joined", "tags", "meta"]);
    assert_eq!(star.rows[0][3], PropValue::Int(25));
    let aliased = run("SELECT meta.team AS team, tags.0 first_tag FROM users WHERE id = 1");
    assert_eq!(aliased.columns, vec!["team", "first_tag"]);
    assert_eq!(aliased.rows, vec![vec![PropValue::from("core"), "admin".into()]]);

    // Aggregates.
    let agg = run("SELECT COUNT(*), SUM(age), MIN(age), MAX(age), AVG(age), COUNT(joined) FROM users");
    assert_eq!(agg.columns, vec!["count(*)", "sum(age)", "min(age)", "max(age)", "avg(age)", "count(joined)"]);
    assert_eq!(
        agg.rows,
        vec![vec![PropValue::Int(4), 115.into(), 19.into(), 41.into(), 28.75.into(), 3.into()]]
    );
    let grouped = run(
        "SELECT meta.team AS team, COUNT(*) AS n FROM users WHERE meta IS NOT NULL GROUP BY meta.team ORDER BY n DESC, team",
    );
    assert_eq!(grouped.rows, vec![vec![PropValue::from("core"), 2.into()], vec!["web".into(), 1.into()]]);
    assert!(matches!(db.sql("SELECT name, COUNT(*) FROM users", ()), Err(BknError::InvalidQuery(_))));

    // Updates, upserts, deletes.
    assert_eq!(run("UPDATE users SET age = 31, name = 'Ana S.' WHERE email = 'ana@x.io'").affected, 1);
    assert_eq!(first_col("SELECT name FROM users WHERE id = 1", none()), strs(&["Ana S."]));
    assert_eq!(run("INSERT OR REPLACE INTO users (id, email, name) VALUES (2, 'budi@x.io', 'Budi B.')").affected, 1);
    assert_eq!(run("SELECT age FROM users WHERE id = 2").rows, vec![vec![PropValue::Int(0)]], "replace resets to defaults");
    assert_eq!(run("UPSERT INTO users (id, email) VALUES (10, 'eka@x.io')").rows, vec![vec![PropValue::Int(10)]]);
    assert_eq!(run("DELETE FROM users WHERE age < 20 AND id <> 2 OR id = 10").affected, 2);
    assert_eq!(run("SELECT COUNT(*) FROM users").rows, vec![vec![PropValue::Int(3)]]);

    // Schema changes.
    run("ALTER TABLE users ADD COLUMN active BOOL DEFAULT TRUE");
    assert_eq!(run("SELECT COUNT(*) FROM users WHERE active = TRUE").rows, vec![vec![PropValue::Int(3)]]);
    run("ALTER TABLE users DROP COLUMN tags");
    assert!(db.table_schema("users").unwrap().unwrap().column("tags").is_none());
    run("CREATE INDEX idx_name ON users (name)");
    assert!(db.table_schema("users").unwrap().unwrap().is_indexed("name"));
    run("DROP INDEX ON users (name)");
    run("DROP INDEX IF EXISTS ON users (name)");

    // Errors carry positions / reasons.
    for (sql, needle) in [
        ("SELEC * FROM users", "expected SELECT"),
        ("SELECT * FROM users WHERE", "expected a column name"),
        ("SELECT * FROM users WHERE age >", "expected a value"),
        ("SELECT * FROM users LIMIT -1", "non-negative"),
        ("SELECT * FROM users WHERE age = ?", "parameter 1 is not bound"),
        ("SELECT * FROM users extra junk", "unexpected input"),
        ("SELECT * FROM users WHERE x = TIMESTAMP 'yesterday'", "invalid TIMESTAMP"),
        ("INSERT INTO users (email, name) VALUES ('a')", "has 1 values for 2 columns"),
    ] {
        let err = db.sql(sql, ()).unwrap_err().to_string();
        assert!(err.contains(needle), "{sql}: {err}");
    }
    assert!(matches!(db.sql("SELECT nope FROM users", ()), Err(BknError::SchemaMismatch { .. })));
    assert!(matches!(db.sql("SELECT * FROM ghosts", ()), Err(BknError::TableNotFound(_))));

    // Inside an explicit transaction: reads see pending writes; a failure
    // rolls everything back.
    let r: Result<(), BknError> = db.write_tx(|tx| {
        let mut v = tx.view();
        v.sql("INSERT INTO users (email) VALUES ('tx@x.io')", ())?;
        assert_eq!(v.sql("SELECT COUNT(*) FROM users", ())?.rows, vec![vec![PropValue::Int(4)]]);
        v.sql("INSERT INTO users (email) VALUES ('tx@x.io')", ())?; // duplicate: aborts
        Ok(())
    });
    assert!(r.is_err());
    assert_eq!(run("SELECT COUNT(*) FROM users").rows, vec![vec![PropValue::Int(3)]]);
    run("DROP TABLE users");
    assert!(matches!(db.sql("DROP TABLE users", ()), Err(BknError::TableNotFound(_))));
    run("DROP TABLE IF EXISTS users");
}

/// `MATCH` pattern queries over the graph.
#[cfg(all(feature = "graph", feature = "relational"))]
pub fn graph_query_suite<B: StorageBackend>(backend: B) {
    use std::collections::BTreeMap;

    use crate::graph::{GraphDb, Properties};
    use crate::lang::Params;
    use crate::value::PropValue;
    use crate::BknError;

    let db = GraphDb::new(backend);
    let person = |name: &str, age: i64| {
        let mut p = Properties::new();
        p.insert("name".into(), name.into());
        p.insert("age".into(), age.into());
        db.create_node("Person", p).unwrap()
    };
    let company = |name: &str| {
        let mut p = Properties::new();
        p.insert("name".into(), name.into());
        db.create_node("Company", p).unwrap()
    };
    let edge = |a, t: &str, b, since: Option<i64>| {
        let mut p = Properties::new();
        if let Some(s) = since {
            p.insert("since".into(), s.into());
        }
        db.create_edge(a, t, b, p).unwrap()
    };
    let (ana, budi, citra, dewi) = (person("ana", 30), person("budi", 25), person("citra", 41), person("dewi", 19));
    let (acme, globex) = (company("Acme"), company("Globex"));
    edge(ana, "KNOWS", budi, Some(2020));
    edge(budi, "KNOWS", citra, Some(2021));
    edge(citra, "KNOWS", ana, None);
    edge(ana, "KNOWS", dewi, Some(2020));
    edge(ana, "WORKS_AT", acme, None);
    edge(budi, "WORKS_AT", acme, None);
    edge(citra, "WORKS_AT", globex, None);

    let q = |text: &str, params: Params| db.query(text, params).unwrap_or_else(|e| panic!("{text}: {e}"));
    let col0 = |text: &str, params: Params| -> Vec<PropValue> { q(text, params).rows.into_iter().map(|r| r[0].clone()).collect() };
    let strs = |v: &[&str]| v.iter().map(|s| PropValue::from(*s)).collect::<Vec<_>>();
    let none = Params::none;

    for pass in ["scan", "indexed"] {
        if pass == "indexed" {
            db.create_property_index("Person", "name").unwrap();
        }
        // Directions.
        assert_eq!(col0("MATCH (a:Person {name: 'ana'})-[:KNOWS]->(b) RETURN b.name ORDER BY b.name", none()), strs(&["budi", "dewi"]), "{pass}");
        assert_eq!(col0("MATCH (b:Person {name: $n})<-[:KNOWS]-(a) RETURN a.name", Params::named([("n", "budi")])), strs(&["ana"]));
        assert_eq!(
            col0("MATCH (a:Person)-[:KNOWS]-(b) WHERE a.name = 'ana' RETURN b.name ORDER BY b.name", none()),
            strs(&["budi", "citra", "dewi"])
        );
        assert_eq!(col0("MATCH (a {name: 'dewi'})<--(b) RETURN b.name", none()), strs(&["ana"]));
        assert_eq!(col0("MATCH (a {name: 'dewi'})--(b) RETURN b.name", none()), strs(&["ana"]));
    }

    // Variable-length paths (edges never reused, so the 3-hop cycle back to
    // ana is found but not 4+ hops).
    assert_eq!(col0("MATCH (a {name: 'ana'})-[:KNOWS*2]->(c) RETURN c.name", none()), strs(&["citra"]));
    assert_eq!(
        col0("MATCH (a {name: 'ana'})-[:KNOWS*1..3]->(c) RETURN DISTINCT c.name ORDER BY c.name", none()),
        strs(&["ana", "budi", "citra", "dewi"])
    );
    let path = q("MATCH (a {name: 'ana'})-[p:KNOWS*..2]->(c {name: 'citra'}) RETURN p", none());
    let PropValue::List(edges) = &path.rows[0][0] else { panic!("{path:?}") };
    assert_eq!(edges.len(), 2);

    // Longer patterns, cross-variable WHERE, labels, cycles.
    let coworkers = q(
        "MATCH (a:Person)-[:WORKS_AT]->(c:Company)<-[:WORKS_AT]-(b:Person) WHERE a.name < b.name RETURN a.name, b.name, c.name",
        none(),
    );
    assert_eq!(coworkers.columns, vec!["a.name", "b.name", "c.name"]);
    assert_eq!(coworkers.rows, vec![strs(&["ana", "budi", "Acme"])]);
    assert_eq!(
        col0("MATCH (a)-[:KNOWS]->(b)-[:KNOWS]->(c)-[:KNOWS]->(a) RETURN a.name ORDER BY a.name", none()),
        strs(&["ana", "budi", "citra"])
    );
    assert_eq!(col0("MATCH (a {name: 'ana'})-->(x) WHERE x:Company RETURN x.name", none()), strs(&["Acme"]));
    assert_eq!(col0("MATCH (a {name: 'ana'})-[:WORKS_AT|KNOWS]->(x) RETURN count(*)", none()), vec![PropValue::Int(3)]);

    // Aggregation.
    let per_company = q(
        "MATCH (p:Person)-[:WORKS_AT]->(c:Company) RETURN c.name AS company, count(*) AS n, collect(p.name) AS people, avg(p.age) AS age ORDER BY n DESC",
        none(),
    );
    assert_eq!(per_company.columns, vec!["company", "n", "people", "age"]);
    assert_eq!(
        per_company.rows,
        vec![
            vec!["Acme".into(), 2.into(), PropValue::List(strs(&["ana", "budi"])), 27.5.into()],
            vec!["Globex".into(), 1.into(), PropValue::List(strs(&["citra"])), 41.0.into()],
        ]
    );
    assert_eq!(q("MATCH (p:Person {name: 'zed'}) RETURN count(*)", none()).rows, vec![vec![PropValue::Int(0)]]);
    assert_eq!(
        q("MATCH (p:Person) RETURN sum(p.age), min(p.age), max(p.name)", none()).rows,
        vec![vec![115.into(), 19.into(), "dewi".into()]]
    );

    // Functions, relationship variables, whole entities.
    let rel = q("MATCH (a)-[r:KNOWS {since: 2020}]->(b) RETURN type(r), r.since, a.name, b.name ORDER BY b.name", none());
    assert_eq!(rel.rows, vec![
        vec!["KNOWS".into(), 2020.into(), "ana".into(), "budi".into()],
        vec!["KNOWS".into(), 2020.into(), "ana".into(), "dewi".into()],
    ]);
    let whole = q("MATCH (n) WHERE id(n) = $id RETURN n, label(n)", Params::named([("id", dewi.0 as i64)]));
    let mut expected = BTreeMap::new();
    expected.insert("id".to_string(), PropValue::Int(dewi.0 as i64));
    expected.insert("label".to_string(), "Person".into());
    let mut props = BTreeMap::new();
    props.insert("name".to_string(), "dewi".into());
    props.insert("age".to_string(), 19.into());
    expected.insert("properties".to_string(), PropValue::Map(props));
    assert_eq!(whole.rows, vec![vec![PropValue::Map(expected), "Person".into()]]);
    let star = q("MATCH (a {name: 'budi'})-[r:WORKS_AT]->(c) RETURN *", none());
    assert_eq!(star.columns, vec!["a", "c", "r"]);

    // Predicates, paging.
    assert_eq!(col0("MATCH (p:Person) WHERE p.name STARTS WITH 'c' OR p.name ENDS WITH 'wi' RETURN p.name ORDER BY p.name", none()), strs(&["citra", "dewi"]));
    assert_eq!(col0("MATCH (p:Person) WHERE p.name IN ['ana', 'zed'] AND NOT p.age < 20 RETURN p.name", none()), strs(&["ana"]));
    assert_eq!(col0("MATCH (p:Person) WHERE p.city IS NULL AND p.name CONTAINS 'itr' RETURN p.name", none()), strs(&["citra"]));
    assert_eq!(col0("MATCH (p:Person) WHERE p.name ILIKE 'B%' RETURN p.name", none()), strs(&["budi"]));
    assert_eq!(col0("MATCH (p:Person) RETURN p.name ORDER BY p.age DESC SKIP 1 LIMIT 2", none()), strs(&["ana", "budi"]));
    assert_eq!(q("MATCH (p:Person) RETURN p.name LIMIT 2", none()).rows.len(), 2);
    assert_eq!(col0("MATCH (p:Person) WHERE p.age > $min RETURN p.name ORDER BY p.name", Params::named([("min", 29)])), strs(&["ana", "citra"]));

    // Errors.
    for (text, needle) in [
        ("CREATE (n)", "expected MATCH"),
        ("MATCH (a) RETURN b", "'b' is not defined"),
        ("MATCH (a)-[r]->(b) RETURN label(r)", "not a node"),
        ("MATCH (a)-[r*]->(b) RETURN r.since", "variable-length"),
        ("MATCH (a)-[*1..99]->(b) RETURN a", "hop range"),
        ("MATCH (a), (b) RETURN a", "single path pattern"),
        ("MATCH (a) RETURN count(*) ORDER BY a.name", "must name a RETURN column"),
    ] {
        let err = db.query(text, ()).unwrap_err();
        assert!(matches!(err, BknError::InvalidQuery(_)), "{text}: {err}");
        assert!(err.to_string().contains(needle), "{text}: {err}");
    }

    // Inside a write batch, queries see the batch's own writes.
    db.write_tx(|tx| {
        let mut g = tx.graph();
        let mut p = Properties::new();
        p.insert("name".into(), "eka".into());
        let eka = g.create_node("Person", p)?;
        g.create_edge(eka, "KNOWS", ana, Properties::new())?;
        let r = g.query("MATCH (e {name: 'eka'})-[:KNOWS]->(x) RETURN x.name", ())?;
        assert_eq!(r.rows, vec![vec![PropValue::from("ana")]]);
        Ok(())
    })
    .unwrap();
    let _ = globex;
}

/// Full-text (BM25) and vector search, including index maintenance.
#[cfg(all(feature = "graph", feature = "relational", feature = "search"))]
pub fn search_suite<B: StorageBackend>(backend: B) {
    use crate::relational::{col, pack_vector, ColumnKind, ColumnSchema, RelationalDb, ScoredRow, TableSchema, VectorMetric};
    use crate::value::{PropValue, Properties};
    use crate::BknError;

    let db = RelationalDb::new(backend);
    let docs = TableSchema::builder("docs")
        .column(ColumnSchema::new("id", ColumnKind::Int))
        .column(ColumnSchema::new("title", ColumnKind::Str))
        .column(ColumnSchema::new("body", ColumnKind::Str))
        .column(ColumnSchema::new("lang", ColumnKind::Str))
        .column(ColumnSchema::new("emb", ColumnKind::List))
        .column(ColumnSchema::new("packed", ColumnKind::Bytes))
        .primary_key("id")
        .build()
        .unwrap();
    db.create_table(&docs).unwrap();
    let t = db.table_named("docs").unwrap();
    let row = |id: i64, title: &str, body: &str, lang: &str, emb: [f32; 3]| {
        let mut p = Properties::new();
        p.insert("id".into(), id.into());
        p.insert("title".into(), title.into());
        p.insert("body".into(), body.into());
        p.insert("lang".into(), lang.into());
        p.insert("emb".into(), PropValue::List(emb.iter().map(|x| PropValue::Float(*x as f64)).collect()));
        p.insert("packed".into(), pack_vector(&emb));
        p
    };
    // Row 1 exists before the index: it must be backfilled.
    t.insert(row(1, "Rust ownership", "Ownership and borrowing in Rust, the borrow checker explained.", "en", [1.0, 0.0, 0.0]))
        .unwrap();
    assert!(db.create_fulltext_index("docs", "body").unwrap());
    assert!(!db.create_fulltext_index("docs", "body").unwrap());
    assert_eq!(db.fulltext_indexes("docs").unwrap(), vec!["body"]);
    t.insert(row(2, "Graph databases", "A graph database stores nodes and edges. Graph queries traverse edges.", "en", [0.0, 1.0, 0.0]))
        .unwrap();
    t.insert(row(3, "Basis data graf", "Basis data graf menyimpan simpul dan sisi; kueri graf menelusuri sisi.", "id", [0.0, 0.9, 0.1]))
        .unwrap();
    t.insert(row(4, "Rust and graphs", "Writing a graph database engine in Rust.", "en", [0.7, 0.7, 0.0])).unwrap();

    let ids = |hits: Vec<ScoredRow>| -> Vec<i64> {
        hits.into_iter()
            .map(|h| match h.row.pk {
                PropValue::Int(i) => i,
                other => panic!("{other:?}"),
            })
            .collect()
    };
    let search = |q: &str, all: bool| ids(db.search_text("docs", "body", q, 10, all, None).unwrap());

    // BM25: "graph" appears twice in doc 2 (short doc) and once in doc 4.
    assert_eq!(search("graph", false), vec![2, 4]);
    assert_eq!(search("GRAPH database", false)[..2], [2, 4]);
    assert_eq!(search("rust graph", true), vec![4], "match_all needs every term");
    assert_eq!(search("rust", false), vec![4, 1], "same tf: the shorter document ranks first");
    assert_eq!(search("borrow*", false), vec![1], "prefix matches 'borrowing' and 'borrow'");
    assert_eq!(search("graf", false), vec![3], "language-neutral tokens");
    assert!(search("nothing-here", false).is_empty());
    let hits = db.search_text("docs", "body", "graph", 1, false, Some(&col("lang").eq("en"))).unwrap();
    assert_eq!(ids(hits.clone()), vec![2]);
    assert!(hits[0].score > 0.0);
    assert_eq!(ids(db.search_text("docs", "body", "graph", 10, false, Some(&col("id").gt(2))).unwrap()), vec![4]);

    // Maintenance: update, delete, upsert, rolled-back writes.
    t.update().where_eq("id", 1).set("body", "Nothing about that topic anymore.").run().unwrap();
    assert_eq!(search("rust", false), vec![4]);
    assert_eq!(search("topic", false), vec![1]);
    t.delete().where_eq("id", 4).run().unwrap();
    assert!(search("rust", false).is_empty());
    let mut again = row(4, "x", "Rust again", "en", [0.0, 0.0, 1.0]);
    again.remove("id");
    t.upsert_with_pk(PropValue::Int(4), again).unwrap();
    assert_eq!(search("rust", false), vec![4]);
    let _ = db.write_tx(|tx| -> Result<(), BknError> {
        tx.view().table_named("docs")?.insert(row(9, "t", "ephemeral rust", "en", [1.0, 1.0, 1.0]))?;
        Err(BknError::NotFound) // roll back
    });
    assert_eq!(search("ephemeral", false), Vec::<i64>::new());

    // Errors.
    assert!(matches!(db.search_text("docs", "title", "x", 5, false, None), Err(BknError::InvalidQuery(_))));
    assert!(db.create_fulltext_index("docs", "emb").is_err());
    assert!(db.create_fulltext_index("docs", "nope").is_err());
    assert!(db.search_text("docs", "body", "x", 5, false, Some(&col("nope").eq(1))).is_err());

    // Vector search, both storage forms and all metrics.
    for column in ["emb", "packed"] {
        let near = |q: [f32; 3], metric| ids(db.search_vector("docs", column, &q, 2, metric, None).unwrap());
        assert_eq!(near([0.0, 1.0, 0.05], VectorMetric::Cosine), vec![2, 3], "{column}");
        assert_eq!(near([0.0, 0.0, 1.0], VectorMetric::Euclidean), vec![4, 3]);
        assert_eq!(near([2.0, 0.1, 0.0], VectorMetric::Dot), vec![1, 2]);
    }
    let best = db.search_vector("docs", "emb", &[0.0, 1.0, 0.0], 1, VectorMetric::Cosine, None).unwrap();
    assert!((best[0].score - 1.0).abs() < 1e-6);
    let filtered = db.search_vector("docs", "packed", &[0.0, 1.0, 0.0], 5, VectorMetric::Cosine, Some(&col("lang").eq("id"))).unwrap();
    assert_eq!(ids(filtered), vec![3]);
    assert!(matches!(db.search_vector("docs", "emb", &[1.0, 0.0], 3, VectorMetric::Dot, None), Err(BknError::InvalidQuery(_))));
    assert!(db.search_vector("docs", "title", &[1.0], 3, VectorMetric::Dot, None).is_err());

    // Dropping the column (migration) or the table drops the index data.
    let without_body = docs.to_builder().drop_column("body").build().unwrap();
    db.ensure_table(&without_body).unwrap();
    assert!(db.fulltext_indexes("docs").unwrap().is_empty());
    db.ensure_table(&docs).unwrap();
    db.create_fulltext_index("docs", "title").unwrap();
    assert!(db.drop_table("docs").unwrap());
    assert!(db.fulltext_indexes("docs").unwrap().is_empty());
    db.create_table(&docs).unwrap();
    db.create_fulltext_index("docs", "title").unwrap();
    assert!(db.search_text("docs", "title", "graph", 5, false, None).unwrap().is_empty(), "no stale postings");
    assert!(db.drop_fulltext_index("docs", "title").unwrap());
    assert!(!db.drop_fulltext_index("docs", "title").unwrap());
}
