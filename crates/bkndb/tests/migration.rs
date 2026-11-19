#![cfg(all(
    feature = "redb-backend",
    feature = "lsm-backend",
    feature = "search",
    feature = "graph"
))]
use bkndb::{Db, LsmStorageBackend, RedbStorageBackend, graph::NodeId, migration};
#[test]
fn redb_lsm_redb_preserves_graph_sql_and_search() {
    let dir = tempfile::tempdir().unwrap();
    let source = Db::new(RedbStorageBackend::open(dir.path().join("source.redb")).unwrap());
    source
        .relational()
        .sql("CREATE TABLE docs (id INT PRIMARY KEY, text TEXT)", ())
        .unwrap();
    source
        .relational()
        .sql("INSERT INTO docs VALUES (1,'hello engine')", ())
        .unwrap();
    source
        .relational()
        .create_fulltext_index("docs", "text")
        .unwrap();
    let node = source
        .graph()
        .create_node("Person", Default::default())
        .unwrap();
    let lsm_path = dir.path().join("converted.lsm");
    migration::redb_to_lsm(source.backend(), &lsm_path).unwrap();
    assert!(migration::redb_to_lsm(source.backend(), &lsm_path).is_err());
    let lsm = Db::new(LsmStorageBackend::open(&lsm_path).unwrap());
    assert_eq!(lsm.graph().get_node(node).unwrap().unwrap().label, "Person");
    assert_eq!(
        lsm.relational()
            .search_text("docs", "text", "hello", 10, false, None)
            .unwrap()
            .len(),
        1
    );
    let redb_path = dir.path().join("converted.redb");
    migration::lsm_to_redb(lsm.backend(), &redb_path).unwrap();
    let redb = Db::new(RedbStorageBackend::open(redb_path).unwrap());
    assert!(redb.graph().get_node(NodeId(node.0)).unwrap().is_some());
    assert_eq!(
        redb.relational()
            .sql("SELECT * FROM docs", ())
            .unwrap()
            .rows
            .len(),
        1
    );
    assert_eq!(
        source
            .relational()
            .sql("SELECT * FROM docs", ())
            .unwrap()
            .rows
            .len(),
        1
    );
}
