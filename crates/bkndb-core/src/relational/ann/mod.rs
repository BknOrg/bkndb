//! Approximate nearest-neighbour vector indexes (HNSW) — the scalable path
//! behind [`RelationalDb::search_vector`](crate::relational::RelationalDb::search_vector).
//!
//! [`RelationalDb::create_vector_index`](crate::relational::RelationalDb::create_vector_index)
//! builds a Hierarchical Navigable Small World graph over a `List`/`Bytes`
//! embedding column for one metric. Every node links to its closest
//! neighbours; upper layers hold ever fewer nodes with long-range links, so a
//! search greedily descends from the top and only compares the query with a
//! few hundred vectors instead of every row. Results are approximate (recall
//! is tuned by `m`, `ef_construction` and the per-query `ef_search`).
//!
//! The graph lives in the same key-value store as the table, and every
//! insert, update and delete maintains it inside the caller's transaction, so
//! it rolls back, backs up and recovers together with the rows.
//!
//! Storage (per indexed column): `<table>__ann_<column>` maps a node id
//! (big-endian `u64`) to `level ++ pk ++ vector`, `<table>__annlnk_<column>`
//! maps `id ++ layer` to the node's neighbour ids, and `<table>__annpk_<column>`
//! maps `sortable(pk)` to the node id. The shared `meta` table holds the
//! registry (`relann:<table>`) and the index parameters and entry point
//! (`relannmeta:<table>:<column>`).
//!
//! Deleting a row removes its node and re-links the neighbours it pointed
//! to; links from elsewhere that still name the removed node are skipped on
//! read and dropped the next time that list is rewritten.
use std::cmp::{Ordering, Reverse};
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::ops::Bound;
use std::rc::Rc;

use serde::{Deserialize, Serialize};

use crate::relational::catalog;
use crate::relational::codec::{decode_sortable, intern, meta_table, sortable_encode};
use crate::relational::db::{get_in, scan_all_in};
use crate::relational::query::Query;
use crate::relational::schema::{ColumnKind, TableSchema};
use crate::relational::search::{clear, score, vector_of, ScoredRow, VectorMetric};
use crate::value::Properties;
use crate::{BknError, StorageReadTx, StorageWriteTx, TableSpec};

mod storage;
mod graph;
mod search;

pub(crate) use storage::*;
use graph::*;
pub(crate) use search::*;

/// Parameters of a vector index, see
/// [`RelationalDb::create_vector_index`](crate::relational::RelationalDb::create_vector_index).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VectorIndexOptions {
    /// The metric the index is built for; searches with another metric scan
    /// the table instead.
    pub metric: VectorMetric,
    /// Links per node (layer 0 keeps up to `2 * m`). Higher: better recall,
    /// more space, slower writes. At least 2.
    pub m: usize,
    /// Candidates considered while inserting. Higher: a better graph and
    /// slower writes. At least 1.
    pub ef_construction: usize,
}

impl Default for VectorIndexOptions {
    fn default() -> Self {
        Self { metric: VectorMetric::Cosine, m: 16, ef_construction: 200 }
    }
}

/// One vector index, as listed by
/// [`RelationalDb::vector_indexes`](crate::relational::RelationalDb::vector_indexes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VectorIndexInfo {
    pub column: String,
    pub metric: VectorMetric,
    pub m: usize,
    pub ef_construction: usize,
    /// Dimensions of the indexed vectors; `None` while the index is empty.
    pub dimensions: Option<usize>,
    /// Vectors currently indexed.
    pub vectors: u64,
}

/// Per-query knobs for
/// [`RelationalDb::search_vector_with`](crate::relational::RelationalDb::search_vector_with).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VectorSearchOptions {
    /// Ignore any vector index and scan every row (exact results).
    pub exact: bool,
    /// Candidates an index search keeps (default `max(4 * limit, 64)`).
    /// Higher: better recall, slower queries.
    pub ef_search: Option<usize>,
}

/// Beyond this many candidates a filtered search gives up on the index and
/// scans the rows the filter selects.
const MAX_EF: usize = 16_384;
const MAX_LEVEL: u8 = 32;

// ---------------------------------------------------------------------------
// Storage layout
// ---------------------------------------------------------------------------

/// Keeps every vector index of `schema`'s table in step with one row change
/// (`pk` already sortable-encoded; see
/// [`crate::relational::search::on_row_change`]).
pub(crate) fn on_row_change<W: StorageWriteTx>(
    wtx: &mut W,
    schema: &TableSchema,
    pk: &[u8],
    old: Option<&Properties>,
    new: Option<&Properties>,
) -> Result<(), BknError> {
    for column in columns_in(&*wtx, schema.name())? {
        let (before, after) = (old.and_then(|v| v.get(&column)), new.and_then(|v| v.get(&column)));
        if before == after {
            continue;
        }
        let mut index = Hnsw::load(&*wtx, schema.name(), &column)?
            .ok_or_else(|| BknError::Corruption(format!("vector index '{}.{column}' has no metadata", schema.name())))?;
        if before.is_some() {
            index.remove(wtx, pk)?;
        }
        if let Some(v) = after.and_then(vector_of) {
            index.add(wtx, pk, v)?;
        }
        index.save(wtx)?;
    }
    Ok(())
}

fn drop_data<W: StorageWriteTx>(wtx: &mut W, table: &str, column: &str) -> Result<(), BknError> {
    clear(wtx, nodes_table(table, column))?;
    clear(wtx, links_table(table, column))?;
    clear(wtx, pks_table(table, column))?;
    wtx.delete(meta_table(), &meta_key(table, column))
}

fn is_vector_column(schema: &TableSchema, column: &str) -> bool {
    column != schema.primary_key() && schema.column(column).is_some_and(|c| matches!(c.kind, ColumnKind::List | ColumnKind::Bytes))
}

/// Drops the vector indexes of columns `schema` no longer has (after a
/// migration), or of every column (`schema = None`, table dropped).
pub(crate) fn retain_valid_in<W: StorageWriteTx>(wtx: &mut W, table: &str, schema: Option<&TableSchema>) -> Result<(), BknError> {
    let (keep, gone): (Vec<String>, Vec<String>) =
        columns_in(&*wtx, table)?.into_iter().partition(|c| schema.is_some_and(|s| is_vector_column(s, c)));
    if gone.is_empty() {
        return Ok(());
    }
    for c in &gone {
        drop_data(wtx, table, c)?;
    }
    save_columns(wtx, table, &keep)
}

pub(crate) fn create_in<W: StorageWriteTx>(wtx: &mut W, table: &str, column: &str, options: VectorIndexOptions) -> Result<bool, BknError> {
    let schema = catalog::require_schema_in(&*wtx, table)?;
    if schema.column(column).is_none() {
        return Err(BknError::SchemaMismatch { table: table.to_string(), message: format!("unknown column '{column}'") });
    }
    if !is_vector_column(&schema, column) {
        return Err(BknError::SchemaMismatch {
            table: table.to_string(),
            message: format!("vector indexes need a non-key List or Bytes column; '{column}' isn't one"),
        });
    }
    if options.m < 2 || options.m > 1024 || options.ef_construction == 0 {
        return Err(BknError::InvalidQuery("a vector index needs 2 <= m <= 1024 and ef_construction >= 1".into()));
    }
    let mut columns = columns_in(&*wtx, table)?;
    if columns.iter().any(|c| c == column) {
        return Ok(false);
    }
    // Start from a clean slate, then backfill.
    drop_data(wtx, table, column)?;
    columns.push(column.to_string());
    save_columns(wtx, table, &columns)?;
    let meta = AnnMeta {
        metric: metric_code(options.metric),
        m: options.m as u32,
        ef_construction: options.ef_construction.min(u32::MAX as usize) as u32,
        dim: 0,
        entry: None,
        max_level: 0,
        next_id: 1,
        count: 0,
    };
    wtx.put(meta_table(), &meta_key(table, column), &bincode::serialize(&meta).map_err(enc_err)?)?;
    let mut index = Hnsw::load(&*wtx, table, column)?.expect("just written");
    for row in scan_all_in(&*wtx, &schema)? {
        if let Some(v) = row.values.get(column).and_then(vector_of) {
            index.add(wtx, &sortable_encode(&row.pk)?, v)?;
        }
    }
    index.save(wtx)?;
    Ok(true)
}

pub(crate) fn drop_in<W: StorageWriteTx>(wtx: &mut W, table: &str, column: &str) -> Result<bool, BknError> {
    let mut columns = columns_in(&*wtx, table)?;
    let Some(pos) = columns.iter().position(|c| c == column) else {
        return Ok(false);
    };
    columns.remove(pos);
    drop_data(wtx, table, column)?;
    save_columns(wtx, table, &columns)?;
    Ok(true)
}

pub(crate) fn list_in<R: StorageReadTx>(rtx: &R, table: &str) -> Result<Vec<VectorIndexInfo>, BknError> {
    let mut out = Vec::new();
    for column in columns_in(rtx, table)? {
        let Some(index) = Hnsw::load(rtx, table, &column)? else { continue };
        out.push(VectorIndexInfo {
            metric: index.metric,
            m: index.meta.m as usize,
            ef_construction: index.meta.ef_construction as usize,
            dimensions: (index.meta.count > 0).then_some(index.meta.dim as usize),
            vectors: index.meta.count,
            column,
        });
    }
    Ok(out)
}
