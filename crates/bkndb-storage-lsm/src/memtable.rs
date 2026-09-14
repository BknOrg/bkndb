use std::collections::BTreeMap;

/// A `BTreeMap`, not a hand-written skip list: `begin_write()` is already
/// single-writer-serialized (see `engine.rs`), so there is no concurrent
/// insert case a lock-free skip list would help with here. `BTreeMap` gets
/// full correctness for free from the standard library.
pub type Memtable = BTreeMap<Vec<u8>, LsmValue>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LsmValue {
    Value(Vec<u8>),
    Tombstone,
}

/// Rough byte size of one memtable, used to decide when to flush. Not
/// exact (ignores `BTreeMap` node overhead), just proportional enough to
/// trigger a flush at a sane point.
pub fn memtable_byte_size(table: &Memtable) -> usize {
    table
        .iter()
        .map(|(k, v)| {
            k.len()
                + match v {
                    LsmValue::Value(bytes) => bytes.len(),
                    LsmValue::Tombstone => 0,
                }
        })
        .sum()
}
