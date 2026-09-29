//! Relational writes inside explicit write transactions.
use super::*;

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
pub(super) static HYBRID_FILES: crate::relational::RelSchema = crate::relational::RelSchema {
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
pub(super) static HYBRID_SYMBOLS: crate::relational::RelSchema = crate::relational::RelSchema {
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
