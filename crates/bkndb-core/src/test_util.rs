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
        vec![(vec![2], vec![20]), (vec![3], vec![30]), (vec![4], vec![40])]
    );
}

/// Shared conformance suite for the graph layer (M2 CRUD, M3 cascade
/// delete + property mutation, M4 traversal), exercised identically
/// against every `StorageBackend` implementation.
#[cfg(feature = "graph")]
pub fn graph_conformance_suite<B: StorageBackend>(backend: B) {
    use crate::graph::{Direction, EdgeId, GraphDb, NodeId, Properties};
    use crate::BknError;

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
    let result = db.traversal().start(x).outgoing("calls").max_depth(2).run().unwrap();
    let nodes: Vec<NodeId> = result.iter().map(|n| n.node).collect();
    assert_eq!(nodes, vec![x, y, z]);
    assert_eq!(result[0].depth, 0);
    assert_eq!(result[1].depth, 1);
    assert_eq!(result[2].depth, 2);

    // incoming direction
    let rev = db.traversal().start(y).incoming("calls").max_depth(5).run().unwrap();
    let rev_nodes: Vec<NodeId> = rev.iter().map(|n| n.node).collect();
    assert_eq!(rev_nodes, vec![y, x]);

    // edge-type filter: y also has a non-"calls" edge that must be excluded
    db.create_edge(y, "imports", w, Properties::new()).unwrap();
    let filtered = db.traversal().start(y).outgoing("calls").max_depth(5).run().unwrap();
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
    assert_eq!(row1.values.get("name"), Some(&PropValue::Str("bolt".to_string())));

    // --- where_eq on an indexed column, duplicate values ---
    let red = table.select().where_eq("color", PropValue::Str("red".to_string())).run().unwrap();
    assert_eq!(red.len(), 2);
    assert!(red.iter().any(|r| r.pk == id1));
    assert!(red.iter().any(|r| r.pk == id2));

    // --- where_eq on an unindexed column (full-scan path) ---
    let cheap = table.select().where_eq("price", PropValue::Int(2)).run().unwrap();
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
    let red_after = table.select().where_eq("color", PropValue::Str("red".to_string())).run().unwrap();
    assert_eq!(red_after.len(), 1);
    assert_eq!(red_after[0].pk, id1);
    let green = table.select().where_eq("color", PropValue::Str("green".to_string())).run().unwrap();
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
    let red_final = table.select().where_eq("color", PropValue::Str("red".to_string())).run().unwrap();
    assert!(red_final.is_empty());

    // --- predicate matching nothing returns Ok(empty)/Ok(0), not an error ---
    let none = table.select().where_eq("color", PropValue::Str("purple".to_string())).run().unwrap();
    assert!(none.is_empty());
    let none_updated = table
        .update()
        .where_eq("color", PropValue::Str("purple".to_string()))
        .set("price", PropValue::Int(1))
        .run()
        .unwrap();
    assert_eq!(none_updated, 0);
    let none_deleted = table.delete().where_eq("color", PropValue::Str("purple".to_string())).run().unwrap();
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
    assert_eq!(all, vec![(b"a".to_vec(), b"1".to_vec()), (b"b".to_vec(), b"2".to_vec())]);
}

/// Proves every [`crate::kv::Kv`] method rejects every one of
/// `bkndb-core`'s reserved table names, while an ordinary name still works
/// end-to-end.
pub fn kv_reserved_name_conformance_suite<B: StorageBackend>(backend: B) {
    use crate::kv::Kv;
    use crate::BknError;

    let kv = Kv::new(backend);
    for &name in crate::RESERVED_TABLE_NAMES {
        let t = TableSpec(name);
        assert!(matches!(kv.get(t, b"k").unwrap_err(), BknError::ReservedTableName(_)));
        assert!(matches!(kv.put(t, b"k", b"v").unwrap_err(), BknError::ReservedTableName(_)));
        assert!(matches!(kv.delete(t, b"k").unwrap_err(), BknError::ReservedTableName(_)));
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
    use crate::graph::{GraphDb, Properties};
    use crate::BknError;

    let db = GraphDb::new(backend);
    let existing = db.create_node("Fn", Properties::new()).unwrap();

    // A batch that creates a node, an edge from it to a pre-existing node,
    // and then fails: neither the new node nor the new edge must exist.
    let new_node_id = std::sync::Mutex::new(None);
    let err = db.write_tx(|batch| -> Result<(), BknError> {
        let a = batch.graph().create_node("Fn", Properties::new())?;
        *new_node_id.lock().unwrap() = Some(a);
        batch.graph().create_edge(a, "calls", existing, Properties::new())?;
        Err(BknError::NotFound)
    });
    assert!(err.is_err());
    let a = new_node_id.into_inner().unwrap().unwrap();
    assert!(db.get_node(a).unwrap().is_none(), "a failed batch must leave the new node absent");
    assert!(db.neighbors_out(a, "calls").unwrap().is_empty());

    // A matching batch that succeeds: node + edge commit together.
    let (a, edge) = db
        .write_tx(|batch| {
            let a = batch.graph().create_node("Fn", Properties::new())?;
            let e = batch.graph().create_edge(a, "calls", existing, Properties::new())?;
            Ok::<_, BknError>((a, e))
        })
        .unwrap();
    assert!(db.get_node(a).unwrap().is_some());
    assert_eq!(db.neighbors_out(a, "calls").unwrap(), vec![(existing, edge)]);
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
    let node = db.graph().create_node("File", crate::graph::Properties::new()).unwrap();

    db.relational()
        .table(&ROWS)
        .insert_with_pk(PropValue::Int(node.0 as i64), RelProperties::new())
        .unwrap();

    db.kv().put(TableSpec("scratch"), b"k", b"v").unwrap();

    assert!(db.graph().get_node(node).unwrap().is_some());
    assert!(db.relational().table(&ROWS).get(&PropValue::Int(node.0 as i64)).unwrap().is_some());
    assert_eq!(db.kv().get(TableSpec("scratch"), b"k").unwrap(), Some(b"v".to_vec()));
}

/// Shared conformance suite for [`crate::db::Db::write_tx`] — the three-way
/// atomicity proof: a batch touching graph + relational + raw KV that fails
/// must leave all three completely untouched; a matching batch that
/// succeeds must commit all three together.
#[cfg(all(feature = "graph", feature = "relational"))]
pub fn db_write_tx_conformance_suite<B: StorageBackend>(backend: B) {
    use crate::db::Db;
    use crate::relational::{ColumnDef, ColumnKind, RelSchema};
    use crate::value::{PropValue, Properties as RelProperties};
    use crate::BknError;

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
        let node = batch.graph().create_node("File", crate::graph::Properties::new())?;
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
    assert!(db.graph().get_node(node).unwrap().is_none(), "failed batch must leave the graph node absent");
    assert!(
        db.relational().table(&ROWS).get(&PropValue::Int(node.0 as i64)).unwrap().is_none(),
        "failed batch must leave the relational row absent"
    );
    assert_eq!(db.kv().get(scratch, b"k").unwrap(), None, "failed batch must leave the kv key absent");

    let node = db
        .write_tx(|batch| {
            let node = batch.graph().create_node("File", crate::graph::Properties::new())?;
            batch
                .relational()
                .table(&ROWS)
                .insert_with_pk(PropValue::Int(node.0 as i64), RelProperties::new())?;
            batch.kv().put(scratch, b"k", b"v")?;
            Ok::<_, BknError>(node)
        })
        .unwrap();
    assert!(db.graph().get_node(node).unwrap().is_some());
    assert!(db.relational().table(&ROWS).get(&PropValue::Int(node.0 as i64)).unwrap().is_some());
    assert_eq!(db.kv().get(scratch, b"k").unwrap(), Some(b"v".to_vec()));
}

/// Shared conformance suite for [`crate::db::Db::read_tx`]
/// — proves consistent reading across graph, relational, and raw KV
/// over a single read transaction snapshot.
#[cfg(all(feature = "graph", feature = "relational"))]
pub fn db_read_tx_conformance_suite<B: StorageBackend>(backend: B) {
    use crate::db::Db;
    use crate::relational::{ColumnDef, ColumnKind, RelSchema};
    use crate::value::{PropValue, Properties as RelProperties};
    use crate::BknError;

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
            let a = batch.graph().create_node("File", crate::graph::Properties::new())?;
            let b = batch.graph().create_node("File", crate::graph::Properties::new())?;
            let e = batch.graph().create_edge(a, "imports", b, crate::graph::Properties::new())?;
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

        let row = tx.relational().table(&ROWS).get(&PropValue::Int(node_a.0 as i64))?.expect("row exists");
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
    use crate::db::Db;
    use crate::relational::{ColumnDef, ColumnKind, RelSchema};
    use crate::value::{PropValue, Properties as RelProperties};
    use crate::BknError;

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
        let a = batch.graph().create_node("File", crate::graph::Properties::new())?;
        let b = batch.graph().create_node("File", crate::graph::Properties::new())?;
        let e = batch.graph().create_edge(a, "calls", b, crate::graph::Properties::new())?;

        assert!(batch.graph().get_node(a)?.is_some());
        assert_eq!(batch.graph().neighbors_out(a, "calls")?, vec![(b, e)]);
        assert_eq!(batch.graph().out_degree(a)?, 1);
        assert_eq!(batch.graph().in_degree(b)?, 1);

        batch.relational().table(&ROWS).insert_with_pk(PropValue::Int(a.0 as i64), RelProperties::new())?;
        assert!(batch.relational().table(&ROWS).get(&PropValue::Int(a.0 as i64))?.is_some());

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
    use crate::relational::{ColumnDef, ColumnKind, RelSchema, RelationalDb};
    use crate::value::{PropValue, Properties};
    use crate::BknError;

    static A: RelSchema = RelSchema {
        name: "wtx_a",
        columns: &[ColumnDef {
            name: "id",
            kind: ColumnKind::Int,
        }, ColumnDef {
            name: "val",
            kind: ColumnKind::Str,
        }],
        primary_key: "id",
        auto_increment_pk: true,
        indexed_columns: &[],
    };
    static B_SCHEMA: RelSchema = RelSchema {
        name: "wtx_b",
        columns: &[ColumnDef {
            name: "id",
            kind: ColumnKind::Int,
        }, ColumnDef {
            name: "val",
            kind: ColumnKind::Str,
        }],
        primary_key: "id",
        auto_increment_pk: true,
        indexed_columns: &[],
    };
    static C: RelSchema = RelSchema {
        name: "wtx_c",
        columns: &[ColumnDef {
            name: "id",
            kind: ColumnKind::Int,
        }, ColumnDef {
            name: "val",
            kind: ColumnKind::Str,
        }],
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
    assert!(db.table(&B_SCHEMA).select().run().unwrap().is_empty(), "a failed batch must leave table B empty");
    assert!(db.table(&C).select().run().unwrap().is_empty(), "a failed batch must leave table C empty");

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
        db.table(&B_SCHEMA).get(&b_id).unwrap().unwrap().values.get("val"),
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
        db.table(&A).get(&d_table_pre).unwrap().unwrap().values.get("val"),
        Some(&PropValue::Str("deduped".to_string()))
    );
    assert_eq!(
        db.table(&A).get(&d_table_pre2).unwrap().unwrap().values.get("val"),
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
