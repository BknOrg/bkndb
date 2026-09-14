pub use crate::value::{PropValue, Properties};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
pub struct NodeId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
pub struct EdgeId(pub u64);

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NodeRecord {
    pub label: String,
    pub properties: Properties,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EdgeRecord {
    pub from: NodeId,
    pub to: NodeId,
    pub edge_type: String,
    pub properties: Properties,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_properties() -> Properties {
        let mut p = Properties::new();
        p.insert("a".to_string(), PropValue::Null);
        p.insert("b".to_string(), PropValue::Bool(true));
        p.insert("c".to_string(), PropValue::Int(-42));
        p.insert("d".to_string(), PropValue::Float(3.5));
        p.insert("e".to_string(), PropValue::Str("hi".to_string()));
        p.insert("f".to_string(), PropValue::Bytes(vec![1, 2, 3]));
        p
    }

    #[test]
    fn node_record_bincode_roundtrip() {
        let record = NodeRecord {
            label: "Function".to_string(),
            properties: sample_properties(),
        };
        let bytes = bincode::serialize(&record).unwrap();
        let decoded: NodeRecord = bincode::deserialize(&bytes).unwrap();
        assert_eq!(decoded.label, record.label);
        assert_eq!(decoded.properties, record.properties);
    }

    #[test]
    fn edge_record_bincode_roundtrip() {
        let record = EdgeRecord {
            from: NodeId(1),
            to: NodeId(2),
            edge_type: "calls".to_string(),
            properties: sample_properties(),
        };
        let bytes = bincode::serialize(&record).unwrap();
        let decoded: EdgeRecord = bincode::deserialize(&bytes).unwrap();
        assert_eq!(decoded.from, record.from);
        assert_eq!(decoded.to, record.to);
        assert_eq!(decoded.edge_type, record.edge_type);
        assert_eq!(decoded.properties, record.properties);
    }
}
