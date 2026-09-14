//! Table names and binary key layout for the graph layer.
//!
//! Two encodings are at play here and they must not be confused:
//! - **Record values** (`NodeRecord`/`EdgeRecord`) are encoded with `bincode`,
//!   which is little-endian internally. That's fine — values are opaque
//!   blobs, nothing scans them byte-by-byte.
//! - **Keys** must sort, byte-lexicographically, in the same order as their
//!   numeric meaning, because `StorageReadTx::range` does a raw byte-range
//!   scan. That's only true for **big-endian fixed-width** integers: e.g.
//!   `7u64.to_be_bytes() < 100u64.to_be_bytes()` byte-wise, but the
//!   little-endian encodings would *not* compare correctly. So every id in a
//!   key (not in a value) is encoded with `to_be_bytes()`, independent of
//!   whatever `bincode` does for values.

use crate::graph::model::{EdgeId, NodeId};
use crate::TableSpec;

pub const NODES: TableSpec = TableSpec("nodes");
pub const EDGES: TableSpec = TableSpec("edges");
pub const ADJ_OUT: TableSpec = TableSpec("adj_out");
pub const ADJ_IN: TableSpec = TableSpec("adj_in");
pub const META: TableSpec = TableSpec("meta");

pub const NEXT_NODE_ID_KEY: &[u8] = b"next_node_id";
pub const NEXT_EDGE_ID_KEY: &[u8] = b"next_edge_id";

pub fn node_key(id: NodeId) -> [u8; 8] {
    id.0.to_be_bytes()
}

pub fn edge_key(id: EdgeId) -> [u8; 8] {
    id.0.to_be_bytes()
}

/// Exclusive upper bound for "every key starting with this node id's 8-byte
/// prefix, regardless of what follows" — used for cascade-delete scans and
/// prefix (any-edge-type) neighbor/degree scans. `None` only for
/// `id == u64::MAX`, an edge case callers must treat as "scan to the end of
/// the table" (`Bound::Unbounded`).
pub fn next_node_prefix(id: NodeId) -> Option<[u8; 8]> {
    id.0.checked_add(1).map(u64::to_be_bytes)
}

/// Length-prefixed edge_type: 2-byte BE length + UTF-8 bytes. Without the
/// length prefix, edge_type `"cal"` would be a byte-prefix of `"calls"`, and
/// a range scan bounded only by the `"cal"` prefix would incorrectly include
/// `"calls"` edges too. Comparing the length first (3 != 5) disambiguates
/// them regardless of shared leading bytes.
fn encode_edge_type(t: &str) -> Vec<u8> {
    let bytes = t.as_bytes();
    debug_assert!(bytes.len() <= u16::MAX as usize, "edge_type too long");
    let mut out = Vec::with_capacity(2 + bytes.len());
    out.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
    out.extend_from_slice(bytes);
    out
}

/// adj_out key = from(8) ++ len(2) ++ edge_type(len) ++ to(8) ++ edge_id(8).
/// Value is empty; edge_id (and hence the canonical `edges` record) is
/// already recoverable from the key tail.
///
/// Worked example: node 42 --calls--> node 7 (edge_id 1001) and
/// node 42 --calls--> node 100 (edge_id 1002) produce
/// `be(42)++be16(5)++"calls"++be(7)++be(1001)` and
/// `be(42)++be16(5)++"calls"++be(100)++be(1002)`, which sort with the first
/// before the second because `be(7) < be(100)` byte-wise (fixed-width BE
/// makes numeric 7 < 100 hold as a byte comparison too, unlike variable
/// -length decimal text).
pub fn adj_out_key(from: NodeId, edge_type: &str, to: NodeId, edge: EdgeId) -> Vec<u8> {
    encode_adj_key(from, edge_type, to, edge)
}

pub fn adj_in_key(to: NodeId, edge_type: &str, from: NodeId, edge: EdgeId) -> Vec<u8> {
    encode_adj_key(to, edge_type, from, edge)
}

fn encode_adj_key(first: NodeId, edge_type: &str, second: NodeId, edge: EdgeId) -> Vec<u8> {
    let mut k = Vec::with_capacity(8 + 2 + edge_type.len() + 8 + 8);
    k.extend_from_slice(&first.0.to_be_bytes());
    k.extend_from_slice(&encode_edge_type(edge_type));
    k.extend_from_slice(&second.0.to_be_bytes());
    k.extend_from_slice(&edge.0.to_be_bytes());
    k
}

/// Inclusive start bound for "first=X, edge_type=Y, any second/edge_id" — a
/// strict byte-prefix of any full key with that (first, edge_type), and a
/// strict prefix always sorts as `Less` than anything it prefixes, so it is
/// a valid inclusive lower bound with no padding needed.
pub fn adj_type_prefix(first: NodeId, edge_type: &str) -> Vec<u8> {
    let mut k = Vec::with_capacity(8 + 2 + edge_type.len());
    k.extend_from_slice(&first.0.to_be_bytes());
    k.extend_from_slice(&encode_edge_type(edge_type));
    k
}

/// Inclusive upper bound for the same type-filtered scan. The suffix after
/// the type prefix is always exactly 16 bytes (second:8 + edge_id:8), so
/// appending 16 bytes of `0xFF` is a safe, overflow-free upper bound with no
/// last-byte-increment edge cases to worry about.
pub fn adj_type_upper_bound(first: NodeId, edge_type: &str) -> Vec<u8> {
    let mut k = adj_type_prefix(first, edge_type);
    k.extend_from_slice(&[0xFF; 16]);
    k
}

/// Decodes an adj_out or adj_in key back into its four physical fields:
/// (first_id, edge_type, second_id, edge_id). Field names are physical, not
/// semantic — callers know from which table they read whether `first` means
/// `from` or `to`.
pub fn decode_adj_key(key: &[u8]) -> (u64, String, u64, EdgeId) {
    let first = u64::from_be_bytes(key[0..8].try_into().unwrap());
    let len = u16::from_be_bytes(key[8..10].try_into().unwrap()) as usize;
    let edge_type = String::from_utf8(key[10..10 + len].to_vec()).expect("edge_type is valid utf8");
    let off = 10 + len;
    let second = u64::from_be_bytes(key[off..off + 8].try_into().unwrap());
    let edge_id = u64::from_be_bytes(key[off + 8..off + 16].try_into().unwrap());
    (first, edge_type, second, EdgeId(edge_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adj_key_roundtrip() {
        let key = adj_out_key(NodeId(42), "calls", NodeId(7), EdgeId(1001));
        let (first, edge_type, second, edge_id) = decode_adj_key(&key);
        assert_eq!(first, 42);
        assert_eq!(edge_type, "calls");
        assert_eq!(second, 7);
        assert_eq!(edge_id, EdgeId(1001));
    }

    #[test]
    fn adj_key_sorts_by_second_id_numerically() {
        let key1 = adj_out_key(NodeId(42), "calls", NodeId(7), EdgeId(1001));
        let key2 = adj_out_key(NodeId(42), "calls", NodeId(100), EdgeId(1002));
        assert!(key1 < key2, "be(7) must sort before be(100) byte-wise");
    }

    #[test]
    fn edge_type_length_prefix_prevents_prefix_collision() {
        // "cal" must not be treated as a prefix match for "calls" entries.
        let cal_key = adj_out_key(NodeId(42), "cal", NodeId(1), EdgeId(1));
        let start = adj_type_prefix(NodeId(42), "calls");
        let end = adj_type_upper_bound(NodeId(42), "calls");
        assert!(
            !(start.as_slice()..=end.as_slice()).contains(&cal_key.as_slice()),
            "\"cal\"-typed edge must not fall inside a \"calls\"-filtered range"
        );
    }

    #[test]
    fn type_filtered_range_bounds_include_only_matching_type() {
        let calls_key1 = adj_out_key(NodeId(42), "calls", NodeId(7), EdgeId(1001));
        let calls_key2 = adj_out_key(NodeId(42), "calls", NodeId(100), EdgeId(1002));
        let imports_key = adj_out_key(NodeId(42), "imports", NodeId(5), EdgeId(2000));

        let start = adj_type_prefix(NodeId(42), "calls");
        let end = adj_type_upper_bound(NodeId(42), "calls");
        let range = start.as_slice()..=end.as_slice();

        assert!(range.contains(&calls_key1.as_slice()));
        assert!(range.contains(&calls_key2.as_slice()));
        assert!(!range.contains(&imports_key.as_slice()));
    }

    #[test]
    fn next_node_prefix_handles_max_id() {
        assert_eq!(next_node_prefix(NodeId(u64::MAX)), None);
        assert_eq!(next_node_prefix(NodeId(41)), Some(42u64.to_be_bytes()));
    }
}
