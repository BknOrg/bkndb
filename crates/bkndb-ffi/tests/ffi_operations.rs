use std::collections::HashMap;

use bkndb_ffi::{
    BknDbEngine, FfiBknError, FfiColumn, FfiColumnKind, FfiLsmOptions, FfiPropValue, FfiTableCount, FfiTableSchema,
};

fn items_schema() -> FfiTableSchema {
    FfiTableSchema {
        name: "items".into(),
        columns: vec![
            FfiColumn { name: "id".into(), kind: FfiColumnKind::Int, nullable: true, unique: false, default_value: None },
            FfiColumn { name: "name".into(), kind: FfiColumnKind::Str, nullable: true, unique: false, default_value: None },
        ],
        primary_key: "id".into(),
        auto_increment: true,
        indexed_columns: vec![],
    }
}

fn seed(db: &BknDbEngine, rows: usize) {
    db.create_table(items_schema()).unwrap();
    let rows = (0..rows)
        .map(|i| HashMap::from([("name".to_string(), FfiPropValue::Str(format!("item-{i}")))]))
        .collect();
    db.insert_many("items".into(), rows).unwrap();
    let a = db.create_node("N".into(), HashMap::new()).unwrap();
    let b = db.create_node("N".into(), HashMap::new()).unwrap();
    db.create_edge(a, "E".into(), b, HashMap::new()).unwrap();
}

#[test]
fn stats_backup_and_verify_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let opts = FfiLsmOptions {
        memtable_flush_bytes: 4096,
        compaction_trigger_files: 50,
        block_size_bytes: Some(512),
        compression: Some(true),
    };
    let db = BknDbEngine::open_with_options(dir.path().join("a.bkndb").to_string_lossy().into(), opts).unwrap();
    seed(&db, 300);

    let stats = db.stats().unwrap();
    assert_eq!((stats.nodes, stats.edges), (2, 1));
    assert_eq!(stats.tables, vec![FfiTableCount { table: "items".into(), rows: 300 }]);
    let storage = stats.storage.expect("on-disk databases report storage figures");
    assert!(storage.file_bytes > 0 && storage.sstable_count >= 1);

    let report = db.verify_integrity().unwrap();
    assert!(report.blocks_verified > 0);

    // Backup works while a transaction is open, and doesn't include it.
    let tx = db.begin_transaction().unwrap();
    tx.insert("items".into(), HashMap::from([("name".to_string(), FfiPropValue::Str("pending".into()))])).unwrap();
    let backup = dir.path().join("b.bkndb").to_string_lossy().to_string();
    db.backup(backup.clone()).unwrap();
    tx.commit().unwrap();
    assert!(matches!(db.backup(backup.clone()), Err(FfiBknError::Backend { .. })), "never overwrites");

    let copy = BknDbEngine::open(backup).unwrap();
    let copy_stats = copy.stats().unwrap();
    assert_eq!(copy_stats.tables[0].rows, 300);
    assert_eq!(copy_stats.storage.unwrap().reclaimable_bytes, 0);
    assert_eq!(db.stats().unwrap().tables[0].rows, 301);
}

#[test]
fn in_memory_databases_report_what_applies() {
    let db = BknDbEngine::in_memory().unwrap();
    seed(&db, 3);
    let stats = db.stats().unwrap();
    assert_eq!(stats.tables[0].rows, 3);
    assert!(stats.storage.is_none());
    assert_eq!(db.verify_integrity().unwrap().sstables_checked, 0);
    assert!(matches!(db.backup("x.bkndb".into()), Err(FfiBknError::InvalidArgument { .. })));
    db.close().unwrap();
    assert!(matches!(db.stats(), Err(FfiBknError::DatabaseClosed)));
}
