//! Search result and vector index records.
use super::*;

/// A search hit: the row and its score (BM25 for text; for vectors the
/// cosine similarity, dot product or Euclidean distance).
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiScoredRow {
    pub row: FfiRow,
    pub score: f64,
}

impl From<bkndb_core::relational::ScoredRow> for FfiScoredRow {
    fn from(s: bkndb_core::relational::ScoredRow) -> Self {
        Self { row: s.row.into(), score: s.score }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiVectorMetric {
    /// Cosine similarity, highest first.
    Cosine,
    /// Dot product, highest first.
    Dot,
    /// Euclidean distance, lowest first.
    Euclidean,
}

impl From<FfiVectorMetric> for bkndb_core::relational::VectorMetric {
    fn from(m: FfiVectorMetric) -> Self {
        match m {
            FfiVectorMetric::Cosine => Self::Cosine,
            FfiVectorMetric::Dot => Self::Dot,
            FfiVectorMetric::Euclidean => Self::Euclidean,
        }
    }
}

impl From<bkndb_core::relational::VectorMetric> for FfiVectorMetric {
    fn from(m: bkndb_core::relational::VectorMetric) -> Self {
        use bkndb_core::relational::VectorMetric;
        match m {
            VectorMetric::Cosine => Self::Cosine,
            VectorMetric::Dot => Self::Dot,
            VectorMetric::Euclidean => Self::Euclidean,
        }
    }
}

/// An approximate (HNSW) vector index over one column.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiVectorIndexInfo {
    pub column: String,
    pub metric: FfiVectorMetric,
    pub m: u32,
    pub ef_construction: u32,
    /// `None` while the index is empty.
    pub dimensions: Option<u32>,
    pub vectors: u64,
}

impl From<bkndb_core::relational::VectorIndexInfo> for FfiVectorIndexInfo {
    fn from(i: bkndb_core::relational::VectorIndexInfo) -> Self {
        Self {
            column: i.column,
            metric: i.metric.into(),
            m: i.m as u32,
            ef_construction: i.ef_construction as u32,
            dimensions: i.dimensions.map(|d| d as u32),
            vectors: i.vectors,
        }
    }
}
