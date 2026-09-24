use bkndb_core::db::{Db, SyncBatch};
use bkndb_core::graph::{Direction, NodeId};
use bkndb_core::value::{PropValue, Properties};
use bkndb_storage_lsm::LsmStorageBackend;
use bkndb_storage_redb::RedbStorageBackend;
use std::time::Instant;

#[test]
fn compare_redb_vs_lsm_performance() {
    const NODES: usize = 50_000;
    const EDGES: usize = 50_000;

    println!("\n=== PERFORMANCE COMPARISON: REDB vs LSM ({NODES} nodes + {EDGES} edges) ===");

    // 1. Benchmark REDB
    let dir_redb = tempfile::tempdir().unwrap();
    let redb_backend = RedbStorageBackend::open(dir_redb.path().join("redb.bkndb")).unwrap();
    let db_redb = Db::new(redb_backend);

    let mut batch_redb = SyncBatch::new();
    for i in 1..=NODES {
        let mut p = Properties::new();
        p.insert("id".to_string(), PropValue::Int(i as i64));
        batch_redb.nodes.push(("Node".to_string(), p));
    }
    for i in 1..=EDGES {
        let to = if i == EDGES {
            NodeId(1)
        } else {
            NodeId((i + 1) as u64)
        };
        batch_redb
            .edges
            .push((NodeId(i as u64), "next".to_string(), to, Properties::new()));
    }

    let t0 = Instant::now();
    db_redb.sync_batch(batch_redb).unwrap();
    let redb_write_time = t0.elapsed();

    let t1 = Instant::now();
    let redb_path = db_redb
        .graph()
        .find_shortest_path(NodeId(1), NodeId(5_000), Direction::Out, Some(&["next"]))
        .unwrap()
        .unwrap();
    let redb_bfs_time = t1.elapsed();

    // 2. Benchmark LSM
    let dir_lsm = tempfile::tempdir().unwrap();
    let lsm_backend = LsmStorageBackend::open(dir_lsm.path().join("lsm.bkndb")).unwrap();
    let db_lsm = Db::new(lsm_backend);

    let mut batch_lsm = SyncBatch::new();
    for i in 1..=NODES {
        let mut p = Properties::new();
        p.insert("id".to_string(), PropValue::Int(i as i64));
        batch_lsm.nodes.push(("Node".to_string(), p));
    }
    for i in 1..=EDGES {
        let to = if i == EDGES {
            NodeId(1)
        } else {
            NodeId((i + 1) as u64)
        };
        batch_lsm
            .edges
            .push((NodeId(i as u64), "next".to_string(), to, Properties::new()));
    }

    let t2 = Instant::now();
    db_lsm.sync_batch(batch_lsm).unwrap();
    let lsm_write_time = t2.elapsed();

    let t3 = Instant::now();
    let lsm_path = db_lsm
        .graph()
        .find_shortest_path(NodeId(1), NodeId(5_000), Direction::Out, Some(&["next"]))
        .unwrap()
        .unwrap();
    let lsm_bfs_time = t3.elapsed();

    println!("REDB Write (10k+10k) : {:?}", redb_write_time);
    println!("LSM  Write (10k+10k) : {:?}", lsm_write_time);
    println!("REDB BFS (5,000 hops): {:?}", redb_bfs_time);
    println!("LSM  BFS (5,000 hops): {:?}", lsm_bfs_time);

    assert_eq!(redb_path.steps.len(), lsm_path.steps.len());
}
