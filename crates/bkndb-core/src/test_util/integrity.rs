//! Constraints (NOT NULL, UNIQUE, defaults) and the table catalog/migrations.
use super::*;

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
