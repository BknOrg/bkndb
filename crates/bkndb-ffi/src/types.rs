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
    /// Microseconds since the Unix epoch, UTC.
    Timestamp(i64),
    /// A UUID as two big-endian halves (`hi` = first 8 bytes).
    Uuid { hi: u64, lo: u64 },
    List(Vec<FfiPropValue>),
    Map(HashMap<String, FfiPropValue>),
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
            PropValue::Timestamp(t) => FfiPropValue::Timestamp(t),
            PropValue::Uuid(u) => FfiPropValue::Uuid {
                hi: u64::from_be_bytes(u[..8].try_into().unwrap()),
                lo: u64::from_be_bytes(u[8..].try_into().unwrap()),
            },
            PropValue::List(l) => FfiPropValue::List(l.into_iter().map(Into::into).collect()),
            PropValue::Map(m) => FfiPropValue::Map(m.into_iter().map(|(k, v)| (k, v.into())).collect()),
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
            FfiPropValue::Timestamp(t) => PropValue::Timestamp(t),
            FfiPropValue::Uuid { hi, lo } => {
                let mut u = [0u8; 16];
                u[..8].copy_from_slice(&hi.to_be_bytes());
                u[8..].copy_from_slice(&lo.to_be_bytes());
                PropValue::Uuid(u)
            }
            FfiPropValue::List(l) => PropValue::List(l.into_iter().map(Into::into).collect()),
            FfiPropValue::Map(m) => PropValue::Map(m.into_iter().map(|(k, v)| (k, v.into())).collect()),
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

impl From<bkndb_core::graph::PathResult> for FfiPathResult {
    fn from(p: bkndb_core::graph::PathResult) -> Self {
        FfiPathResult {
            node_ids: p.nodes().into_iter().map(|n| n.0).collect(),
            edge_ids: p.edges().into_iter().map(|e| e.0).collect(),
            steps: p
                .steps
                .into_iter()
                .map(|s| FfiPathStep {
                    node_id: s.node.0,
                    via_edge_id: s.via_edge.map(|e| e.0),
                    edge_type: s.edge_type,
                })
                .collect(),
        }
    }
}

/// Rows to upsert into one registered table as part of a sync batch.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiTableRows {
    pub table: String,
    pub rows: Vec<HashMap<String, FfiPropValue>>,
}

#[derive(Debug, Clone, Default, PartialEq, uniffi::Record)]
pub struct FfiSyncBatch {
    pub nodes: Vec<FfiNodeInput>,
    pub edges: Vec<FfiEdgeInput>,
    /// Relational rows, upserted by primary key (so a batch can be re-applied).
    #[uniffi(default)]
    pub rows: Vec<FfiTableRows>,
    /// Edges that may connect nodes created by this same batch.
    #[uniffi(default)]
    pub linked_edges: Vec<FfiLinkedEdgeInput>,
}

/// An edge endpoint in a sync batch: an existing node, or the node at
/// `index` (0-based) in the batch's `nodes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiNodeRef {
    Existing { id: u64 },
    New { index: u32 },
}

impl From<FfiNodeRef> for bkndb_core::NodeRef {
    fn from(r: FfiNodeRef) -> Self {
        match r {
            FfiNodeRef::Existing { id } => bkndb_core::NodeRef::Existing(bkndb_core::graph::NodeId(id)),
            FfiNodeRef::New { index } => bkndb_core::NodeRef::New(index as usize),
        }
    }
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiLinkedEdgeInput {
    pub from: FfiNodeRef,
    pub edge_type: String,
    pub to: FfiNodeRef,
    #[uniffi(default)]
    pub properties: HashMap<String, FfiPropValue>,
}

/// A lowest-cost path and its total cost.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiWeightedPath {
    pub path: FfiPathResult,
    pub cost: f64,
}

/// An index on `property` of nodes with `label`.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiPropertyIndex {
    pub label: String,
    pub property: String,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiSyncResult {
    pub node_ids: Vec<u64>,
    pub edge_ids: Vec<u64>,
    /// Primary keys of `FfiSyncBatch::rows`, one list per table entry.
    pub row_pks: Vec<Vec<FfiPropValue>>,
}

/// A neighbor together with the type of the edge leading to it.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiTypedNeighbor {
    pub node_id: u64,
    pub edge_id: u64,
    pub edge_type: String,
}

/// One node reached by a breadth-first traversal.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiTraversalHit {
    pub node_id: u64,
    pub depth: u32,
    pub via_edge_id: Option<u64>,
    pub parent_id: Option<u64>,
}

/// Tuning knobs for the on-disk (LSM) engine.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiLsmOptions {
    /// Flush the in-memory write buffer to disk once it reaches this size.
    pub memtable_flush_bytes: u64,
    /// Compact automatically once this many on-disk segments exist.
    pub compaction_trigger_files: u32,
    /// Target size of one on-disk block (the unit that is checksummed,
    /// compressed and read per lookup). Default 4096.
    #[uniffi(default = None)]
    pub block_size_bytes: Option<u32>,
    /// lz4-compress on-disk blocks. Default true.
    #[uniffi(default = None)]
    pub compression: Option<bool>,
}

/// Row count of one relational table.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiTableCount {
    pub table: String,
    pub rows: u64,
}

/// File-level figures for an on-disk database (see `LsmStats`).
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiStorageStats {
    pub file_bytes: u64,
    pub sstable_count: u32,
    pub legacy_sstable_count: u32,
    pub sstable_bytes: u64,
    pub sstable_entries: u64,
    pub memtable_entries: u64,
    pub memtable_bytes: u64,
    pub wal_bytes: u64,
    pub reclaimable_bytes: u64,
}

/// Logical counts plus, for on-disk databases, storage figures.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiDbStats {
    pub nodes: u64,
    pub edges: u64,
    pub tables: Vec<FfiTableCount>,
    /// `None` for in-memory databases.
    pub storage: Option<FfiStorageStats>,
}

/// What `verify_integrity` checked (all zero for in-memory databases).
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiIntegrityReport {
    pub sstables_checked: u32,
    pub blocks_verified: u64,
    pub legacy_blocks_unchecked: u64,
    pub entries: u64,
    pub wal_records: u64,
}

impl From<bkndb::LsmStats> for FfiStorageStats {
    fn from(s: bkndb::LsmStats) -> Self {
        Self {
            file_bytes: s.file_bytes,
            sstable_count: s.sstable_count as u32,
            legacy_sstable_count: s.legacy_sstable_count as u32,
            sstable_bytes: s.sstable_bytes,
            sstable_entries: s.sstable_entries,
            memtable_entries: s.memtable_entries as u64,
            memtable_bytes: s.memtable_bytes as u64,
            wal_bytes: s.wal_bytes,
            reclaimable_bytes: s.reclaimable_bytes,
        }
    }
}

impl From<bkndb::IntegrityReport> for FfiIntegrityReport {
    fn from(r: bkndb::IntegrityReport) -> Self {
        Self {
            sstables_checked: r.sstables_checked as u32,
            blocks_verified: r.blocks_verified,
            legacy_blocks_unchecked: r.legacy_blocks_unchecked,
            entries: r.entries,
            wal_records: r.wal_records,
        }
    }
}
