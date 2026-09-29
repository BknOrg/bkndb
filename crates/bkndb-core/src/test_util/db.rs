//! The combined `Db` facade: conformance, read/write transactions, batches.
use super::*;

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
