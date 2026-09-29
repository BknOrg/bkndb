//! Shared conformance test suite, exercised identically against every
//! `StorageBackend` implementation so all backends satisfy the same contract.
#![cfg(feature = "test-util")]

use std::ops::Bound;

use crate::{StorageBackend, StorageReadTx, StorageWriteTx, TableSpec};

mod graph;
mod relational;
mod db;
mod relational_tx;
mod relational_index;
mod sync;
mod integrity;
mod query;
mod graph_ext;
mod values;
mod search;

pub use graph::*;
pub use relational::*;
pub use db::*;
pub use relational_tx::*;
pub use relational_index::*;
pub use sync::*;
pub use integrity::*;
pub use query::*;
pub use graph_ext::*;
pub use values::*;
pub use search::*;

pub fn conformance_suite<B: StorageBackend>(backend: &B) {
    const T: TableSpec = TableSpec("t");

    // 1. read-after-write-commit round trip
    {
        let mut w = backend.begin_write().unwrap();
        w.put(T, b"k1", b"v1").unwrap();
        w.commit().unwrap();
    }
    let r = backend.begin_read().unwrap();
    assert_eq!(r.get(T, b"k1").unwrap(), Some(b"v1".to_vec()));

    // 2. missing key returns None, not an error, even on a fresh table
    assert_eq!(r.get(T, b"nope").unwrap(), None);

    // 3. delete removes the key
    {
        let mut w = backend.begin_write().unwrap();
        w.delete(T, b"k1").unwrap();
        w.commit().unwrap();
    }
    let r2 = backend.begin_read().unwrap();
    assert_eq!(r2.get(T, b"k1").unwrap(), None);

    // 4. range scan returns keys in sorted order within bounds
    {
        let mut w = backend.begin_write().unwrap();
        for k in [1u8, 2, 3, 4, 5] {
            w.put(T, &[k], &[k * 10]).unwrap();
        }
        w.commit().unwrap();
    }
    let r3 = backend.begin_read().unwrap();
    let got = r3
        .range(T, Bound::Included(&[2u8][..]), Bound::Included(&[4u8][..]))
        .unwrap();
    assert_eq!(
        got,
        vec![
            (vec![2], vec![20]),
            (vec![3], vec![30]),
            (vec![4], vec![40])
        ]
    );

    // 5. streaming scan yields exactly what range returns, can stop early,
    //    and bounds that can't contain anything are empty (never a panic)
    let all = r3.range(T, Bound::Unbounded, Bound::Unbounded).unwrap();
    let streamed: Vec<_> = r3.scan(T, Bound::Unbounded, Bound::Unbounded).unwrap().map(Result::unwrap).collect();
    assert_eq!(streamed, all);
    let first_two: Vec<_> = r3.scan(T, Bound::Excluded(&[1u8][..]), Bound::Unbounded).unwrap().take(2).map(Result::unwrap).collect();
    assert_eq!(first_two, vec![(vec![2], vec![20]), (vec![3], vec![30])]);
    for (lo, hi) in [
        (Bound::Included(&[4u8][..]), Bound::Included(&[2u8][..])),
        (Bound::Excluded(&[3u8][..]), Bound::Excluded(&[3u8][..])),
        (Bound::Included(&[3u8][..]), Bound::Excluded(&[3u8][..])),
    ] {
        assert!(r3.range(T, lo, hi).unwrap().is_empty());
        assert_eq!(r3.scan(T, lo, hi).unwrap().count(), 0);
    }
    assert!(r3.scan(TableSpec("never_written"), Bound::Unbounded, Bound::Unbounded).unwrap().next().is_none());

    // 6. a write transaction's scan sees its own uncommitted puts/deletes
    {
        let mut w = backend.begin_write().unwrap();
        w.delete(T, &[2u8]).unwrap();
        w.put(T, &[6u8], &[60]).unwrap();
        let keys: Vec<u8> = w.scan(T, Bound::Unbounded, Bound::Unbounded).unwrap().map(|kv| kv.unwrap().0[0]).collect();
        assert_eq!(keys, vec![1, 3, 4, 5, 6]);
        let (lo, hi) = (Bound::Included(&[5u8][..]), Bound::Included(&[1u8][..]));
        assert!(w.range(T, lo, hi).unwrap().is_empty());
        assert_eq!(w.scan(T, lo, hi).unwrap().count(), 0);
        // dropped without commit: rolled back
    }

    // 7. a read transaction keeps seeing its snapshot while a writer commits
    let before = backend.begin_read().unwrap();
    {
        let mut w = backend.begin_write().unwrap();
        w.put(T, &[9u8], &[90]).unwrap();
        w.delete(T, &[1u8]).unwrap();
        w.commit().unwrap();
    }
    assert_eq!(before.get(T, &[1u8]).unwrap(), Some(vec![10]));
    assert_eq!(before.get(T, &[9u8]).unwrap(), None);
    assert_eq!(before.scan(T, Bound::Unbounded, Bound::Unbounded).unwrap().count(), 5);
    let after = backend.begin_read().unwrap();
    assert_eq!(after.scan(T, Bound::Unbounded, Bound::Unbounded).unwrap().count(), 5);
    assert_eq!(after.get(T, &[9u8]).unwrap(), Some(vec![90]));
}
