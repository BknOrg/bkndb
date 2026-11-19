use bkndb_engine::{Engine, LsmOptions, StorageBackend, StorageReadTx, StorageWriteTx, TableSpec};
use std::{
    collections::BTreeMap,
    ops::Bound,
    time::{Duration, Instant},
};
const T: TableSpec = TableSpec("model");
fn random(seed: &mut u64) -> u64 {
    *seed = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    *seed
}
fn exercise(operations: u64, seconds: u64) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("model.bkndb");
    let options = LsmOptions {
        memtable_flush_bytes: 64 * 1024,
        compaction_trigger_files: 4,
        block_size_bytes: 1024,
        compression: true,
    };
    let mut db = Engine::open_with_options(&path, options.clone()).unwrap();
    let mut model = BTreeMap::new();
    let mut seed = 0x29bf_7c43_b9e1_00d5;
    let start = Instant::now();
    let mut n = 0;
    let mut latencies = Vec::new();
    while n < operations || start.elapsed() < Duration::from_secs(seconds) {
        let mut tx = db.begin_write().unwrap();
        let mut next = model.clone();
        let commit = random(&mut seed) % 7 != 0;
        for _ in 0..1000 {
            let key = (random(&mut seed) % 4096).to_be_bytes().to_vec();
            if random(&mut seed) % 4 == 0 {
                tx.delete(T, &key).unwrap();
                next.remove(&key);
            } else {
                let value = random(&mut seed).to_be_bytes().to_vec();
                tx.put(T, &key, &value).unwrap();
                next.insert(key, value);
            }
            n += 1;
        }
        let now = Instant::now();
        if commit {
            tx.commit().unwrap();
            model = next;
        } else {
            drop(tx);
        }
        latencies.push(now.elapsed().as_micros());
        let actual = db
            .begin_read()
            .unwrap()
            .range(T, Bound::Unbounded, Bound::Unbounded)
            .unwrap();
        assert_eq!(
            actual,
            model
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect::<Vec<_>>(),
            "seed={seed:x} op={n}"
        );
        if n % 17000 == 0 {
            let snap = db.begin_read_owned().unwrap();
            db.force_compact().unwrap();
            assert_eq!(
                snap.range(T, Bound::Unbounded, Bound::Unbounded).unwrap(),
                actual
            );
        }
        if n % 23000 == 0 {
            drop(db);
            db = Engine::open_with_options(&path, options.clone()).unwrap();
        }
        if n % 31000 == 0 {
            let backup = dir.path().join(format!("backup-{n}"));
            db.backup_to(&backup).unwrap();
            let copy = Engine::open(&backup).unwrap();
            assert_eq!(
                copy.begin_read()
                    .unwrap()
                    .range(T, Bound::Unbounded, Bound::Unbounded)
                    .unwrap(),
                actual
            );
            drop(copy);
            std::fs::remove_file(&backup).unwrap();
            std::fs::remove_file(format!("{}.lock", backup.display())).unwrap();
        }
    }
    db.verify_integrity().unwrap();
    latencies.sort_unstable();
    let stats = db.stats().unwrap();
    eprintln!(
        "operations={n} seed=29bf7c43b9e100d5 elapsed={:?} batch_commit_us p50={} p95={} p99={} file_bytes={}",
        start.elapsed(),
        latencies[latencies.len() / 2],
        latencies[latencies.len() * 95 / 100],
        latencies[latencies.len() * 99 / 100],
        stats.file_bytes
    );
}
#[test]
fn deterministic_model() {
    exercise(30_000, 0)
}
#[test]
#[ignore = "stable-candidate gate: run explicitly"]
fn million_operations() {
    exercise(1_000_000, 0)
}
#[test]
#[ignore = "stable-candidate gate: minimum eight-hour soak"]
fn eight_hour_soak() {
    exercise(1_000_000, 8 * 60 * 60)
}
#[test]
fn invalid_user_inputs_return_errors_without_panics() {
    let dir = tempfile::tempdir().unwrap();
    let db = Engine::open(dir.path().join("db")).unwrap();
    let name = "x".repeat(256);
    let mut tx = db.begin_write().unwrap();
    assert!(tx.put(TableSpec(&name), b"key", b"value").is_err());
    assert!(
        db.begin_read()
            .unwrap()
            .get(TableSpec(&name), b"key")
            .is_err()
    );
}
