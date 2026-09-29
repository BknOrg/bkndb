//! Database statistics and the extended value types (timestamps, UUIDs, lists, maps).
use super::*;

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
