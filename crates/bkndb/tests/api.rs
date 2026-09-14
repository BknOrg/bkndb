use bkndb::BknDb;

#[test]
fn test_bkndb_open_disk_and_transactions() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.bkndb");

    let db = BknDb::open(&db_path).unwrap();

    let node_id = db
        .write_tx(|tx| {
            let n = tx.graph().create_node("Symbol", Default::default())?;
            tx.kv().put(bkndb::TableSpec("meta"), b"version", b"1.0")?;
            Ok::<_, bkndb::BknError>(n)
        })
        .unwrap();

    db.read_tx(|tx| {
        let node = tx.graph().get_node(node_id)?.expect("node exists");
        assert_eq!(node.label, "Symbol");

        let ver = tx.kv().get(bkndb::TableSpec("meta"), b"version")?.expect("meta exists");
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
