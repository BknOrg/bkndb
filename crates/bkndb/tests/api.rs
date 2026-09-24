use bkndb::BknDb;

#[test]
fn test_bkndb_open_disk_and_transactions() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.bkndb");

    let db = BknDb::open(&db_path).unwrap();

    let node_id = db
        .write_tx(|tx| {
            let n = tx.graph().create_node("Symbol", Default::default())?;
            tx.kv().put(bkndb::TableSpec("app_meta"), b"version", b"1.0")?;
            Ok::<_, bkndb::BknError>(n)
        })
        .unwrap();

    db.read_tx(|tx| {
        let node = tx.graph().get_node(node_id)?.expect("node exists");
        assert_eq!(node.label, "Symbol");

        let ver = tx.kv().get(bkndb::TableSpec("app_meta"), b"version")?.expect("meta exists");
        assert_eq!(ver, b"1.0");

        Ok::<_, bkndb::BknError>(())
    })
    .unwrap();
}

#[test]
fn test_bkndb_in_memory() {
    let db = BknDb::in_memory();

    let node_id = db
        .write_tx(|tx| {
            let n = tx.graph().create_node("MemNode", Default::default())?;
            Ok::<_, bkndb::BknError>(n)
        })
        .unwrap();

    db.read_tx(|tx| {
        let node = tx.graph().get_node(node_id)?.expect("node exists");
        assert_eq!(node.label, "MemNode");
        Ok::<_, bkndb::BknError>(())
    })
    .unwrap();
}

static TEST_FILES_SCHEMA: bkndb::relational::RelSchema = bkndb::relational::RelSchema {
    name: "test_files",
    columns: &[
        bkndb::relational::ColumnDef {
            name: "id",
            kind: bkndb::relational::ColumnKind::Int,
        },
        bkndb::relational::ColumnDef {
            name: "path",
            kind: bkndb::relational::ColumnKind::Str,
        },
    ],
    primary_key: "id",
    auto_increment_pk: false,
    indexed_columns: &["path"],
};

#[test]
fn test_bkndb_hybrid_join_and_prefix_search() {
    let dir = tempfile::tempdir().unwrap();
    let db = BknDb::open(dir.path().join("hybrid_api.bkndb")).unwrap();

    let (f1, f2) = db
        .write_tx(|tx| {
            let n1 = tx.graph().create_node("File", Default::default())?;
            let n2 = tx.graph().create_node("File", Default::default())?;

            let mut r1 = bkndb::value::Properties::new();
            r1.insert("path".to_string(), bkndb::value::PropValue::Str("src/lib.rs".to_string()));
            tx.relational().table(&TEST_FILES_SCHEMA).insert_with_pk(bkndb::value::PropValue::Int(n1.0 as i64), r1)?;

            let mut r2 = bkndb::value::Properties::new();
            r2.insert("path".to_string(), bkndb::value::PropValue::Str("src/main.rs".to_string()));
            tx.relational().table(&TEST_FILES_SCHEMA).insert_with_pk(bkndb::value::PropValue::Int(n2.0 as i64), r2)?;

            Ok::<_, bkndb::BknError>((n1, n2))
        })
        .unwrap();

    // 1. Direct join via BknDb facade method
    let joined = db.join_nodes_with_table(&[f1, f2], &TEST_FILES_SCHEMA).unwrap();
    assert_eq!(joined.len(), 2);
    assert_eq!(joined[0].row.as_ref().unwrap().values.get("path"), Some(&bkndb::value::PropValue::Str("src/lib.rs".to_string())));
    assert_eq!(joined[1].row.as_ref().unwrap().values.get("path"), Some(&bkndb::value::PropValue::Str("src/main.rs".to_string())));

    // 2. Prefix search via table
    db.read_tx(|tx| {
        let prefix_rows = tx.relational().table(&TEST_FILES_SCHEMA).select_prefix("path", "src/m")?;
        assert_eq!(prefix_rows.len(), 1);
        assert_eq!(prefix_rows[0].values.get("path"), Some(&bkndb::value::PropValue::Str("src/main.rs".to_string())));
        Ok::<_, bkndb::BknError>(())
    })
    .unwrap();
}

