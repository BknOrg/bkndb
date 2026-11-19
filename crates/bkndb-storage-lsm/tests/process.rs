use bkndb_core::{StorageBackend, StorageReadTx, StorageWriteTx, TableSpec};
use bkndb_storage_lsm::LsmStorageBackend;
use std::{
    io::{BufRead, BufReader, Write},
    process::{Command, Stdio},
};
const T: TableSpec = TableSpec("test");
#[test]
fn child_process() {
    let Some(path) = std::env::var_os("BKNDB_PROCESS_CHILD") else {
        return;
    };
    let db = LsmStorageBackend::open(path).unwrap();
    let mut tx = db.begin_write().unwrap();
    tx.put(T, b"ack", b"durable").unwrap();
    tx.commit().unwrap();
    db.force_compact().unwrap();
    let mut pending = db.begin_write().unwrap();
    pending.put(T, b"pending", b"uncommitted").unwrap();
    println!("READY");
    std::io::stdout().flush().unwrap();
    let mut input = String::new();
    std::io::stdin().read_line(&mut input).unwrap();
    drop(pending);
}
#[test]
fn process_kill_releases_lock_and_preserves_only_committed_data() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child_process", "--nocapture"])
        .env("BKNDB_PROCESS_CHILD", &path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let reader = BufReader::new(child.stdout.take().unwrap());
    let mut ready = false;
    for line in reader.lines() {
        if line.unwrap() == "READY" {
            ready = true;
            break;
        }
    }
    assert!(ready);
    assert!(LsmStorageBackend::open(&path).is_err());
    child.kill().unwrap();
    child.wait().unwrap();
    let db = LsmStorageBackend::open(&path).unwrap();
    assert_eq!(
        db.begin_read().unwrap().get(T, b"ack").unwrap(),
        Some(b"durable".to_vec())
    );
    assert_eq!(db.begin_read().unwrap().get(T, b"pending").unwrap(), None);
    db.verify_integrity().unwrap();
}

#[path = "support/crash_io.rs"]
mod crash_io;
fn small_options() -> bkndb_storage_lsm::LsmOptions {
    bkndb_storage_lsm::LsmOptions {
        memtable_flush_bytes: 1,
        compaction_trigger_files: 2,
        block_size_bytes: 128,
        compression: true,
    }
}
#[test]
fn crash_at_io_child() {
    use std::sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    };
    let Some(path) = std::env::var_os("BKNDB_CRASH_CHILD") else {
        return;
    };
    let hook = Arc::new(crash_io::CrashIo {
        counter: Arc::new(AtomicU64::new(0)),
        at: std::env::var("BKNDB_CRASH_AT").unwrap().parse().unwrap(),
        before: std::env::var("BKNDB_CRASH_BEFORE").unwrap() == "true",
    });
    let db = LsmStorageBackend::open_with_io(
        path,
        small_options(),
        Arc::new(crash_io::Host(hook.clone())),
    )
    .unwrap();
    let mut tx = db.begin_write().unwrap();
    tx.put(T, b"candidate", b"two").unwrap();
    tx.put(TableSpec("other"), b"candidate", b"three").unwrap();
    tx.commit().unwrap();
    std::fs::write(std::env::var_os("BKNDB_ACK_PATH").unwrap(), b"ACK").unwrap();
    println!("DONE {}", hook.counter.load(Ordering::SeqCst));
}
#[test]
fn kill_before_and_after_each_persistence_operation() {
    for before in [true, false] {
        for at in 1..50 {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("db");
            let ack = dir.path().join("ack");
            {
                let db = LsmStorageBackend::open_with_options(&path, small_options()).unwrap();
                let mut tx = db.begin_write().unwrap();
                tx.put(T, b"baseline", b"one").unwrap();
                tx.commit().unwrap();
            }
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "crash_at_io_child", "--nocapture"])
                .env("BKNDB_CRASH_CHILD", &path)
                .env("BKNDB_ACK_PATH", &ack)
                .env("BKNDB_CRASH_AT", at.to_string())
                .env("BKNDB_CRASH_BEFORE", before.to_string())
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            let reader = BufReader::new(child.stdout.take().unwrap());
            let mut killed = false;
            for line in reader.lines() {
                let line = line.unwrap();
                if line == "KILL" {
                    child.kill().unwrap();
                    killed = true;
                    break;
                }
            }
            let status = child.wait().unwrap();
            if !killed {
                assert!(status.success());
            }
            let db = LsmStorageBackend::open(&path)
                .unwrap_or_else(|e| panic!("{before} event {at}: {e}"));
            let r = db.begin_read().unwrap();
            assert_eq!(r.get(T, b"baseline").unwrap(), Some(b"one".to_vec()));
            let a = r.get(T, b"candidate").unwrap();
            let b = r.get(TableSpec("other"), b"candidate").unwrap();
            assert_eq!(a.is_some(), b.is_some(), "non-atomic {before} {at}");
            if ack.exists() {
                assert!(a.is_some(), "lost ack {before} {at}");
            }
            db.verify_integrity().unwrap();
            // We have exhausted every checkpoint once an event is beyond the whole path.
            if !killed {
                break;
            }
            assert!(at < 49, "increase crash checkpoint bound");
        }
    }
}
