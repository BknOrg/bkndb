use serde_json::{Value, json};
use std::{
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};
fn path(p: &Path) -> &str {
    p.to_str().unwrap()
}
fn call(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_bkndb"))
        .args(["--format", "json"])
        .args(args)
        .output()
        .unwrap()
}
fn ok(args: &[&str]) -> Value {
    let r = call(args);
    assert!(
        r.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&r.stderr)
    );
    serde_json::from_slice::<Value>(&r.stdout).unwrap()["data"].take()
}
fn fail(args: &[&str], code: i32) {
    let r = call(args);
    assert_eq!(
        r.status.code(),
        Some(code),
        "{args:?}: {}",
        String::from_utf8_lossy(&r.stderr)
    );
    assert!(!r.stderr.is_empty());
}
fn create(p: &Path, b: &str) {
    ok(&["create", path(p), "--backend", b]);
}
fn sql(p: &Path, q: &str) {
    ok(&["sql", path(p), "--write", "--query", q]);
}
fn setup(p: &Path, b: &str) {
    create(p, b);
    sql(
        p,
        "CREATE TABLE docs (id INT PRIMARY KEY, body TEXT, embedding LIST); INSERT INTO docs VALUES (1,'hello dunia',[1.0,0.0]),(2,'hello rust',[0.0,1.0]);",
    );
}

#[test]
fn command_tree_is_consistent() {
    use clap::CommandFactory;
    bkndb_cli::args::Cli::command().debug_assert();
}
#[test]
fn missing_and_foreign_files_are_never_initialized() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("missing");
    for c in ["info", "stats", "check", "doctor", "compact", "tables"] {
        let r = call(&[c, path(&p)]);
        assert!(!r.status.success());
        assert!(!p.exists());
    }
    for bytes in [b"".as_slice(), b"hello", b"BKNDBLS1bad"] {
        std::fs::write(&p, bytes).unwrap();
        for c in ["info", "check", "doctor", "compact"] {
            assert!(!call(&[c, path(&p)]).status.success());
            assert_eq!(std::fs::read(&p).unwrap(), bytes);
        }
    }
}
#[test]
fn inspection_preserves_source_bytes_on_both_backends() {
    let d = tempfile::tempdir().unwrap();
    for b in ["redb", "lsm"] {
        let p = d.path().join(b);
        setup(&p, b);
        let before = std::fs::read(&p).unwrap();
        for c in [
            "info", "stats", "check", "doctor", "tables", "schema", "indexes",
        ] {
            ok(&[c, path(&p)]);
            assert_eq!(std::fs::read(&p).unwrap(), before, "{b} {c}");
        }
        let r = ok(&["sql", path(&p), "--query", "SELECT * FROM docs ORDER BY id"]);
        assert_eq!(r["rows"].as_array().unwrap().len(), 2);
        assert_eq!(std::fs::read(&p).unwrap(), before);
        fail(
            &[
                "info",
                path(&p),
                "--backend",
                if b == "lsm" { "redb" } else { "lsm" },
            ],
            2,
        );
    }
}
#[test]
fn sql_requires_write_and_atomic_script_rolls_back() {
    let d = tempfile::tempdir().unwrap();
    for b in ["redb", "lsm"] {
        let p = d.path().join(b);
        setup(&p, b);
        let original = std::fs::read(&p).unwrap();
        fail(
            &[
                "sql",
                path(&p),
                "--query",
                "-- harmless comment\n DELETE FROM docs",
            ],
            2,
        );
        assert_eq!(std::fs::read(&p).unwrap(), original);
        fail(
            &[
                "sql",
                path(&p),
                "--write",
                "--query",
                "INSERT INTO docs VALUES (3,'first',[1.0]); INSERT INTO docs VALUES (1,'duplicate',[1.0]);",
            ],
            2,
        );
        assert_eq!(
            ok(&["sql", path(&p), "--query", "SELECT COUNT(*) FROM docs"])["rows"][0][0],
            json!({"Int":2})
        );
        fail(
            &[
                "sql",
                path(&p),
                "--write",
                "--per-statement",
                "--query",
                "INSERT INTO docs VALUES (3,'first',[1.0]); INSERT INTO docs VALUES (1,'duplicate',[1.0]);",
            ],
            10,
        );
        assert_eq!(
            ok(&["sql", path(&p), "--query", "SELECT COUNT(*) FROM docs"])["rows"][0][0],
            json!({"Int":3})
        );
        sql(
            &p,
            "INSERT INTO docs VALUES (4,'semi;colon /* text */',[1.0]); -- ; ignored\n",
        );
    }
}
#[test]
fn backup_restore_migrate_and_destination_safety() {
    let d = tempfile::tempdir().unwrap();
    for b in ["redb", "lsm"] {
        let p = d.path().join(b);
        setup(&p, b);
        let original = std::fs::read(&p).unwrap();
        let backup = d.path().join(format!("{b}.backup"));
        let restore = d.path().join(format!("{b}.restore"));
        let migrated = d.path().join(format!("{b}.migrated"));
        ok(&["backup", path(&p), "--output", path(&backup)]);
        ok(&["restore", path(&backup), "--output", path(&restore)]);
        ok(&[
            "migrate",
            path(&p),
            "--to",
            if b == "lsm" { "redb" } else { "lsm" },
            "--output",
            path(&migrated),
        ]);
        for q in [&backup, &restore, &migrated] {
            ok(&["check", path(q)]);
            assert_eq!(
                ok(&["sql", path(q), "--query", "SELECT COUNT(*) FROM docs"])["rows"][0][0],
                json!({"Int":2})
            );
        }
        fail(&["backup", path(&p), "--output", path(&p)], 2);
        fail(&["create", path(&p)], 2);
        let sentinel = d.path().join(format!("{b}.sentinel"));
        std::fs::write(&sentinel, b"keep me").unwrap();
        fail(
            &[
                "migrate",
                path(&p),
                "--to",
                "lsm",
                "--output",
                path(&sentinel),
            ],
            2,
        );
        assert_eq!(std::fs::read(sentinel).unwrap(), b"keep me");
        assert_eq!(std::fs::read(&p).unwrap(), original);
        ok(&["compact", path(&p)]);
        ok(&["check", path(&p)]);
    }
}
#[test]
fn graph_search_and_dump_roundtrip() {
    let d = tempfile::tempdir().unwrap();
    for b in ["redb", "lsm"] {
        let p = d.path().join(b);
        setup(&p, b);
        for id in [1, 2] {
            let r = ok(&[
                "graph",
                "node",
                "add",
                path(&p),
                "Person",
                "--properties",
                "{\"name\":\"Bkn\",\"nested\":[1,true]}",
                "--write",
            ]);
            assert_eq!(r["id"], id);
        }
        ok(&[
            "graph",
            "edge",
            "add",
            path(&p),
            "1",
            "KNOWS",
            "2",
            "--write",
        ]);
        ok(&[
            "graph",
            "node",
            "update",
            path(&p),
            "1",
            "--properties",
            "{\"updated\":true}",
            "--write",
        ]);
        ok(&[
            "graph",
            "edge",
            "update",
            path(&p),
            "1",
            "--properties",
            "{\"weight\":1}",
            "--write",
        ]);
        assert_eq!(ok(&["graph", "node", "list", path(&p)])[0]["id"], 1);
        assert_eq!(ok(&["graph", "edge", "list", path(&p)])[0]["id"], 1);
        assert_eq!(ok(&["graph", "neighbors", path(&p), "1"])[0][1], 2);
        ok(&["graph", "query", path(&p), "MATCH (n:Person) RETURN n"]);
        for kind in ["text", "vector"] {
            ok(&[
                "index",
                "create",
                path(&p),
                kind,
                "docs",
                if kind == "text" { "body" } else { "embedding" },
                "--write",
            ]);
        }
        ok(&[
            "index",
            "create",
            path(&p),
            "property",
            "Person",
            "name",
            "--write",
        ]);
        assert_eq!(
            ok(&["search", "text", path(&p), "docs", "body", "hello"])
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            ok(&[
                "search",
                "vector",
                path(&p),
                "docs",
                "embedding",
                "[1.0,0.0]",
                "--limit",
                "1"
            ])[0]["pk"],
            json!({"Int":1})
        );
        let dump = p.with_extension("dump");
        let imported = p.with_extension("imported");
        ok(&["export", path(&p), "--output", path(&dump)]);
        ok(&[
            "import",
            path(&imported),
            "--input",
            path(&dump),
            "--backend",
            if b == "lsm" { "redb" } else { "lsm" },
            "--write",
        ]);
        assert_eq!(
            ok(&["graph", "edge", "get", path(&imported), "1"])["edge"]["to"],
            2
        );
        assert_eq!(
            ok(&["search", "text", path(&imported), "docs", "body", "hello"])
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            ok(&[
                "search",
                "vector",
                path(&imported),
                "docs",
                "embedding",
                "[1.0,0.0]",
                "--limit",
                "1"
            ])[0]["pk"],
            json!({"Int":1})
        );
        assert_eq!(
            ok(&["graph", "node", "add", path(&imported), "Person", "--write"])["id"],
            3
        );
        for kind in ["text", "vector"] {
            ok(&[
                "index",
                "drop",
                path(&p),
                kind,
                "docs",
                if kind == "text" { "body" } else { "embedding" },
                "--write",
            ]);
        }
        ok(&["graph", "edge", "delete", path(&p), "1", "--write"]);
        ok(&["graph", "node", "delete", path(&p), "2", "--write"]);
    }
}
#[test]
fn dump_truncation_checksum_failure_and_existing_destination() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("src");
    setup(&p, "lsm");
    let dump = d.path().join("dump");
    ok(&["export", path(&p), "--output", path(&dump)]);
    let bytes = std::fs::read(&dump).unwrap();
    for (i, content) in [
        bytes[..bytes.len() / 2].to_vec(),
        {
            let mut v = bytes.clone();
            let pos = v.windows(6).position(|s| s == b"sha256").unwrap() + 10;
            v[pos] = if v[pos] == b'a' { b'b' } else { b'a' };
            v
        },
        [bytes.clone(), b"garbage".to_vec()].concat(),
    ]
    .iter()
    .enumerate()
    {
        let bad = d.path().join(format!("bad{i}"));
        let dest = d.path().join(format!("dest{i}"));
        std::fs::write(&bad, content).unwrap();
        assert!(
            !call(&["import", path(&dest), "--input", path(&bad), "--write"])
                .status
                .success()
        );
        assert!(!dest.exists());
    }
    fail(&["import", path(&p), "--input", path(&dump), "--write"], 2);
}
#[test]
fn kv_namespaces_are_separate_and_internal_tables_protected() {
    let d = tempfile::tempdir().unwrap();
    for b in ["redb", "lsm"] {
        let p = d.path().join(b);
        create(&p, b);
        fail(&["kv", "put", path(&p), "app", "utf8:key", "utf8:value"], 2);
        ok(&[
            "kv",
            "put",
            path(&p),
            "app",
            "utf8:key",
            "hex:00ff",
            "--write",
        ]);
        assert_eq!(
            ok(&["kv", "get", path(&p), "app", "utf8:key"])["value_hex"],
            "00ff"
        );
        assert!(ok(&["kv", "get", path(&p), "other", "utf8:key"])["value_hex"].is_null());
        assert_eq!(
            ok(&["kv", "scan", path(&p), "app"])
                .as_array()
                .unwrap()
                .len(),
            1
        );
        fail(
            &[
                "kv",
                "put",
                path(&p),
                "app",
                "ambiguous",
                "utf8:value",
                "--write",
            ],
            2,
        );
        fail(&["kv", "get", path(&p), "../meta", "utf8:key"], 2);
        fail(
            &[
                "sql",
                path(&p),
                "--write",
                "--query",
                "CREATE TABLE __bkndb_cli_kv_v1 (id INT PRIMARY KEY)",
            ],
            2,
        );
        ok(&["kv", "delete", path(&p), "app", "utf8:key", "--write"]);
        assert!(ok(&["kv", "get", path(&p), "app", "utf8:key"])["value_hex"].is_null());
    }
}
fn shell(p: &Path, input: &str, write: bool) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_bkndb"));
    c.args(["--format", "json", "shell", path(p)]);
    if write {
        c.arg("--write");
    }
    let mut child = c
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}
#[test]
fn shell_transaction_commit_rollback_and_eof() {
    let d = tempfile::tempdir().unwrap();
    for b in ["redb", "lsm"] {
        let p = d.path().join(b);
        setup(&p, b);
        let r = shell(
            &p,
            "BEGIN; -- comment\nINSERT INTO docs VALUES (3,'pending',[1.0]);\nSELECT COUNT(*) FROM docs;\nROLLBACK;\nBEGIN;\nINSERT INTO docs VALUES (4,'committed',[1.0]);\nCOMMIT;\n.quit\n",
            true,
        );
        assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
        assert_eq!(
            ok(&["sql", path(&p), "--query", "SELECT COUNT(*) FROM docs"])["rows"][0][0],
            json!({"Int":3})
        );
        let r = shell(
            &p,
            "BEGIN;\nINSERT INTO docs VALUES (5,'discarded',[1.0]);\n",
            true,
        );
        assert!(r.status.success());
        assert_eq!(
            ok(&["sql", path(&p), "--query", "SELECT COUNT(*) FROM docs"])["rows"][0][0],
            json!({"Int":3})
        );
        let r = shell(
            &p,
            "BEGIN;\nINSERT INTO docs VALUES (6,'discarded',[1.0]);\nINSERT INTO docs VALUES (4,'duplicate',[1.0]);\nCOMMIT;\n",
            true,
        );
        assert!(!r.status.success());
        let r = shell(&p, "DELETE FROM docs;\n", false);
        assert_eq!(r.status.code(), Some(2));
    }
}
#[test]
fn csv_schema_import_atomic_and_typed_values() {
    let d = tempfile::tempdir().unwrap();
    for b in ["redb", "lsm"] {
        let p = d.path().join(b);
        setup(&p, b);
        sql(
            &p,
            "UPDATE docs SET body = 'line1\nline2,\"quoted\"' WHERE id = 1",
        );
        let csv = p.with_extension("csv");
        ok(&[
            "export",
            path(&p),
            "--table",
            "docs",
            "--output",
            path(&csv),
        ]);
        let q = p.with_extension("target");
        create(&q, b);
        sql(
            &q,
            "CREATE TABLE docs (id INT PRIMARY KEY, body TEXT, embedding LIST)",
        );
        ok(&[
            "import",
            path(&q),
            "--input",
            path(&csv),
            "--backend",
            b,
            "--table",
            "docs",
            "--write",
        ]);
        assert_eq!(
            ok(&["sql", path(&p), "--query", "SELECT * FROM docs ORDER BY id"]),
            ok(&["sql", path(&q), "--query", "SELECT * FROM docs ORDER BY id"])
        );
        assert!(
            !call(&[
                "import",
                path(&q),
                "--input",
                path(&csv),
                "--backend",
                b,
                "--table",
                "docs",
                "--write"
            ])
            .status
            .success()
        );
        assert_eq!(
            ok(&["sql", path(&q), "--query", "SELECT COUNT(*) FROM docs"])["rows"][0][0],
            json!({"Int":2})
        );
    }
}
#[test]
fn native_writers_block_cli() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("lsm");
    create(&p, "lsm");
    let _held = bkndb::LsmStorageBackend::open_existing(&p, false).unwrap();
    fail(&["info", path(&p)], 4);
    let q = d.path().join("redb");
    create(&q, "redb");
    let _held = bkndb::RedbStorageBackend::open_existing(&q).unwrap();
    fail(&["check", path(&q)], 4);
}

#[test]
fn malformed_graph_adjacency_is_reported_without_panic_or_publication() {
    use bkndb::{StorageBackend, StorageWriteTx, TableSpec};
    let d = tempfile::tempdir().unwrap();
    for backend in ["redb", "lsm"] {
        let p = d.path().join(backend);
        create(&p, backend);
        let mut key = 1u64.to_be_bytes().to_vec();
        key.extend_from_slice(&[0, 3, b'x']); // truncated type and missing IDs
        if backend == "redb" {
            let b = bkndb::RedbStorageBackend::open_existing(&p).unwrap();
            let mut tx = b.begin_write().unwrap();
            tx.put(TableSpec("adj_out"), &key, &[]).unwrap();
            tx.commit().unwrap();
        } else {
            let b = bkndb::LsmStorageBackend::open_existing(&p, false).unwrap();
            let mut tx = b.begin_write().unwrap();
            tx.put(TableSpec("adj_out"), &key, &[]).unwrap();
            tx.commit().unwrap();
        }
        let before = std::fs::read(&p).unwrap();
        fail(&["graph", "neighbors", path(&p), "1"], 5);
        fail(&["check", path(&p)], 5);
        let output = p.with_extension("backup");
        fail(&["backup", path(&p), "--output", path(&output)], 5);
        assert!(!output.exists());
        assert_eq!(before, std::fs::read(&p).unwrap());
    }
}

#[test]
fn oversized_edge_type_is_rejected_before_allocating_id() {
    use bkndb::{Db, StorageBackend, value::Properties};
    fn exercise<B: StorageBackend>(backend: B) {
        let db = Db::new(backend);
        let g = db.graph();
        let a = g.create_node("node", Properties::new()).unwrap();
        let label = "x".repeat(65536);
        assert!(g.create_edge(a, &label, a, Properties::new()).is_err());
        assert!(
            g.create_edges_bulk([(a, label.clone(), a, Properties::new())])
                .is_err()
        );
        assert!(g.neighbors_out(a, &label).is_err());
        assert_eq!(
            g.create_edge(a, "valid", a, Properties::new()).unwrap().0,
            1
        );
    }
    let d = tempfile::tempdir().unwrap();
    exercise(bkndb::RedbStorageBackend::open(d.path().join("redb")).unwrap());
    exercise(bkndb::LsmStorageBackend::open(d.path().join("lsm")).unwrap());
}

#[test]
fn checksum_valid_dump_with_broken_graph_is_never_published() {
    use sha2::{Digest, Sha256};
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("source");
    create(&p, "lsm");
    ok(&["graph", "node", "add", path(&p), "N", "--write"]);
    ok(&[
        "graph",
        "edge",
        "add",
        path(&p),
        "1",
        "SELF",
        "1",
        "--write",
    ]);
    let dump = d.path().join("valid.dump");
    ok(&["export", path(&p), "--output", path(&dump)]);
    let records: Vec<Value> = std::fs::read_to_string(&dump)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    for backend in ["lsm", "redb"] {
        for defect in ["adj_out", "adj_in", "counter"] {
            let mut changed = records[..records.len() - 1].to_vec();
            if defect == "counter" {
                let counter = changed
                    .iter_mut()
                    .find(|r| r["table"] == "meta" && r["key"] == hex::encode(b"next_node_id"))
                    .unwrap();
                counter["value"] = json!(hex::encode(1u64.to_be_bytes()));
            } else {
                changed.retain(|r| r["table"] != defect);
            }
            let mut bytes = Vec::new();
            for row in &changed {
                serde_json::to_writer(&mut bytes, row).unwrap();
                bytes.push(b'\n');
            }
            let footer = json!({"end":true,"records":changed.len()-1,
                "sha256":hex::encode(Sha256::digest(&bytes))});
            serde_json::to_writer(&mut bytes, &footer).unwrap();
            bytes.push(b'\n');
            let input = d.path().join(format!("{backend}-{defect}.dump"));
            let output = input.with_extension("database");
            std::fs::write(&input, &bytes).unwrap();
            fail(
                &[
                    "import",
                    path(&output),
                    "--input",
                    path(&input),
                    "--backend",
                    backend,
                    "--write",
                ],
                5,
            );
            assert!(!output.exists());
            assert_eq!(std::fs::read(&input).unwrap(), bytes);
        }
    }
}

#[test]
fn csv_column_limit_is_enforced_before_allocating_unbounded_fields() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("database");
    setup(&p, "lsm");
    let input = d.path().join("wide.csv");
    std::fs::write(&input, ",".repeat(16384)).unwrap();
    fail(
        &[
            "import",
            path(&p),
            "--input",
            path(&input),
            "--backend",
            "lsm",
            "--table",
            "docs",
            "--write",
        ],
        9,
    );
    assert_eq!(
        ok(&["sql", path(&p), "--query", "SELECT COUNT(*) FROM docs"])["rows"][0][0],
        json!({"Int":2})
    );
}

#[test]
fn corrupt_catalog_primary_key_is_an_error_instead_of_a_panic() {
    use bkndb::{
        StorageBackend, StorageWriteTx, TableSpec,
        relational::{ColumnKind, ColumnSchema},
    };
    let d = tempfile::tempdir().unwrap();
    for backend in ["redb", "lsm"] {
        let p = d.path().join(backend);
        setup(&p, backend);
        // TableDef's fixed bincode field order, with a nonexistent primary key.
        let def = (
            "docs",
            vec![ColumnSchema::new("id", ColumnKind::Int)],
            "missing",
            false,
            Vec::<String>::new(),
        );
        let mut bytes = vec![1];
        bytes.extend(bincode::serialize(&def).unwrap());
        if backend == "redb" {
            let b = bkndb::RedbStorageBackend::open_existing(&p).unwrap();
            let mut tx = b.begin_write().unwrap();
            tx.put(TableSpec("meta"), b"relschema:docs", &bytes)
                .unwrap();
            tx.commit().unwrap();
        } else {
            let b = bkndb::LsmStorageBackend::open_existing(&p, false).unwrap();
            let mut tx = b.begin_write().unwrap();
            tx.put(TableSpec("meta"), b"relschema:docs", &bytes)
                .unwrap();
            tx.commit().unwrap();
        }
        let original = std::fs::read(&p).unwrap();
        fail(&["check", path(&p)], 5);
        fail(&["schema", path(&p)], 5);
        assert_eq!(std::fs::read(&p).unwrap(), original);
    }
}

#[test]
fn import_rebuilds_derived_indexes_and_retains_unique_constraints() {
    use sha2::{Digest, Sha256};
    let d = tempfile::tempdir().unwrap();
    let source = d.path().join("source");
    setup(&source, "lsm");
    sql(
        &source,
        "CREATE TABLE users (id INT PRIMARY KEY, email TEXT UNIQUE); INSERT INTO users VALUES (1,'a@example.test')",
    );
    ok(&[
        "graph",
        "node",
        "add",
        path(&source),
        "Person",
        "--properties",
        "{\"name\":\"Ada\"}",
        "--write",
    ]);
    for (kind, table, column) in [
        ("text", "docs", "body"),
        ("vector", "docs", "embedding"),
        ("property", "Person", "name"),
    ] {
        ok(&[
            "index",
            "create",
            path(&source),
            kind,
            table,
            column,
            "--write",
        ]);
    }
    let dump = d.path().join("source.dump");
    ok(&["export", path(&source), "--output", path(&dump)]);
    let records: Vec<Value> = std::fs::read_to_string(&dump)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let mut bytes = Vec::new();
    let mut count = 0;
    for row in &records[..records.len() - 1] {
        if row["table"].as_str().is_some_and(|name| {
            name.contains("__idx_")
                || name.contains("__fts")
                || name.contains("__ann")
                || name == "node_labels"
                || name == "node_props"
        }) {
            continue;
        }
        serde_json::to_writer(&mut bytes, row).unwrap();
        bytes.push(b'\n');
        count += 1;
    }
    let footer = json!({"end":true,"records":count-1,"sha256":hex::encode(Sha256::digest(&bytes))});
    serde_json::to_writer(&mut bytes, &footer).unwrap();
    bytes.push(b'\n');
    std::fs::write(&dump, bytes).unwrap();
    for backend in ["redb", "lsm"] {
        let dest = d.path().join(backend);
        let report = ok(&[
            "import",
            path(&dest),
            "--backend",
            backend,
            "--input",
            path(&dump),
            "--write",
        ]);
        assert_eq!(report["derived_indexes_rebuilt"], true);
        assert_eq!(
            ok(&["graph", "query", path(&dest), "MATCH (n:Person) RETURN n"])["rows"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            ok(&["search", "text", path(&dest), "docs", "body", "hello"])
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            ok(&[
                "search",
                "vector",
                path(&dest),
                "docs",
                "embedding",
                "[1.0,0.0]",
                "--limit",
                "1"
            ])[0]["pk"],
            json!({"Int":1})
        );
        assert_eq!(
            ok(&[
                "sql",
                path(&dest),
                "--query",
                "SELECT id FROM users WHERE email = 'a@example.test'"
            ])["rows"][0][0],
            json!({"Int":1})
        );
        fail(
            &[
                "sql",
                path(&dest),
                "--write",
                "--query",
                "INSERT INTO users VALUES (2,'a@example.test')",
            ],
            2,
        );
        ok(&["check", path(&dest)]);
    }
}
#[test]
fn repair_partial_wal_preserves_source_and_reports_loss() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("src");
    setup(&p, "lsm");
    std::fs::OpenOptions::new()
        .append(true)
        .open(&p)
        .unwrap()
        .write_all(b"BKW3bad")
        .unwrap();
    let bytes = std::fs::read(&p).unwrap();
    assert!(!call(&["check", path(&p)]).status.success());
    assert_eq!(std::fs::read(&p).unwrap(), bytes);
    let dest = d.path().join("repaired");
    fail(&["repair", path(&p), "--output", path(&dest)], 10);
    ok(&["check", path(&dest)]);
    assert_eq!(std::fs::read(&p).unwrap(), bytes);
    assert_eq!(
        ok(&["sql", path(&dest), "--query", "SELECT COUNT(*) FROM docs"])["rows"][0][0],
        json!({"Int":2})
    );
    let redb = d.path().join("redb");
    create(&redb, "redb");
    fail(
        &[
            "repair",
            path(&redb),
            "--output",
            path(&d.path().join("unsupported")),
        ],
        6,
    );
}
#[test]
fn debug_workloads_completions_and_limits() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("db");
    setup(&p, "lsm");
    for c in ["header", "manifest", "wal", "sstable"] {
        ok(&["debug", c, path(&p)]);
    }
    ok(&["capabilities", path(&p)]);
    ok(&["version"]);
    for kind in ["bench", "stress"] {
        let p = d.path().join(kind);
        let r = ok(&[kind, path(&p), "--operations", "1101"]);
        assert_eq!(r["operations"], 1101);
        fail(&[kind, path(&p)], 2);
    }
    for shell in ["bash", "zsh", "fish", "powershell"] {
        let r = Command::new(env!("CARGO_BIN_EXE_bkndb"))
            .args(["completion", shell])
            .output()
            .unwrap();
        assert!(r.status.success());
        assert!(!r.stdout.is_empty());
    }
    fail(
        &["graph", "node", "list", path(&p), "--limit", "1000001"],
        9,
    );
    let invalid = d.path().join("csv-side-effect");
    let r = Command::new(env!("CARGO_BIN_EXE_bkndb"))
        .args(["create", path(&invalid), "--format", "csv"])
        .output()
        .unwrap();
    assert_eq!(r.status.code(), Some(2));
    assert!(!invalid.exists());
}
#[cfg(unix)]
#[test]
fn path_aliases_and_read_only_permissions() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let d = tempfile::tempdir().unwrap();
    for b in ["redb", "lsm"] {
        let p = d.path().join(b);
        setup(&p, b);
        let alias = p.with_extension("alias");
        symlink(&p, &alias).unwrap();
        ok(&["info", path(&alias)]);
        fail(&["backup", path(&p), "--output", path(&alias)], 2);
        let hard = p.with_extension("hard");
        std::fs::hard_link(&p, &hard).unwrap();
        assert!(!call(&["info", path(&hard)]).status.success());
        std::fs::remove_file(hard).unwrap();
        let bytes = std::fs::read(&p).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o400)).unwrap();
        ok(&["check", path(&p)]);
        assert_eq!(std::fs::read(&p).unwrap(), bytes);
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}

#[test]
fn large_graph_ids_and_all_property_types_survive_dump_and_json() {
    use bkndb::{
        Db, StorageBackend, StorageWriteTx, TableSpec,
        graph::NodeId,
        value::{PropValue as P, Properties},
    };
    fn seed<B: StorageBackend>(db: Db<B>) {
        let mut tx = db.backend().begin_write().unwrap();
        tx.put(
            TableSpec("meta"),
            b"next_node_id",
            &9007199254740993u64.to_be_bytes(),
        )
        .unwrap();
        tx.commit().unwrap();
        let mut properties = Properties::new();
        properties.insert("bytes".into(), P::Bytes(vec![0, 255]));
        properties.insert("timestamp".into(), P::Timestamp(i64::MAX));
        properties.insert("uuid".into(), P::Uuid([255; 16]));
        properties.insert(
            "nested".into(),
            P::List(vec![
                P::Map(Properties::from([("null".into(), P::Null)])),
                P::Int(i64::MIN),
            ]),
        );
        properties.insert("nan".into(), P::Float(f64::from_bits(0x7ff8000000000029)));
        assert_eq!(
            db.graph().create_node("Typed", properties).unwrap(),
            NodeId(9007199254740993)
        );
    }
    let d = tempfile::tempdir().unwrap();
    for backend in ["redb", "lsm"] {
        let p = d.path().join(backend);
        create(&p, backend);
        if backend == "redb" {
            seed(Db::new(
                bkndb::RedbStorageBackend::open_existing(&p).unwrap(),
            ));
        } else {
            seed(Db::new(
                bkndb::LsmStorageBackend::open_existing(&p, false).unwrap(),
            ));
        }
        let before = ok(&["graph", "node", "get", path(&p), "9007199254740993"]);
        assert_eq!(before["id"], json!(9007199254740993u64));
        assert_eq!(
            before["node"]["properties"]["nan"]["FloatBits"],
            "7ff8000000000029"
        );
        let dump = p.with_extension("dump");
        let dest = p.with_extension("dest");
        ok(&["export", path(&p), "--output", path(&dump)]);
        ok(&[
            "import",
            path(&dest),
            "--input",
            path(&dump),
            "--write",
            "--backend",
            if backend == "redb" { "lsm" } else { "redb" },
        ]);
        assert_eq!(
            ok(&["graph", "node", "get", path(&dest), "9007199254740993"]),
            before
        );
        assert_eq!(
            ok(&["graph", "node", "add", path(&dest), "Typed", "--write"])["id"],
            json!(9007199254740994u64)
        );
    }
}
#[test]
fn excessive_query_recursion_returns_resource_error() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("lsm");
    setup(&p, "lsm");
    let q = format!("SELECT * FROM docs WHERE {} id=1", "NOT ".repeat(10000));
    fail(&["sql", path(&p), "--query", &q], 9);
}
