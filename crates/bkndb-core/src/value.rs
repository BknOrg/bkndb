//! Value type shared by the graph layer's node/edge properties and the
//! relational layer's row columns — reusing one type is what makes a graph
//! node's property and a relational cell interchangeable without a
//! conversion layer.
use std::cmp::Ordering;
use std::collections::BTreeMap;

/// One property / column value.
///
/// Variants are only ever **appended**: values are persisted with bincode,
/// which tags an enum by variant index, so reordering would reinterpret
/// stored data.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum PropValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Bytes(Vec<u8>),
    /// A point in time: microseconds since the Unix epoch, UTC.
    Timestamp(i64),
    /// A 128-bit UUID, big-endian (RFC 9562 byte order).
    Uuid([u8; 16]),
    /// An ordered list of values (may nest).
    List(Vec<PropValue>),
    /// String-keyed values (may nest) — together with `List`, enough to hold
    /// any JSON document.
    Map(BTreeMap<String, PropValue>),
}

/// `BTreeMap`, not `HashMap`: deterministic iteration order gives
/// deterministic bincode byte output, which matters for tests and for any
/// future content hashing of records.
pub type Properties = BTreeMap<String, PropValue>;

impl PropValue {
    /// Short name of this value's kind, for error messages.
    pub fn kind_name(&self) -> &'static str {
        match self {
            PropValue::Null => "null",
            PropValue::Bool(_) => "bool",
            PropValue::Int(_) => "int",
            PropValue::Float(_) => "float",
            PropValue::Str(_) => "str",
            PropValue::Bytes(_) => "bytes",
            PropValue::Timestamp(_) => "timestamp",
            PropValue::Uuid(_) => "uuid",
            PropValue::List(_) => "list",
            PropValue::Map(_) => "map",
        }
    }

    /// Looks up a nested value by path: map keys by name, list elements by
    /// decimal index — e.g. `["author", "tags", "0"]`.
    pub fn get_path<'v, S: AsRef<str>>(&'v self, path: &[S]) -> Option<&'v PropValue> {
        let mut cur = self;
        for seg in path {
            let seg = seg.as_ref();
            cur = match cur {
                PropValue::Map(m) => m.get(seg)?,
                PropValue::List(l) => l.get(seg.parse::<usize>().ok()?)?,
                _ => return None,
            };
        }
        Some(cur)
    }
}

/// Order-preserving ("memcomparable") byte encoding of a key value, shared
/// by every relational key and index. Only `Int`, `Str`, `Timestamp` and
/// `Uuid` are keyable; `None` for anything else, including strings
/// containing a NUL byte.
///
/// - `Int`/`Timestamp`: sign bit flipped, then big-endian, so negatives
///   sort first.
/// - `Str`: UTF-8 bytes plus a `0x00` terminator, so `"ab" < "abc"` — which
///   is why an embedded NUL can't be allowed.
/// - `Uuid`: its 16 bytes as-is.
///
/// Encodings of different kinds can coincide (an `Int` and a `Timestamp`
/// with the same number), so a key is only meaningful together with its
/// kind — which a relational column fixes.
#[cfg_attr(not(any(feature = "graph", feature = "relational")), allow(dead_code))]
pub(crate) fn sortable_key(value: &PropValue) -> Option<Vec<u8>> {
    match value {
        PropValue::Int(i) | PropValue::Timestamp(i) => Some(((*i as u64) ^ 0x8000_0000_0000_0000).to_be_bytes().to_vec()),
        PropValue::Str(s) if !s.as_bytes().contains(&0) => {
            let mut out = Vec::with_capacity(s.len() + 1);
            out.extend_from_slice(s.as_bytes());
            out.push(0);
            Some(out)
        }
        PropValue::Uuid(u) => Some(u.to_vec()),
        _ => None,
    }
}

/// Ordering between two non-null values, or `None` if their kinds aren't
/// comparable. `Int`/`Float` compare numerically across kinds.
#[cfg_attr(not(any(feature = "graph", feature = "relational")), allow(dead_code))]
pub(crate) fn compare_values(a: &PropValue, b: &PropValue) -> Option<Ordering> {
    use PropValue::*;
    match (a, b) {
        (Int(x), Int(y)) => Some(x.cmp(y)),
        (Float(x), Float(y)) => x.partial_cmp(y),
        (Int(x), Float(y)) => (*x as f64).partial_cmp(y),
        (Float(x), Int(y)) => x.partial_cmp(&(*y as f64)),
        (Str(x), Str(y)) => Some(x.cmp(y)),
        (Bool(x), Bool(y)) => Some(x.cmp(y)),
        (Bytes(x), Bytes(y)) => Some(x.cmp(y)),
        (Timestamp(x), Timestamp(y)) => Some(x.cmp(y)),
        (Uuid(x), Uuid(y)) => Some(x.cmp(y)),
        (List(x), List(y)) => {
            // Lexicographic, element by element.
            for (a, b) in x.iter().zip(y) {
                match (a, b) {
                    (Null, Null) => continue,
                    _ => match compare_values(a, b)? {
                        Ordering::Equal => continue,
                        ord => return Some(ord),
                    },
                }
            }
            Some(x.len().cmp(&y.len()))
        }
        (Map(x), Map(y)) => (x == y).then_some(Ordering::Equal),
        _ => None,
    }
}

/// A total order over optional values for ORDER BY and grouping: nulls
/// (and missing values) first, then by kind, then by value.
#[cfg_attr(not(any(feature = "graph", feature = "relational")), allow(dead_code))]
pub(crate) fn total_cmp(a: Option<&PropValue>, b: Option<&PropValue>) -> Ordering {
    fn rank(v: Option<&PropValue>) -> u8 {
        match v {
            None | Some(PropValue::Null) => 0,
            Some(PropValue::Bool(_)) => 1,
            Some(PropValue::Int(_) | PropValue::Float(_)) => 2,
            Some(PropValue::Str(_)) => 3,
            Some(PropValue::Bytes(_)) => 4,
            Some(PropValue::Timestamp(_)) => 5,
            Some(PropValue::Uuid(_)) => 6,
            Some(PropValue::List(_)) => 7,
            Some(PropValue::Map(_)) => 8,
        }
    }
    rank(a).cmp(&rank(b)).then_with(|| match (a, b) {
        (Some(PropValue::Float(x)), Some(PropValue::Float(y))) => x.total_cmp(y),
        (Some(PropValue::Int(x)), Some(PropValue::Float(y))) => (*x as f64).total_cmp(y),
        (Some(PropValue::Float(x)), Some(PropValue::Int(y))) => x.total_cmp(&(*y as f64)),
        (Some(x), Some(y)) => compare_values(x, y).unwrap_or(Ordering::Equal),
        _ => Ordering::Equal,
    })
}

/// SQL `LIKE` matching over characters (`%` = any run, `_` = one).
#[cfg_attr(not(any(feature = "graph", feature = "relational")), allow(dead_code))]
pub(crate) fn like_matches(text: &str, pattern: &str, case_insensitive: bool) -> bool {
    let fold = |s: &str| -> Vec<char> {
        if case_insensitive {
            s.chars().flat_map(char::to_lowercase).collect()
        } else {
            s.chars().collect()
        }
    };
    let (t, p) = (fold(text), fold(pattern));
    // Greedy matching with backtracking to the last `%`: O(len(t) * len(p)).
    let (mut ti, mut pi) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '_' || (p[pi] != '%' && p[pi] == t[ti])) {
            ti += 1;
            pi += 1;
        } else if pi < p.len() && p[pi] == '%' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '%')
}

macro_rules! prop_from {
    ($($t:ty => $variant:ident $(as $cast:ty)?),* $(,)?) => {
        $(impl From<$t> for PropValue {
            fn from(v: $t) -> Self {
                PropValue::$variant(v $(as $cast)?)
            }
        })*
    };
}

prop_from! {
    bool => Bool,
    i64 => Int,
    i32 => Int as i64,
    u32 => Int as i64,
    f64 => Float,
    String => Str,
    Vec<u8> => Bytes,
    [u8; 16] => Uuid,
    Vec<PropValue> => List,
    BTreeMap<String, PropValue> => Map,
}

impl From<&str> for PropValue {
    fn from(v: &str) -> Self {
        PropValue::Str(v.to_string())
    }
}

impl From<&[u8]> for PropValue {
    fn from(v: &[u8]) -> Self {
        PropValue::Bytes(v.to_vec())
    }
}

impl<T: Into<PropValue>> From<Option<T>> for PropValue {
    fn from(v: Option<T>) -> Self {
        v.map_or(PropValue::Null, Into::into)
    }
}

impl From<std::time::SystemTime> for PropValue {
    /// Truncates to whole microseconds; saturates outside ±292,000 years.
    fn from(t: std::time::SystemTime) -> Self {
        let micros = match t.duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => i64::try_from(d.as_micros()).unwrap_or(i64::MAX),
            Err(e) => i64::try_from(e.duration().as_micros()).map_or(i64::MIN, |m| -m),
        };
        PropValue::Timestamp(micros)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Properties {
        let mut p = Properties::new();
        p.insert("a".to_string(), PropValue::Null);
        p.insert("b".to_string(), PropValue::Bool(true));
        p.insert("c".to_string(), PropValue::Int(-42));
        p.insert("d".to_string(), PropValue::Float(3.5));
        p.insert("e".to_string(), PropValue::Str("hi".to_string()));
        p.insert("f".to_string(), PropValue::Bytes(vec![1, 2, 3]));
        p.insert("g".to_string(), PropValue::Timestamp(1_700_000_000_000_000));
        p.insert("h".to_string(), PropValue::Uuid([7; 16]));
        let mut inner = Properties::new();
        inner.insert("k".to_string(), PropValue::List(vec![1.into(), "x".into(), PropValue::Null]));
        p.insert("i".to_string(), PropValue::Map(inner));
        p
    }

    #[test]
    fn prop_value_bincode_roundtrip_all_variants() {
        let p = sample();
        let bytes = bincode::serialize(&p).unwrap();
        let decoded: Properties = bincode::deserialize(&bytes).unwrap();
        assert_eq!(decoded, p);
    }

    #[test]
    fn existing_variants_keep_their_encoding() {
        // Variant indices are part of the on-disk format: data written
        // before the newer variants existed must decode unchanged.
        assert_eq!(bincode::serialize(&PropValue::Int(1)).unwrap(), [2, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(bincode::serialize(&PropValue::Bytes(vec![9])).unwrap()[0..4], [5, 0, 0, 0]);
        assert_eq!(bincode::serialize(&PropValue::Timestamp(0)).unwrap()[0..4], [6, 0, 0, 0]);
    }

    #[test]
    fn sortable_keys_order_timestamps_and_uuids() {
        let t = |m: i64| sortable_key(&PropValue::Timestamp(m)).unwrap();
        assert!(t(-5) < t(0) && t(0) < t(10));
        let mut lo = [0u8; 16];
        let mut hi = [0u8; 16];
        lo[15] = 1;
        hi[0] = 1;
        assert!(sortable_key(&PropValue::Uuid(lo)).unwrap() < sortable_key(&PropValue::Uuid(hi)).unwrap());
        assert!(sortable_key(&PropValue::List(vec![])).is_none());
    }

    #[test]
    fn nested_paths() {
        let p = PropValue::Map(sample());
        assert_eq!(p.get_path(&["i", "k", "1"]), Some(&PropValue::Str("x".into())));
        assert_eq!(p.get_path(&["i", "k", "9"]), None);
        assert_eq!(p.get_path(&["c", "x"]), None);
        assert_eq!(p.get_path::<&str>(&[]), Some(&p));
    }

    #[test]
    fn system_time_converts_to_micros() {
        let t = std::time::UNIX_EPOCH + std::time::Duration::from_micros(1_234_567);
        assert_eq!(PropValue::from(t), PropValue::Timestamp(1_234_567));
        let before = std::time::UNIX_EPOCH - std::time::Duration::from_micros(10);
        assert_eq!(PropValue::from(before), PropValue::Timestamp(-10));
    }
}
