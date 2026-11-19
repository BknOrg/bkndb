use bkndb_engine::{
    Engine, StorageReadTx, StorageWriteTx, TableSpec,
    io::{NativeIo, OpenMode, StorageIo},
};
use std::io::Write;
#[test]
fn read_only_replay_preserves_all_bytes_and_refuses_writes() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("data");
    let candidate = d.path().join("data.compact.tmp");
    {
        let db = Engine::open(&p).unwrap();
        let mut tx = db.begin_write_owned().unwrap();
        tx.put(TableSpec("t"), b"k", b"v").unwrap();
        tx.commit().unwrap();
    }
    std::fs::write(&candidate, b"preserve candidate").unwrap();
    let bytes = std::fs::read(&p).unwrap();
    let db = Engine::open_existing(&p, true).unwrap();
    assert_eq!(
        db.begin_read_owned()
            .unwrap()
            .get(TableSpec("t"), b"k")
            .unwrap(),
        Some(b"v".to_vec())
    );
    assert!(db.begin_write_owned().is_err());
    assert!(db.force_compact().is_err());
    assert!(!db.recovery_required());
    drop(db);
    assert_eq!(std::fs::read(&p).unwrap(), bytes);
    assert_eq!(std::fs::read(candidate).unwrap(), b"preserve candidate");
    let file = NativeIo.open(&p, OpenMode::ReadOnly).unwrap();
    assert!(file.write_all_at(0, b"bad").is_err());
    assert!(file.set_len(0).is_err());
}
#[test]
fn incomplete_tail_is_reported_without_truncation() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("data");
    drop(Engine::open(&p).unwrap());
    std::fs::OpenOptions::new()
        .append(true)
        .open(&p)
        .unwrap()
        .write_all(b"BKW3x")
        .unwrap();
    let bytes = std::fs::read(&p).unwrap();
    let db = Engine::open_existing(&p, true).unwrap();
    assert!(db.recovery_required());
    assert!(db.verify_integrity().is_err());
    drop(db);
    assert_eq!(std::fs::read(&p).unwrap(), bytes);
}
#[test]
fn existing_never_creates_or_initializes() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("data");
    assert!(Engine::open_existing(&p, false).is_err());
    assert!(!p.exists());
    std::fs::write(&p, b"").unwrap();
    assert!(Engine::open_existing(&p, false).is_err());
    assert_eq!(std::fs::metadata(p).unwrap().len(), 0);
}
