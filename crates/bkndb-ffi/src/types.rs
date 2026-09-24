use std::collections::HashMap;
use bkndb_core::value::{PropValue, Properties};

#[derive(Debug, Clone, PartialEq, uniffi::Enum)]
pub enum FfiPropValue {
    Null,
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Bytes(Vec<u8>),
}

impl From<PropValue> for FfiPropValue {
    fn from(p: PropValue) -> Self {
        match p {
            PropValue::Null => FfiPropValue::Null,
            PropValue::Str(s) => FfiPropValue::Str(s),
            PropValue::Int(i) => FfiPropValue::Int(i),
            PropValue::Float(f) => FfiPropValue::Float(f),
            PropValue::Bool(b) => FfiPropValue::Bool(b),
            PropValue::Bytes(b) => FfiPropValue::Bytes(b),
        }
    }
}

impl From<FfiPropValue> for PropValue {
    fn from(p: FfiPropValue) -> Self {
        match p {
            FfiPropValue::Null => PropValue::Null,
            FfiPropValue::Str(s) => PropValue::Str(s),
            FfiPropValue::Int(i) => PropValue::Int(i),
            FfiPropValue::Float(f) => PropValue::Float(f),
            FfiPropValue::Bool(b) => PropValue::Bool(b),
            FfiPropValue::Bytes(b) => PropValue::Bytes(b),
        }
    }
}

pub fn ffi_props_to_core(props: HashMap<String, FfiPropValue>) -> Properties {
    let mut out = Properties::new();
    for (k, v) in props {
        out.insert(k, v.into());
    }
    out
}

pub fn core_props_to_ffi(props: Properties) -> HashMap<String, FfiPropValue> {
    let mut out = HashMap::new();
    for (k, v) in props {
        out.insert(k, v.into());
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiDirection {
    Out,
    In,
    Both,
}

impl From<FfiDirection> for bkndb_core::graph::Direction {
    fn from(d: FfiDirection) -> Self {
        match d {
            FfiDirection::Out => bkndb_core::graph::Direction::Out,
            FfiDirection::In => bkndb_core::graph::Direction::In,
            FfiDirection::Both => bkndb_core::graph::Direction::Both,
        }
    }
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiNodeRecord {
    pub id: u64,
    pub label: String,
    pub properties: HashMap<String, FfiPropValue>,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiNodeInput {
    pub label: String,
    pub properties: HashMap<String, FfiPropValue>,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiEdgeRecord {
    pub id: u64,
    pub from: u64,
    pub to: u64,
    pub edge_type: String,
    pub properties: HashMap<String, FfiPropValue>,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiEdgeInput {
    pub from: u64,
    pub edge_type: String,
    pub to: u64,
    pub properties: HashMap<String, FfiPropValue>,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiNeighbor {
    pub node_id: u64,
    pub edge_id: u64,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiPathStep {
    pub node_id: u64,
    pub via_edge_id: Option<u64>,
    pub edge_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiPathResult {
    pub steps: Vec<FfiPathStep>,
    pub node_ids: Vec<u64>,
    pub edge_ids: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiHubRecord {
    pub node_id: u64,
    pub degree: u64,
}

#[derive(Debug, Clone, Default, PartialEq, uniffi::Record)]
pub struct FfiSyncBatch {
    pub nodes: Vec<FfiNodeInput>,
    pub edges: Vec<FfiEdgeInput>,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiSyncResult {
    pub node_ids: Vec<u64>,
    pub edge_ids: Vec<u64>,
}
