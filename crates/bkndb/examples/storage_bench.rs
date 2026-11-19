//! Repeatable smoke benchmark. Run one backend per process under /usr/bin/time -v
//! for peak RSS. Batch latency includes durability sync; scan/query workloads are
//! separate from this raw KV workload. No stability/performance claim is implied.
use bkndb::{StorageBackend, StorageReadTx, StorageWriteTx, TableSpec};
use std::{path::Path, time::Instant};
const T: TableSpec = TableSpec("bench");
fn quantile(v: &mut [u128], p: usize) -> u128 {
    v.sort_unstable();
    v[(v.len() - 1) * p / 100]
}
fn bench<B: StorageBackend>(backend: &B, path: &Path, operations: usize) {
    let mut commits = Vec::new();
    let mut reads = Vec::new();
    let start = Instant::now();
    let mut peak = 0;
    for batch in 0..operations.div_ceil(100) {
        let mut tx = backend.begin_write().unwrap();
        let value = vec![b'x'; 256];
        for i in 0..(operations - batch * 100).min(100) {
            let key = ((batch * 100 + i) % 10000).to_be_bytes();
            tx.put(T, &key, &value).unwrap();
        }
        let t = Instant::now();
        tx.commit().unwrap();
        commits.push(t.elapsed().as_micros());
        let r = backend.begin_read().unwrap();
        let t = Instant::now();
        assert!(
            r.get(T, &((batch * 100) % 10000).to_be_bytes())
                .unwrap()
                .is_some()
        );
        reads.push(t.elapsed().as_micros());
        peak = peak.max(std::fs::metadata(path).unwrap().len());
    }
    println!(
        "operations={} seconds={:.3} ops_per_second={:.0} batch_commit_us_p50={} p95={} p99={} point_read_us_p50={} p95={} p99={} file_bytes={} sampled_peak_file_bytes={}",
        operations,
        start.elapsed().as_secs_f64(),
        operations as f64 / start.elapsed().as_secs_f64(),
        quantile(&mut commits, 50),
        quantile(&mut commits, 95),
        quantile(&mut commits, 99),
        quantile(&mut reads, 50),
        quantile(&mut reads, 95),
        quantile(&mut reads, 99),
        std::fs::metadata(path).unwrap().len(),
        peak
    );
}
fn main() {
    let args: Vec<_> = std::env::args().collect();
    let operations = args.get(3).map(|s| s.parse().unwrap()).unwrap_or(100000);
    assert!(operations > 0);
    let path = Path::new(&args[2]);
    assert!(!path.exists(), "benchmark needs a new file");
    match args[1].as_str() {
        #[cfg(feature = "lsm-backend")]
        "lsm" => {
            let db = bkndb::LsmStorageBackend::open(path).unwrap();
            bench(&db, path, operations);
            let start = Instant::now();
            db.force_compact().unwrap();
            println!(
                "final_compaction_us={} peak_compaction_temporary_bytes={} peak_current_plus_temporary_bytes={}",
                start.elapsed().as_micros(),
                db.peak_compaction_temporary_bytes(),
                db.peak_compaction_disk_bytes()
            );
        }
        #[cfg(feature = "redb-backend")]
        "redb" => bench(
            &bkndb::RedbStorageBackend::open(path).unwrap(),
            path,
            operations,
        ),
        _ => panic!("enable the backend feature and pass lsm|redb path [operations]"),
    }
}
