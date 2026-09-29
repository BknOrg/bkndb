use bkndb_core::test_util::{
    batch_sync_bulk_conformance_suite, db_batch_read_your_own_writes_suite, db_conformance_suite,
    db_read_tx_conformance_suite, db_write_tx_conformance_suite,
};
use bkndb_storage_lsm::LsmStorageBackend;

#[test]
fn lsm_backend_satisfies_db_conformance_suite() {
    let dir = tempfile::tempdir().unwrap();
    let backend = LsmStorageBackend::open(dir.path().join("db_test.bkndb")).unwrap();
    db_conformance_suite(backend);
}

#[test]
fn lsm_backend_satisfies_db_write_tx_conformance_suite() {
    let dir = tempfile::tempdir().unwrap();
    let backend = LsmStorageBackend::open(dir.path().join("db_write_tx_test.bkndb")).unwrap();
    db_write_tx_conformance_suite(backend);
}

#[test]
fn lsm_backend_satisfies_db_read_tx_conformance_suite() {
    let dir = tempfile::tempdir().unwrap();
    let backend = LsmStorageBackend::open(dir.path().join("db_read_tx_test.bkndb")).unwrap();
    db_read_tx_conformance_suite(backend);
}

#[test]
fn lsm_backend_satisfies_db_ryow_conformance_suite() {
    let dir = tempfile::tempdir().unwrap();
    let backend = LsmStorageBackend::open(dir.path().join("db_ryow_test.bkndb")).unwrap();
    db_batch_read_your_own_writes_suite(backend);
}

#[test]
fn lsm_backend_satisfies_batch_sync_bulk_conformance_suite() {
    let dir = tempfile::tempdir().unwrap();
    let backend = LsmStorageBackend::open(dir.path().join("batch_sync_test.bkndb")).unwrap();
    batch_sync_bulk_conformance_suite(backend);
}


#[test]
fn lsm_backend_satisfies_sync_batch_upsert_suite() {
    let dir = tempfile::tempdir().unwrap();
    bkndb_core::test_util::sync_batch_upsert_suite(LsmStorageBackend::open(dir.path().join("sync_upsert.bkndb")).unwrap());
}

#[test]
fn lsm_backend_satisfies_hybrid_query_suite() {
    let dir = tempfile::tempdir().unwrap();
    bkndb_core::test_util::hybrid_query_suite(LsmStorageBackend::open(dir.path().join("hybrid_query.bkndb")).unwrap());
}

#[test]
fn lsm_backend_satisfies_db_stats_suite() {
    let dir = tempfile::tempdir().unwrap();
    bkndb_core::test_util::db_stats_suite(LsmStorageBackend::open(dir.path().join("stats.bkndb")).unwrap());
}

#[test]
fn lsm_backend_satisfies_value_types_suite() {
    let dir = tempfile::tempdir().unwrap();
    bkndb_core::test_util::value_types_suite(bkndb_storage_lsm::LsmStorageBackend::open(dir.path().join("types.bkndb")).unwrap());
}

#[test]
fn lsm_backend_satisfies_sql_suite() {
    let dir = tempfile::tempdir().unwrap();
    bkndb_core::test_util::sql_suite(bkndb_storage_lsm::LsmStorageBackend::open(dir.path().join("sql.bkndb")).unwrap());
}

#[test]
fn lsm_backend_satisfies_graph_query_suite() {
    let dir = tempfile::tempdir().unwrap();
    bkndb_core::test_util::graph_query_suite(bkndb_storage_lsm::LsmStorageBackend::open(dir.path().join("gq.bkndb")).unwrap());
}

#[test]
fn lsm_backend_satisfies_search_suite() {
    let dir = tempfile::tempdir().unwrap();
    bkndb_core::test_util::search_suite(bkndb_storage_lsm::LsmStorageBackend::open(dir.path().join("search.bkndb")).unwrap());
}

#[test]
fn lsm_backend_satisfies_ann_suite() {
    let dir = tempfile::tempdir().unwrap();
    bkndb_core::test_util::ann_suite(bkndb_storage_lsm::LsmStorageBackend::open(dir.path().join("ann.bkndb")).unwrap());
}

#[test]
fn lsm_vector_index_survives_reopen_compaction_and_backup() {
    use bkndb_core::relational::{
        pack_vector, ColumnKind, ColumnSchema, RelationalDb, TableSchema, VectorIndexOptions, VectorMetric,
    };
    use bkndb_core::value::{PropValue, Properties};
    use bkndb_storage_lsm::LsmOptions;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ann.bkndb");
    let options = LsmOptions { memtable_flush_bytes: 16 * 1024, compaction_trigger_files: 4, ..Default::default() };
    let vector = |i: u32| -> Vec<f32> { (0..8).map(|d| ((i * 31 + d * 17) % 97) as f32 / 97.0 - 0.5).collect() };
    let query = vector(1234);
    let search = |db: &RelationalDb<LsmStorageBackend>| {
        let hits = db.search_vector("v", "emb", &query, 10, VectorMetric::Euclidean, None).unwrap();
        hits.into_iter().map(|h| (h.row.pk, h.score)).collect::<Vec<_>>()
    };

    let expected = {
        let db = RelationalDb::new(LsmStorageBackend::open_with_options(&path, options.clone()).unwrap());
        let schema = TableSchema::builder("v")
            .column(ColumnSchema::new("id", ColumnKind::Int))
            .column(ColumnSchema::new("emb", ColumnKind::Bytes))
            .primary_key("id")
            .build()
            .unwrap();
        db.create_table(&schema).unwrap();
        db.create_vector_index("v", "emb", VectorIndexOptions { metric: VectorMetric::Euclidean, m: 8, ef_construction: 32 }).unwrap();
        let t = db.table_named("v").unwrap();
        for i in 0..400u32 {
            let mut p = Properties::new();
            p.insert("id".into(), PropValue::Int(i as i64));
            p.insert("emb".into(), pack_vector(&vector(i)));
            t.insert(p).unwrap();
        }
        t.delete().where_eq("id", 7).run().unwrap();
        let hits = search(&db);
        assert_eq!(hits.len(), 10);
        hits
    };

    let backend = LsmStorageBackend::open_with_options(&path, options).unwrap();
    backend.force_compact().unwrap();
    backend.verify_integrity().unwrap();
    let copy = dir.path().join("copy.bkndb");
    backend.backup_to(&copy).unwrap();
    let db = RelationalDb::new(backend);
    assert_eq!(search(&db), expected, "same results after reopen + compaction");
    assert_eq!(db.vector_indexes("v").unwrap()[0].vectors, 399);
    let restored = RelationalDb::new(LsmStorageBackend::open(&copy).unwrap());
    assert_eq!(search(&restored), expected, "same results from the backup");
}
