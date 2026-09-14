//! Scalar value type shared by the graph layer's node/edge properties and
//! the relational layer's row columns — reusing one type is what makes a
//! graph node's property and a relational cell interchangeable without a
//! conversion layer.
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum PropValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Bytes(Vec<u8>),
}

/// `BTreeMap`, not `HashMap`: deterministic iteration order gives
/// deterministic bincode byte output, which matters for tests and for any
/// future content hashing of records.
pub type Properties = BTreeMap<String, PropValue>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prop_value_bincode_roundtrip_all_variants() {
        let mut p = Properties::new();
        p.insert("a".to_string(), PropValue::Null);
        p.insert("b".to_string(), PropValue::Bool(true));
        p.insert("c".to_string(), PropValue::Int(-42));
        p.insert("d".to_string(), PropValue::Float(3.5));
        p.insert("e".to_string(), PropValue::Str("hi".to_string()));
        p.insert("f".to_string(), PropValue::Bytes(vec![1, 2, 3]));

        let bytes = bincode::serialize(&p).unwrap();
        let decoded: Properties = bincode::deserialize(&bytes).unwrap();
        assert_eq!(decoded, p);
    }
}
