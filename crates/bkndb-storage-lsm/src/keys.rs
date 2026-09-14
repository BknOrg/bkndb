//! Maps the `StorageBackend` trait's per-table K/V model onto one shared,
//! flat LSM keyspace: `lsm_key = table_name_len:u8 ++ table_name ++ user_key`.
//!
//! A shared keyspace (rather than one LSM tree per `TableSpec`) is what
//! lets one `commit()` spanning several tables (as `GraphDb::create_edge`
//! already does, writing `edges` + `adj_out` + `adj_in` together) become a
//! single WAL frame and a single memtable batch — trivially atomic, with no
//! per-table two-phase commit needed. Because the table name is the
//! *leading* component of every key, keys still sort with every table's
//! entries grouped contiguously, so range-scan locality isn't lost either.
use bkndb_core::TableSpec;

pub fn encode_key(table: TableSpec, user_key: &[u8]) -> Vec<u8> {
    let name = table.0.as_bytes();
    debug_assert!(name.len() <= u8::MAX as usize, "table name too long");
    let mut out = Vec::with_capacity(1 + name.len() + user_key.len());
    out.push(name.len() as u8);
    out.extend_from_slice(name);
    out.extend_from_slice(user_key);
    out
}

/// Strict prefix of every key belonging to `table` — the inclusive lower
/// bound for an unbounded ("whole table") scan.
pub fn table_prefix(table: TableSpec) -> Vec<u8> {
    let name = table.0.as_bytes();
    let mut out = Vec::with_capacity(1 + name.len());
    out.push(name.len() as u8);
    out.extend_from_slice(name);
    out
}

/// Strips a table's key prefix back off an LSM key, returning the original
/// user-key bytes. Panics if `key` doesn't actually start with `table`'s
/// prefix — a programming error (this module's own encoding invariant),
/// not a data-dependent failure.
pub fn strip_table_prefix(table: TableSpec, key: &[u8]) -> Vec<u8> {
    let prefix = table_prefix(table);
    assert!(
        key.starts_with(&prefix),
        "lsm key does not start with the expected table prefix"
    );
    key[prefix.len()..].to_vec()
}

/// Generic byte-string successor: increments the last byte that is `< 0xFF`,
/// truncating everything after it. Turns a prefix into an exclusive upper
/// bound for "every key starting with this prefix". `None` only if `prefix`
/// is entirely `0xFF` bytes — never happens for a real table-name prefix
/// (the leading length byte alone makes this practically unreachable).
pub fn prefix_upper_bound(prefix: &[u8]) -> Option<Vec<u8>> {
    let mut out = prefix.to_vec();
    while let Some(last) = out.pop() {
        if last != 0xFF {
            out.push(last + 1);
            return Some(out);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn different_tables_do_not_overlap_in_the_shared_keyspace() {
        const A: TableSpec = TableSpec("a");
        const AB: TableSpec = TableSpec("ab");
        // "a" with user_key starting with "b..." must not collide with
        // table "ab" — the length-prefix byte disambiguates them.
        let k1 = encode_key(A, b"bxyz");
        let k2 = encode_key(AB, b"xyz");
        assert_ne!(k1, k2);
    }

    #[test]
    fn table_scan_roundtrip() {
        const T: TableSpec = TableSpec("nodes");
        let k = encode_key(T, b"hello");
        assert!(k.starts_with(&table_prefix(T)));
        assert_eq!(strip_table_prefix(T, &k), b"hello".to_vec());
    }

    #[test]
    fn table_prefix_upper_bound_excludes_other_tables() {
        const NODES: TableSpec = TableSpec("nodes");
        const NODES2: TableSpec = TableSpec("nodesx");
        let prefix = table_prefix(NODES);
        let upper = prefix_upper_bound(&prefix).unwrap();
        let other = encode_key(NODES2, b"k");
        assert!(other.as_slice() >= upper.as_slice(), "a differently-named table must sort at/after the upper bound");
    }
}
