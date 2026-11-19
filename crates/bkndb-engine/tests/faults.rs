#![cfg(feature = "test-io")]
use bkndb_engine::{
    Engine, Error, LsmOptions, StorageBackend, StorageReadTx, StorageWriteTx, TableSpec,
    testing::{Failure, FaultIo},
};
use std::sync::Arc;
const T: TableSpec = TableSpec("table");
fn opts() -> LsmOptions {
    LsmOptions {
        memtable_flush_bytes: 1,
        compaction_trigger_files: 2,
        block_size_bytes: 128,
        compression: true,
    }
}
#[test]
fn power_loss_at_every_mutation_preserves_acknowledged_transactions() {
    for mode in [Failure::Before, Failure::After, Failure::NoSpace] {
        for at in 1..80 {
            let io = FaultIo::default();
            let db = Engine::open_with_io("db", opts(), Arc::new(io.clone())).unwrap();
            let mut tx = db.begin_write().unwrap();
            tx.put(T, b"acknowledged", b"one").unwrap();
            tx.commit().unwrap();
            io.fail_at(at, mode);
            let mut tx = db.begin_write().unwrap();
            tx.put(T, b"candidate", b"two").unwrap();
            tx.put(TableSpec("other"), b"candidate", b"three").unwrap();
            let outcome = tx.commit();
            let acknowledged = outcome.is_ok();
            if matches!(outcome, Err(Error::CommitUnknown(_))) {
                assert!(matches!(db.begin_write(), Err(Error::RecoveryRequired(_))));
            }
            drop(db);
            io.power_loss();
            let reopened = Engine::open_with_io("db", opts(), Arc::new(io))
                .unwrap_or_else(|e| panic!("failure at {at}, ack={acknowledged}: {e}"));
            let r = reopened.begin_read().unwrap();
            assert_eq!(
                r.get(T, b"acknowledged").unwrap(),
                Some(b"one".to_vec()),
                "at {at}"
            );
            let a = r.get(T, b"candidate").unwrap();
            let b = r.get(TableSpec("other"), b"candidate").unwrap();
            assert_eq!(a.is_some(), b.is_some(), "non-atomic at {at}");
            if acknowledged {
                assert_eq!(a, Some(b"two".to_vec()), "lost acknowledged commit at {at}");
            }
            reopened.verify_integrity().unwrap();
        }
    }
}
#[test]
fn short_writes_are_retried_without_sharing_seek_positions() {
    let io = FaultIo::default();
    io.short_writes(3);
    let db = Engine::open_with_io("db", opts(), Arc::new(io.clone())).unwrap();
    for n in 0u32..50 {
        let mut tx = db.begin_write().unwrap();
        tx.put(T, &n.to_be_bytes(), b"value").unwrap();
        tx.commit().unwrap();
    }
    drop(db);
    io.power_loss();
    let db = Engine::open_with_io("db", opts(), Arc::new(io)).unwrap();
    db.verify_integrity().unwrap();
    for n in 0u32..50 {
        assert_eq!(
            db.begin_read().unwrap().get(T, &n.to_be_bytes()).unwrap(),
            Some(b"value".to_vec())
        );
    }
}
#[test]
fn owned_transactions_retain_writer_lock_after_engine_is_dropped() {
    let io = Arc::new(FaultIo::default());
    let db = Engine::open_with_io("db", opts(), io.clone()).unwrap();
    let mut w = db.begin_write_owned().unwrap();
    drop(db);
    assert!(matches!(
        Engine::open_with_io("db", opts(), io.clone()),
        Err(Error::DatabaseLocked(_))
    ));
    w.put(T, b"key", b"value").unwrap();
    w.commit().unwrap();
    let db = Engine::open_with_io("db", opts(), io).unwrap();
    assert_eq!(
        db.begin_read().unwrap().get(T, b"key").unwrap(),
        Some(b"value".to_vec())
    );
}

#[cfg(feature = "fuzzing")]
#[test]
fn saved_corpus_and_seeded_decoder_smoke_do_not_panic() {
    for kind in 0..6 {
        for len in [0, 1, 8, 64, 128, 1024] {
            bkndb_engine::fuzzing::decode(kind, &vec![0xff; len]);
        }
    }
    let mut seed = 0x7df4b1c834a0e527u64;
    for n in 0..10000 {
        let len = n % 512;
        let mut data = vec![0; len];
        for b in &mut data {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            *b = (seed >> 32) as u8;
        }
        bkndb_engine::fuzzing::decode((n % 6) as u8, &data);
    }
}
