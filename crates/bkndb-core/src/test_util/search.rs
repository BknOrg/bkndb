//! Full-text, exact vector and approximate (HNSW) vector search.
use super::*;

/// Approximate (HNSW) vector indexes: recall against the exact scan for
/// every metric, maintenance through inserts/updates/deletes/rollbacks,
/// filters, and cleanup.
#[cfg(all(feature = "graph", feature = "relational", feature = "search"))]
pub fn ann_suite<B: StorageBackend>(backend: B) {
    use std::collections::HashSet;

    use crate::relational::{
        col, pack_vector, ColumnKind, ColumnSchema, Expr, RelationalDb, ScoredRow, TableSchema, VectorIndexOptions, VectorMetric,
        VectorSearchOptions,
    };
    use crate::value::{PropValue, Properties};
    use crate::BknError;

    const N: i64 = 1000;
    const DIM: usize = 12;
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut rand = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        ((seed >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
    };
    let mut vector = move || -> Vec<f32> { (0..DIM).map(|_| rand()).collect() };

    let db = RelationalDb::new(backend);
    let schema = TableSchema::builder("vecs")
        .column(ColumnSchema::new("id", ColumnKind::Int))
        .column(ColumnSchema::new("grp", ColumnKind::Str))
        .column(ColumnSchema::new("emb", ColumnKind::Bytes))
        .column(ColumnSchema::new("lst", ColumnKind::List))
        .primary_key("id")
        .build()
        .unwrap();
    db.create_table(&schema).unwrap();
    let row = |id: i64, v: &[f32]| {
        let mut p = Properties::new();
        p.insert("id".into(), id.into());
        p.insert("grp".into(), if id % 2 == 0 { "a" } else { "b" }.into());
        p.insert("emb".into(), pack_vector(v));
        p
    };
    let insert_range = |range: std::ops::RangeInclusive<i64>, vector: &mut dyn FnMut() -> Vec<f32>| {
        db.write_tx(|tx| -> Result<(), BknError> {
            let mut view = tx.view();
            let mut t = view.table_named("vecs")?;
            for id in range {
                t.insert(row(id, &vector()))?;
            }
            Ok(())
        })
        .unwrap();
    };
    let ids = |hits: &[ScoredRow]| -> Vec<i64> {
        hits.iter()
            .map(|h| match h.row.pk {
                PropValue::Int(i) => i,
                ref other => panic!("{other:?}"),
            })
            .collect()
    };
    let exact = VectorSearchOptions { exact: true, ef_search: None };
    let search = |q: &[f32], k: usize, metric, filter: Option<&Expr>, options| {
        db.search_vector_with("vecs", "emb", q, k, metric, filter, options).unwrap()
    };
    let queries: Vec<Vec<f32>> = (0..25).map(|_| vector()).collect();
    let recall = |metric, filter: Option<&Expr>| -> f64 {
        let mut hit = 0usize;
        let mut total = 0usize;
        for q in &queries {
            let truth: HashSet<i64> = ids(&search(q, 10, metric, filter, exact)).into_iter().collect();
            let approx = search(q, 10, metric, filter, VectorSearchOptions::default());
            assert!(approx.windows(2).all(|w| match metric {
                VectorMetric::Euclidean => w[0].score <= w[1].score,
                _ => w[0].score >= w[1].score,
            }));
            hit += ids(&approx).iter().filter(|i| truth.contains(i)).count();
            total += truth.len();
        }
        hit as f64 / total as f64
    };
    let options = |metric| VectorIndexOptions { metric, m: 8, ef_construction: 64 };

    // Half the rows exist before the index (backfill), half arrive after.
    insert_range(1..=N / 2, &mut vector);
    assert!(db.create_vector_index("vecs", "emb", options(VectorMetric::Cosine)).unwrap());
    assert!(!db.create_vector_index("vecs", "emb", options(VectorMetric::Dot)).unwrap(), "one index per column");
    insert_range(N / 2 + 1..=N, &mut vector);
    let info = db.vector_indexes("vecs").unwrap();
    assert_eq!(info.len(), 1);
    assert_eq!((info[0].column.as_str(), info[0].metric, info[0].dimensions, info[0].vectors), ("emb", VectorMetric::Cosine, Some(DIM), N as u64));
    let r = recall(VectorMetric::Cosine, None);
    assert!(r >= 0.95, "cosine recall {r}");

    // Another metric than the index's: an exact scan.
    assert_eq!(ids(&search(&queries[0], 5, VectorMetric::Euclidean, None, Default::default())), ids(&search(&queries[0], 5, VectorMetric::Euclidean, None, exact)));

    // Filters: a broad one searches the index, a selective one falls back.
    let even = col("grp").eq("a");
    let hits = search(&queries[1], 10, VectorMetric::Cosine, Some(&even), Default::default());
    assert_eq!(hits.len(), 10);
    assert!(ids(&hits).iter().all(|i| i % 2 == 0));
    let r = recall(VectorMetric::Cosine, Some(&even));
    assert!(r >= 0.9, "filtered recall {r}");
    let one = col("id").eq(77);
    assert_eq!(ids(&search(&queries[2], 3, VectorMetric::Cosine, Some(&one), Default::default())), vec![77]);

    // A row's own vector finds it first.
    let own = |id: i64| -> Vec<f32> {
        let r = db.table_named("vecs").unwrap().get(&PropValue::Int(id)).unwrap().unwrap();
        crate::relational::vector_of(&r.values["emb"]).unwrap()
    };
    let v42 = own(42);
    assert_eq!(ids(&search(&v42, 1, VectorMetric::Cosine, None, Default::default())), vec![42]);

    // Maintenance: deletes, vector updates, rollback.
    let t = db.table_named("vecs").unwrap();
    t.delete().filter(col("id").le(300)).run().unwrap();
    for id in 301..=400 {
        t.update().where_eq("id", id).set("emb", pack_vector(&vector())).run().unwrap();
    }
    assert_eq!(db.vector_indexes("vecs").unwrap()[0].vectors, (N - 300) as u64);
    let r = recall(VectorMetric::Cosine, None);
    assert!(r >= 0.9, "recall after deletes/updates {r}");
    assert!(queries.iter().all(|q| ids(&search(q, 10, VectorMetric::Cosine, None, Default::default())).iter().all(|i| *i > 300)));
    let v350 = own(350);
    assert_eq!(ids(&search(&v350, 1, VectorMetric::Cosine, None, Default::default())), vec![350]);
    let probe = vector();
    let _ = db.write_tx(|tx| -> Result<(), BknError> {
        tx.view().table_named("vecs")?.insert(row(5000, &probe))?;
        Err(BknError::NotFound) // roll back
    });
    assert_ne!(ids(&search(&probe, 1, VectorMetric::Cosine, None, Default::default())), vec![5000]);
    assert_eq!(db.vector_indexes("vecs").unwrap()[0].vectors, (N - 300) as u64);
    // Clearing a vector (Null) un-indexes the row.
    t.update().where_eq("id", 350).set("emb", PropValue::Null).run().unwrap();
    assert_ne!(ids(&search(&v350, 1, VectorMetric::Cosine, None, Default::default())), vec![350]);

    // Errors: wrong dimensions (write rejected atomically), bad column/params.
    assert!(matches!(db.search_vector("vecs", "emb", &[1.0, 2.0], 3, VectorMetric::Cosine, None), Err(BknError::InvalidQuery(_))));
    assert!(matches!(t.insert(row(6000, &[1.0, 2.0])), Err(BknError::InvalidQuery(_))));
    assert!(t.get(&PropValue::Int(6000)).unwrap().is_none());
    assert!(matches!(db.create_vector_index("vecs", "grp", options(VectorMetric::Cosine)), Err(BknError::SchemaMismatch { .. })));
    assert!(matches!(db.create_vector_index("vecs", "nope", options(VectorMetric::Cosine)), Err(BknError::SchemaMismatch { .. })));
    assert!(matches!(
        db.create_vector_index("vecs", "lst", VectorIndexOptions { m: 1, ..Default::default() }),
        Err(BknError::InvalidQuery(_))
    ));

    // The other metrics, built by backfill.
    for metric in [VectorMetric::Euclidean, VectorMetric::Dot] {
        assert!(db.drop_vector_index("vecs", "emb").unwrap());
        assert!(db.create_vector_index("vecs", "emb", options(metric)).unwrap());
        let r = recall(metric, None);
        assert!(r >= 0.95, "{metric:?} recall {r}");
    }

    // A small table is searched exhaustively: identical to the exact scan,
    // also as it shrinks to nothing (entry point removed) and regrows.
    let small = TableSchema::builder("small")
        .column(ColumnSchema::new("id", ColumnKind::Int))
        .column(ColumnSchema::new("v", ColumnKind::List))
        .primary_key("id")
        .build()
        .unwrap();
    db.create_table(&small).unwrap();
    db.create_vector_index("small", "v", VectorIndexOptions::default()).unwrap();
    let st = db.table_named("small").unwrap();
    let list = |v: &[f32]| PropValue::List(v.iter().map(|x| PropValue::Float(*x as f64)).collect());
    for id in 1..=30 {
        let mut p = Properties::new();
        p.insert("id".into(), id.into());
        p.insert("v".into(), list(&vector()));
        st.insert(p).unwrap();
    }
    for id in 1..=30 {
        let q = vector();
        let a = db.search_vector("small", "v", &q, 5, VectorMetric::Cosine, None).unwrap();
        let e = db.search_vector_with("small", "v", &q, 5, VectorMetric::Cosine, None, exact).unwrap();
        assert_eq!(ids(&a), ids(&e));
        assert_eq!(a, e);
        st.delete().where_eq("id", id).run().unwrap();
    }
    assert_eq!(db.vector_indexes("small").unwrap()[0].vectors, 0);
    assert!(db.search_vector("small", "v", &vector(), 5, VectorMetric::Cosine, None).unwrap().is_empty());
    let mut p = Properties::new();
    p.insert("id".into(), 99.into());
    p.insert("v".into(), list(&[1.0, 0.0]));
    st.insert(p).unwrap();
    assert_eq!(db.vector_indexes("small").unwrap()[0].dimensions, Some(2), "an emptied index accepts a new dimension");
    assert_eq!(ids(&db.search_vector("small", "v", &[1.0, 0.1], 1, VectorMetric::Cosine, None).unwrap()), vec![99]);

    // Dropping the column (migration) or the table drops the index data.
    let without = small.to_builder().drop_column("v").build().unwrap();
    db.ensure_table(&without).unwrap();
    assert!(db.vector_indexes("small").unwrap().is_empty());
    assert!(db.drop_table("vecs").unwrap());
    assert!(db.vector_indexes("vecs").unwrap().is_empty());
    db.create_table(&schema).unwrap();
    db.create_vector_index("vecs", "emb", VectorIndexOptions::default()).unwrap();
    assert_eq!(db.vector_indexes("vecs").unwrap()[0].vectors, 0, "no stale nodes");
    assert!(db.search_vector("vecs", "emb", &v42, 3, VectorMetric::Cosine, None).unwrap().is_empty());
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
