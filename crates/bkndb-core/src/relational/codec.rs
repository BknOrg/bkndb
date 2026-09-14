use std::collections::HashMap;
use std::ops::Bound;
use std::sync::{Mutex, OnceLock};

use crate::relational::schema::{ColumnKind, RelSchema};
use crate::value::{PropValue, Properties};
use crate::{BknError, StorageWriteTx, TableSpec};

/// Shared meta/counter table. Same physical table name as the graph layer's
/// own `META` (`crates/bkndb-core/src/graph/codec.rs`) — intentional: both
/// layers can coexist over one backend without a name collision because
/// counter keys are namespaced (`next_node_id`/`next_edge_id` vs
/// `relnext:<table>`), so sharing one physical table just avoids growing
/// the table count instead of causing conflicts.
const META: TableSpec = TableSpec("meta");

pub fn base_table(schema: &RelSchema) -> TableSpec {
    TableSpec(schema.name)
}

/// Computes (and caches) the physical `TableSpec` backing one column's
/// secondary index. `TableSpec` requires a `&'static str`, and the index
/// table's name is only known at schema-construction time (it's derived
/// from `schema.name` + the column name), so it must be leaked once to get
/// a `'static` lifetime. Caching by name ensures each distinct
/// (schema, column) pair is leaked at most once, no matter how many times
/// queries call this — a deliberate, bounded leak, not an unbounded one.
pub fn index_table(schema: &RelSchema, column: &str) -> TableSpec {
    static CACHE: OnceLock<Mutex<HashMap<String, &'static str>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = format!("{}__idx_{}", schema.name, column);

    let mut guard = cache.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(&name) = guard.get(&key) {
        return TableSpec(name);
    }
    let leaked: &'static str = Box::leak(key.clone().into_boxed_str());
    guard.insert(key, leaked);
    TableSpec(leaked)
}

/// Sortable ("memcomparable") byte encoding for a scalar value used as a
/// primary key or index key. Only `Int`/`Str` are supported as key material.
///
/// - `Int`: sign-bit flip then big-endian bytes. Plain `to_be_bytes()` of a
///   signed integer does NOT sort negatives correctly byte-lexicographically
///   (a negative `i64`'s top bit is 1, so it would sort *after* every
///   positive value); flipping the sign bit first fixes this, the standard
///   trick for memcomparable signed-integer encodings.
/// - `Str`: raw UTF-8 bytes plus a single trailing `0x00` terminator, so a
///   string that is a strict prefix of another still compares correctly
///   (`"ab\0" < "abc\0"` because the third byte `0x00 < b'c'`). This assumes
///   values never contain an embedded NUL byte — a real constraint of this
///   scheme, acceptable for this project's use (paths, identifiers, names).
pub fn sortable_encode(value: &PropValue) -> Result<Vec<u8>, BknError> {
    match value {
        PropValue::Int(i) => {
            let flipped = (*i as u64) ^ 0x8000_0000_0000_0000;
            Ok(flipped.to_be_bytes().to_vec())
        }
        PropValue::Str(s) => {
            let mut out = s.as_bytes().to_vec();
            out.push(0);
            Ok(out)
        }
        _ => Err(BknError::Encoding(
            "only Int/Str values can be used as a primary key or indexed column".to_string(),
        )),
    }
}

pub fn decode_sortable(kind: ColumnKind, bytes: &[u8]) -> Result<PropValue, BknError> {
    match kind {
        ColumnKind::Int => {
            let raw: [u8; 8] = bytes
                .try_into()
                .map_err(|_| BknError::Encoding("corrupt sortable int key".to_string()))?;
            let flipped = u64::from_be_bytes(raw);
            Ok(PropValue::Int((flipped ^ 0x8000_0000_0000_0000) as i64))
        }
        ColumnKind::Str => {
            let (last, rest) = bytes
                .split_last()
                .ok_or_else(|| BknError::Encoding("empty sortable str key".to_string()))?;
            if *last != 0 {
                return Err(BknError::Encoding(
                    "sortable str key missing terminator".to_string(),
                ));
            }
            let s = String::from_utf8(rest.to_vec()).map_err(|e| BknError::Encoding(e.to_string()))?;
            Ok(PropValue::Str(s))
        }
        _ => Err(BknError::Encoding(
            "only Int/Str columns support sortable decoding".to_string(),
        )),
    }
}

/// index_key = sortable(indexed_value) ++ sortable(pk) — appending the pk
/// disambiguates multiple rows sharing the same indexed value, the same
/// pattern `adj_out_key` uses to append an edge id after a shared
/// (from, edge_type) prefix.
pub fn index_key(indexed_value: &PropValue, pk: &PropValue) -> Result<Vec<u8>, BknError> {
    let mut k = sortable_encode(indexed_value)?;
    k.extend(sortable_encode(pk)?);
    Ok(k)
}

/// Exclusive upper bound for "every key starting with exactly this byte
/// prefix" — a generic byte-string successor: increments the last byte
/// that is < 0xFF, truncating anything after it. `None` only if `prefix`
/// is entirely `0xFF` bytes, which never happens for real encoded values.
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

fn row_counter_key(schema: &RelSchema) -> Vec<u8> {
    format!("relnext:{}", schema.name).into_bytes()
}

/// Allocates the next auto-increment primary key for `schema`, mirroring
/// the graph layer's `next_id` counter pattern (`graph/db.rs`) — a
/// persisted counter in the shared meta table, read-modify-write inside
/// the caller's write tx so allocation and the row insert commit together.
pub fn next_pk<W: StorageWriteTx>(wtx: &mut W, schema: &RelSchema) -> Result<i64, BknError> {
    let key = row_counter_key(schema);
    let current = match wtx.get(META, &key)? {
        Some(bytes) => u64::from_be_bytes(
            bytes
                .as_slice()
                .try_into()
                .map_err(|_| BknError::Encoding("corrupt relational id counter".to_string()))?,
        ),
        None => 1,
    };
    wtx.put(META, &key, &(current + 1).to_be_bytes())?;
    Ok(current as i64)
}

/// The byte length of an indexed value's encoding at the start of one of
/// its index keys — `Int` is always fixed-width (8 bytes); `Str` is found
/// by scanning for its `0x00` terminator (see [`sortable_encode`]'s doc
/// comment for the constraint this relies on). Used to split an index key
/// (`value_bytes ++ pk_bytes`) back into its two parts during a range scan,
/// where (unlike an exact-match lookup) the caller doesn't already know the
/// queried value's exact encoded length up front.
fn indexed_value_len(kind: ColumnKind, key: &[u8]) -> Result<usize, BknError> {
    match kind {
        ColumnKind::Int => Ok(8),
        ColumnKind::Str => key
            .iter()
            .position(|&b| b == 0)
            .map(|pos| pos + 1)
            .ok_or_else(|| BknError::Encoding("index key missing str terminator".to_string())),
        _ => Err(BknError::Encoding(
            "only Int/Str columns support indexing".to_string(),
        )),
    }
}

/// Splits a full index key (`value_bytes ++ pk_bytes`) into the pk-only
/// suffix, decoded via `pk_kind`.
pub fn pk_from_index_key(indexed_kind: ColumnKind, pk_kind: ColumnKind, key: &[u8]) -> Result<PropValue, BknError> {
    let split = indexed_value_len(indexed_kind, key)?;
    decode_sortable(pk_kind, &key[split..])
}

/// Converts a value-level range bound into a byte-level bound over index
/// keys (which are `value_bytes ++ pk_bytes`, always longer than the bare
/// encoded value). A bare `Included(value_bytes)` already works correctly
/// as a byte lower bound (any extension of it sorts after it), but
/// `Excluded(value)` must skip past every possible pk-suffixed extension of
/// `value`'s bytes, not just the bare bytes themselves — hence
/// `prefix_upper_bound`. The `None` case (bytes are all `0xFF`) is a
/// vanishingly rare edge case for real column values; falling back to
/// `Unbounded` slightly over-includes there rather than under-including.
pub fn lower_bound_bytes(bound: &Bound<PropValue>) -> Result<Bound<Vec<u8>>, BknError> {
    match bound {
        Bound::Unbounded => Ok(Bound::Unbounded),
        Bound::Included(v) => Ok(Bound::Included(sortable_encode(v)?)),
        Bound::Excluded(v) => {
            let enc = sortable_encode(v)?;
            Ok(match prefix_upper_bound(&enc) {
                Some(b) => Bound::Included(b),
                None => Bound::Unbounded,
            })
        }
    }
}

/// Upper-bound counterpart of [`lower_bound_bytes`]. `Excluded(value)` works
/// as a bare byte bound directly (every pk-suffixed extension of `value`'s
/// bytes sorts after the bare bytes, so it's naturally excluded already);
/// `Included(value)` must include every such extension, hence
/// `prefix_upper_bound` as the exclusive cutoff.
pub fn upper_bound_bytes(bound: &Bound<PropValue>) -> Result<Bound<Vec<u8>>, BknError> {
    match bound {
        Bound::Unbounded => Ok(Bound::Unbounded),
        Bound::Excluded(v) => Ok(Bound::Excluded(sortable_encode(v)?)),
        Bound::Included(v) => {
            let enc = sortable_encode(v)?;
            Ok(match prefix_upper_bound(&enc) {
                Some(b) => Bound::Excluded(b),
                None => Bound::Unbounded,
            })
        }
    }
}

pub fn encode_row(properties: &Properties) -> Result<Vec<u8>, BknError> {
    bincode::serialize(properties).map_err(|e| BknError::Encoding(e.to_string()))
}

pub fn decode_row(bytes: &[u8]) -> Result<Properties, BknError> {
    bincode::deserialize(bytes).map_err(|e| BknError::Encoding(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn int_sortable_encoding_preserves_numeric_order() {
        let mut values = vec![-100i64, -1, 0, 1, 100, i64::MIN, i64::MAX];
        let mut encoded: Vec<(i64, Vec<u8>)> = values
            .iter()
            .map(|&v| (v, sortable_encode(&PropValue::Int(v)).unwrap()))
            .collect();
        encoded.sort_by(|a, b| a.1.cmp(&b.1));
        values.sort();
        let sorted_values: Vec<i64> = encoded.into_iter().map(|(v, _)| v).collect();
        assert_eq!(sorted_values, values);
    }

    #[test]
    fn int_sortable_roundtrip() {
        for v in [-100i64, -1, 0, 1, 100, i64::MIN, i64::MAX] {
            let bytes = sortable_encode(&PropValue::Int(v)).unwrap();
            let decoded = decode_sortable(ColumnKind::Int, &bytes).unwrap();
            assert_eq!(decoded, PropValue::Int(v));
        }
    }

    #[test]
    fn str_sortable_encoding_preserves_prefix_order() {
        let a = sortable_encode(&PropValue::Str("ab".to_string())).unwrap();
        let b = sortable_encode(&PropValue::Str("abc".to_string())).unwrap();
        let c = sortable_encode(&PropValue::Str("abd".to_string())).unwrap();
        assert!(a < b, "\"ab\" must sort before \"abc\"");
        assert!(b < c, "\"abc\" must sort before \"abd\"");
    }

    #[test]
    fn str_sortable_roundtrip() {
        for s in ["", "hello", "a longer string with spaces"] {
            let bytes = sortable_encode(&PropValue::Str(s.to_string())).unwrap();
            let decoded = decode_sortable(ColumnKind::Str, &bytes).unwrap();
            assert_eq!(decoded, PropValue::Str(s.to_string()));
        }
    }

    #[test]
    fn index_table_is_cached_across_calls() {
        let schema = RelSchema {
            name: "widgets",
            columns: &[],
            primary_key: "id",
            auto_increment_pk: true,
            indexed_columns: &[],
        };
        let t1 = index_table(&schema, "color");
        let t2 = index_table(&schema, "color");
        assert_eq!(t1.0.as_ptr(), t2.0.as_ptr(), "must return the same leaked str, not re-leak");
    }
}
