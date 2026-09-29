//! Full-text and vector search over relational tables (the `search`
//! feature).
//!
//! **Full-text.** [`RelationalDb::create_fulltext_index`] builds an inverted
//! index over a `Str` column, kept up to date by every insert, update and
//! delete from then on (in the same transaction). Text is split into
//! lowercase runs of Unicode letters and digits — no stemming or stop words,
//! so it's language-neutral. [`RelationalDb::search_text`] ranks rows with
//! BM25; a query word ending in `*` matches as a prefix.
//!
//! Storage: `<table>__fts_<column>` holds postings
//! `term ++ 0x00 ++ sortable(pk)` → term frequency, `<table>__ftsdoc_<column>`
//! holds `sortable(pk)` → document length, and the shared `meta` table holds
//! the registry (`relfts:<table>`) and corpus totals
//! (`relftsstat:<table>:<column>`).
//!
//! **Vectors.** [`RelationalDb::search_vector`] is exact k-nearest-neighbour
//! search over a column holding embeddings — either a `List` of numbers, or
//! `Bytes` of packed little-endian `f32`s (4 bytes per dimension, far more
//! compact). Without an index it streams the table (or only the rows an
//! optional filter selects, using the query planner) keeping the best
//! `limit` in a heap: exact, linear time, fine up to a few hundred thousand
//! rows. [`RelationalDb::create_vector_index`] adds an approximate HNSW index
//! (see [`crate::relational::ann`]) that searches with the same metric then
//! use automatically.
use std::cmp::Ordering;
use std::collections::{BTreeMap, BinaryHeap, HashMap};
use std::ops::Bound;

use crate::relational::ann::{self, VectorIndexInfo, VectorIndexOptions, VectorSearchOptions};
use crate::relational::catalog;
use crate::relational::codec::{decode_sortable, intern, meta_table, sortable_encode};
use crate::relational::db::{get_in, scan_all_in, RelationalDb, Row};
use crate::relational::query::{matching_rows, Query};
use crate::relational::schema::{ColumnKind, TableSchema};
use crate::relational::Expr;
use crate::value::{PropValue, Properties};
use crate::{BknError, StorageBackend, StorageReadTx, StorageWriteTx, TableSpec};

mod fulltext;
mod vector;

pub use fulltext::*;
pub use vector::*;

// ---------------------------------------------------------------------------
// Queries
// ---------------------------------------------------------------------------

/// A search hit: the row and its score (BM25 for text; for vectors the
/// cosine similarity, dot product, or Euclidean distance).
#[derive(Debug, Clone, PartialEq)]
pub struct ScoredRow {
    pub row: Row,
    pub score: f64,
}

fn validated_filter(schema: &TableSchema, filter: Option<&Expr>) -> Result<Option<Query>, BknError> {
    let Some(f) = filter else { return Ok(None) };
    let q = Query::new().filter(f.clone());
    q.check_columns(schema, [])?;
    Ok(Some(q))
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

impl<B: StorageBackend> RelationalDb<B> {
    /// Builds a full-text index over a `Str` column of a registered table
    /// (backfilling existing rows). Returns `false` if it already exists.
    pub fn create_fulltext_index(&self, table: &str, column: &str) -> Result<bool, BknError> {
        self.write(|wtx| create_fulltext_index_in(wtx, table, column))
    }

    /// Removes a full-text index; `false` if there was none.
    pub fn drop_fulltext_index(&self, table: &str, column: &str) -> Result<bool, BknError> {
        self.write(|wtx| drop_fulltext_index_in(wtx, table, column))
    }

    /// Columns of `table` with a full-text index.
    pub fn fulltext_indexes(&self, table: &str) -> Result<Vec<String>, BknError> {
        fulltext_columns_in(&self.backend.begin_read()?, table)
    }

    /// Up to `limit` rows whose `column` best matches `query` (BM25). See
    /// the [module docs](self).
    pub fn search_text(
        &self,
        table: &str,
        column: &str,
        query: &str,
        limit: usize,
        match_all: bool,
        filter: Option<&Expr>,
    ) -> Result<Vec<ScoredRow>, BknError> {
        let rtx = self.backend.begin_read()?;
        let schema = catalog::require_schema_in(&rtx, table)?;
        search_text_in(&rtx, &schema, column, query, limit, match_all, filter)
    }

    /// The `limit` rows whose embedding in `column` is nearest to `query`,
    /// from the column's vector index when it has one for `metric`
    /// (approximate), else by scanning (exact). See the [module docs](self).
    pub fn search_vector(
        &self,
        table: &str,
        column: &str,
        query: &[f32],
        limit: usize,
        metric: VectorMetric,
        filter: Option<&Expr>,
    ) -> Result<Vec<ScoredRow>, BknError> {
        self.search_vector_with(table, column, query, limit, metric, filter, VectorSearchOptions::default())
    }

    /// [`Self::search_vector`] with per-query options: force an exact scan,
    /// or trade speed for recall with `ef_search`.
    #[allow(clippy::too_many_arguments)]
    pub fn search_vector_with(
        &self,
        table: &str,
        column: &str,
        query: &[f32],
        limit: usize,
        metric: VectorMetric,
        filter: Option<&Expr>,
        options: VectorSearchOptions,
    ) -> Result<Vec<ScoredRow>, BknError> {
        let rtx = self.backend.begin_read()?;
        let schema = catalog::require_schema_in(&rtx, table)?;
        search_vector_in(&rtx, &schema, column, query, limit, metric, filter, options)
    }

    /// Builds an approximate (HNSW) vector index over a `List`/`Bytes`
    /// embedding column for `options.metric`, backfilling existing rows.
    /// Returns `false` if the column already has one (drop it first to
    /// change the parameters). Once it exists, every indexed vector must
    /// have the same number of dimensions. See [`crate::relational::ann`].
    pub fn create_vector_index(&self, table: &str, column: &str, options: VectorIndexOptions) -> Result<bool, BknError> {
        self.write(|wtx| ann::create_in(wtx, table, column, options))
    }

    /// Removes a vector index; `false` if there was none.
    pub fn drop_vector_index(&self, table: &str, column: &str) -> Result<bool, BknError> {
        self.write(|wtx| ann::drop_in(wtx, table, column))
    }

    /// The vector indexes of `table`.
    pub fn vector_indexes(&self, table: &str) -> Result<Vec<VectorIndexInfo>, BknError> {
        ann::list_in(&self.backend.begin_read()?, table)
    }
}
