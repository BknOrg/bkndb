//! FFI surface of the text query languages (SQL and graph `MATCH`).
use std::collections::HashMap;

use bkndb_core::lang::{Params, QueryResult};

use crate::types::FfiPropValue;

/// Tabular result of a SQL statement or graph query.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiQueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<FfiPropValue>>,
    /// Rows inserted/updated/deleted (0 for queries).
    pub affected: u64,
}

impl From<QueryResult> for FfiQueryResult {
    fn from(r: QueryResult) -> Self {
        Self {
            columns: r.columns,
            rows: r.rows.into_iter().map(|row| row.into_iter().map(Into::into).collect()).collect(),
            affected: r.affected,
        }
    }
}

pub(crate) fn params(positional: Vec<FfiPropValue>, named: Option<HashMap<String, FfiPropValue>>) -> Params {
    Params {
        positional: positional.into_iter().map(Into::into).collect(),
        named: named.unwrap_or_default().into_iter().map(|(k, v)| (k, v.into())).collect(),
    }
}
