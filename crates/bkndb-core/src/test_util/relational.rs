//! Relational and key-value basics, including reserved table names.
use super::*;

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
