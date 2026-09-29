//! Full-text and vector search methods of [`BknDbEngine`](super::BknDbEngine).
use super::*;

#[uniffi::export]
impl BknDbEngine {
    // ---- search ----

    /// Builds a full-text (BM25) index over a text column, backfilling
    /// existing rows; `false` if it already exists.
    pub fn create_fulltext_index(&self, table: String, column: String) -> Result<bool, FfiBknError> {
        write!(self, |b| b.relational().create_fulltext_index(&table, &column))
    }

    pub fn drop_fulltext_index(&self, table: String, column: String) -> Result<bool, FfiBknError> {
        write!(self, |b| b.relational().drop_fulltext_index(&table, &column))
    }

    pub fn list_fulltext_indexes(&self, table: String) -> Result<Vec<String>, FfiBknError> {
        read!(self, |r| r.relational().fulltext_indexes(&table))
    }

    /// Up to `limit` rows whose `column` best matches `query` (BM25).
    /// `word*` is a prefix match; `match_all` requires every word.
    #[uniffi::method(default(match_all = false, filter = []))]
    pub fn search_text(
        &self,
        table: String,
        column: String,
        query: String,
        limit: u32,
        match_all: bool,
        filter: Vec<FfiExprNode>,
    ) -> Result<Vec<FfiScoredRow>, FfiBknError> {
        let filter = filter_expr(filter)?;
        let hits = read!(self, |r| r.relational().search_text(&table, &column, &query, limit as usize, match_all, filter.as_ref()))?;
        Ok(hits.into_iter().map(Into::into).collect())
    }

    /// The `limit` rows whose embedding in `column` (a list of numbers, or
    /// bytes of little-endian f32s) is nearest to `vector`. Uses the column's
    /// vector index when it has one for `metric` (approximate; `ef_search`
    /// trades speed for recall), unless `exact` forces a full scan.
    #[uniffi::method(default(filter = [], exact = false, ef_search = None))]
    #[allow(clippy::too_many_arguments)]
    pub fn search_vector(
        &self,
        table: String,
        column: String,
        vector: Vec<f32>,
        limit: u32,
        metric: FfiVectorMetric,
        filter: Vec<FfiExprNode>,
        exact: bool,
        ef_search: Option<u32>,
    ) -> Result<Vec<FfiScoredRow>, FfiBknError> {
        let filter = filter_expr(filter)?;
        let options = VectorSearchOptions { exact, ef_search: ef_search.map(|e| e as usize) };
        let hits = read!(self, |r| r.relational().search_vector_with(
            &table,
            &column,
            &vector,
            limit as usize,
            metric.into(),
            filter.as_ref(),
            options
        ))?;
        Ok(hits.into_iter().map(Into::into).collect())
    }

    /// Builds an approximate (HNSW) vector index over a list/bytes embedding
    /// column for `metric`, backfilling existing rows; `false` if the column
    /// already has one.
    #[uniffi::method(default(m = 16, ef_construction = 200))]
    pub fn create_vector_index(
        &self,
        table: String,
        column: String,
        metric: FfiVectorMetric,
        m: u32,
        ef_construction: u32,
    ) -> Result<bool, FfiBknError> {
        let options = VectorIndexOptions { metric: metric.into(), m: m as usize, ef_construction: ef_construction as usize };
        write!(self, |b| b.relational().create_vector_index(&table, &column, options))
    }

    pub fn drop_vector_index(&self, table: String, column: String) -> Result<bool, FfiBknError> {
        write!(self, |b| b.relational().drop_vector_index(&table, &column))
    }

    pub fn list_vector_indexes(&self, table: String) -> Result<Vec<FfiVectorIndexInfo>, FfiBknError> {
        let list = read!(self, |r| r.relational().vector_indexes(&table))?;
        Ok(list.into_iter().map(Into::into).collect())
    }
}
