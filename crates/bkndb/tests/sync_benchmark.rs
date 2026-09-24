use std::time::{Duration, Instant};
use bkndb::{BknDb, SyncBatch};
use bkndb::graph::NodeId;
use bkndb::value::{PropValue, Properties};

#[test]
fn test_50k_nodes_and_50k_edges_bulk_sync_benchmark() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bench_50k.bkndb");

    let db = BknDb::open(&db_path).expect("failed to open on-disk database");

    const COUNT: usize = 50_000;

    println!("Generating {} nodes and {} edges in memory...", COUNT, COUNT);
    let mut batch = SyncBatch::new();
    batch.nodes.reserve(COUNT);
    batch.edges.reserve(COUNT);

    for i in 1..=COUNT {
        let mut props = Properties::new();
        props.insert("idx".to_string(), PropValue::Int(i as i64));
        batch.nodes.push(("Symbol".to_string(), props));
    }

    // Connect node i to node i+1 (and wrap around at the end)
    for i in 1..=COUNT {
        let from = NodeId(i as u64);
        let to = if i == COUNT { NodeId(1) } else { NodeId((i + 1) as u64) };
        batch.edges.push((from, "calls".to_string(), to, Properties::new()));
    }

    println!("Starting bulk atomic ingestion of 50k nodes + 50k edges...");
    let start = Instant::now();
    let res = db.sync_batch(batch).expect("bulk sync_batch failed");
    let elapsed = start.elapsed();

    println!(
        "Ingested {} nodes and {} edges in {:?} ({:.2} ops/sec)",
        res.node_ids.len(),
        res.edge_ids.len(),
        elapsed,
        (COUNT * 2) as f64 / elapsed.as_secs_f64()
    );

    assert_eq!(res.node_ids.len(), COUNT);
    assert_eq!(res.edge_ids.len(), COUNT);

    // In debug mode (unoptimized), allow up to 25s for parallel test runs; in release mode (optimized), target < 1000ms.
    let max_duration = if cfg!(debug_assertions) {
        Duration::from_secs(25)
    } else {
        Duration::from_millis(1000)
    };
    assert!(
        elapsed < max_duration,
        "Ingestion took {:?}, exceeding maximum allowable duration {:?}",
        elapsed,
        max_duration
    );

    // Verification: sample random nodes and verify properties and neighbors
    db.read_tx(|tx| {
        for &sample_id in &[1, 25_000, 50_000] {
            let nid = NodeId(sample_id);
            let node = tx.graph().get_node(nid)?.expect("sampled node must exist");
            assert_eq!(node.label, "Symbol");
            assert_eq!(
                node.properties.get("idx"),
                Some(&PropValue::Int(sample_id as i64))
            );

            let neighbors = tx.graph().neighbors_out(nid, "calls")?;
            assert_eq!(neighbors.len(), 1);
            let expected_to = if sample_id == COUNT as u64 { NodeId(1) } else { NodeId(sample_id + 1) };
            assert_eq!(neighbors[0].0, expected_to);
        }
        Ok::<_, bkndb::BknError>(())
    })
    .unwrap();
}
