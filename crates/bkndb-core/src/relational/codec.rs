use std::collections::HashSet;
use std::ops::Bound;
use std::sync::{Mutex, OnceLock};

use crate::relational::schema::{ColumnKind, TableDef, TableSchema};
use crate::value::{PropValue, Properties};
use crate::{BknError, StorageWriteTx, TableSpec};

/// Shared meta/counter table. Same physical table name as the graph layer's
/// own `META` (`crates/bkndb-core/src/graph/codec.rs`) — intentional: both
/// layers can coexist over one backend without a name collision because
/// counter keys are namespaced (`next_node_id`/`next_edge_id` vs
/// `relnext:<table>`), so sharing one physical table just avoids growing
/// the table count instead of causing conflicts.
const META: TableSpec = TableSpec("meta");

/// Returns a `'static` copy of `name`, leaking each distinct string at most
/// once per process. `TableSpec` requires `&'static str`, but table names
/// of runtime-defined tables (and every derived index table name) are only
/// known at runtime; caching by content keeps the leak bounded by the number
/// of distinct table/index names a process ever touches.
pub(crate) fn intern(name: &str) -> &'static str {
    static CACHE: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashSet::new()));
    let mut guard = cache.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(&existing) = guard.get(name) {
        return existing;
    }
    let leaked: &'static str = Box::leak(name.to_string().into_boxed_str());
    guard.insert(leaked);
    leaked
}

pub fn base_table(schema: &TableSchema) -> TableSpec {
    schema.base_table()
}

/// The physical `TableSpec` backing one column's secondary index:
/// `<table>__idx_<column>`.
pub fn index_table(schema: &TableSchema, column: &str) -> TableSpec {
    index_table_named(schema.name(), column)
}

pub fn index_table_named(table: &str, column: &str) -> TableSpec {
    TableSpec(intern(&format!("{table}__idx_{column}")))
}

/// Catalog entries live in the shared `meta` table under this prefix,
/// one per table: `relschema:<table name>` → `CATALOG_VERSION ++ bincode(TableDef)`.
pub(crate) const CATALOG_PREFIX: &[u8] = b"relschema:";
const CATALOG_VERSION: u8 = 1;

pub(crate) fn catalog_key(table: &str) -> Vec<u8> {
    let mut k = CATALOG_PREFIX.to_vec();
    k.extend_from_slice(table.as_bytes());
    k
}

pub(crate) fn encode_table_def(def: &TableDef) -> Result<Vec<u8>, BknError> {
    let mut out = vec![CATALOG_VERSION];
    out.extend(bincode::serialize(def).map_err(|e| BknError::Encoding(e.to_string()))?);
    Ok(out)
}

pub(crate) fn decode_table_def(bytes: &[u8]) -> Result<TableDef, BknError> {
    match bytes.split_first() {
        Some((&CATALOG_VERSION, rest)) => bincode::deserialize(rest).map_err(|e| BknError::Encoding(e.to_string())),
        Some((v, _)) => Err(BknError::Encoding(format!("unsupported catalog entry version {v}"))),
        None => Err(BknError::Encoding("empty catalog entry".to_string())),
    }
}

pub(crate) fn meta_table() -> TableSpec {
    META
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
    crate::value::sortable_key(value).ok_or_else(|| {
        BknError::Encoding(match value {
            PropValue::Str(_) => "strings used as a primary key or indexed value cannot contain a NUL byte".to_string(),
            _ => "only Int/Str/Timestamp/Uuid values can be used as a primary key or indexed column".to_string(),
        })
    })
}

pub fn decode_sortable(kind: ColumnKind, bytes: &[u8]) -> Result<PropValue, BknError> {
    match kind {
        ColumnKind::Int | ColumnKind::Timestamp => {
            let raw: [u8; 8] = bytes
                .try_into()
                .map_err(|_| BknError::Encoding("corrupt sortable int key".to_string()))?;
            let n = (u64::from_be_bytes(raw) ^ 0x8000_0000_0000_0000) as i64;
            Ok(if kind == ColumnKind::Int { PropValue::Int(n) } else { PropValue::Timestamp(n) })
        }
        ColumnKind::Uuid => {
            let raw: [u8; 16] = bytes
                .try_into()
                .map_err(|_| BknError::Encoding("corrupt sortable uuid key".to_string()))?;
            Ok(PropValue::Uuid(raw))
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
            "only Int/Str/Timestamp/Uuid columns support sortable decoding".to_string(),
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

/// Computes range bounds for prefix scan over a sortable string secondary index.
pub fn sortable_str_prefix_bounds(prefix: &str) -> (Bound<Vec<u8>>, Bound<Vec<u8>>) {
    if prefix.is_empty() {
        (Bound::Unbounded, Bound::Unbounded)
    } else {
        let p_bytes = prefix.as_bytes().to_vec();
        let hi = match prefix_upper_bound(&p_bytes) {
            Some(ub) => Bound::Excluded(ub),
            None => Bound::Unbounded,
        };
        (Bound::Included(p_bytes), hi)
    }
}


pub(crate) fn row_counter_key(table: &str) -> Vec<u8> {
    format!("relnext:{table}").into_bytes()
}


/// Reserves a contiguous block of `count` primary keys for `schema`,
/// updating the counter in `META` once. Returns the starting primary key.
pub fn reserve_pks<W: StorageWriteTx>(
    wtx: &mut W,
    schema: &TableSchema,
    count: u64,
) -> Result<i64, BknError> {
    if count == 0 {
        return Ok(0);
    }
    let key = row_counter_key(schema.name());
    let current = match wtx.get(META, &key)? {
        Some(bytes) => u64::from_be_bytes(
            bytes
                .as_slice()
                .try_into()
                .map_err(|_| BknError::Encoding("corrupt relational id counter".to_string()))?,
        ),
        None => 1,
    };
    wtx.put(META, &key, &(current + count).to_be_bytes())?;
    Ok(current as i64)
}

/// Makes sure future auto-increment allocations for `schema` start after
/// `pk` — needed whenever a row is written with an explicit pk (upsert) on
/// an auto-increment table, or the counter could later hand out that pk
/// again and collide.
pub fn bump_pk_counter_past<W: StorageWriteTx>(wtx: &mut W, schema: &TableSchema, pk: i64) -> Result<(), BknError> {
    if pk < 1 {
        return Ok(());
    }
    let key = row_counter_key(schema.name());
    let current = match wtx.get(META, &key)? {
        Some(bytes) => u64::from_be_bytes(
            bytes
                .as_slice()
                .try_into()
                .map_err(|_| BknError::Encoding("corrupt relational id counter".to_string()))?,
        ),
        None => 1,
    };
    let needed = pk as u64 + 1;
    if needed > current {
        wtx.put(META, &key, &needed.to_be_bytes())?;
    }
    Ok(())
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
        ColumnKind::Int | ColumnKind::Timestamp => Ok(8),
        ColumnKind::Uuid => Ok(16),
        ColumnKind::Str => key
            .iter()
            .position(|&b| b == 0)
            .map(|pos| pos + 1)
            .ok_or_else(|| BknError::Encoding("index key missing str terminator".to_string())),
        _ => Err(BknError::Encoding(
            "only Int/Str/Timestamp/Uuid columns support indexing".to_string(),
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
        let t1 = index_table_named("widgets", "color");
        let t2 = index_table_named("widgets", "color");
        assert_eq!(t1.0, "widgets__idx_color");
        assert_eq!(t1.0.as_ptr(), t2.0.as_ptr(), "must return the same leaked str, not re-leak");
    }

    #[test]
    fn strings_with_nul_are_rejected_as_key_material() {
        let with_nul = String::from_utf8(vec![b'a', 0, b'b']).unwrap();
        assert!(sortable_encode(&PropValue::Str(with_nul)).is_err());
    }
}
