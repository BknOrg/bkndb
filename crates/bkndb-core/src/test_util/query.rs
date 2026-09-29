//! The `Expr` query engine, aggregates, and the SQL subset.
use super::*;

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
