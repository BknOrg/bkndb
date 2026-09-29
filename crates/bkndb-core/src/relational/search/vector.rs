//! Exact vector search and embedding helpers.
use super::*;

/// Distance/similarity used by [`RelationalDb::search_vector`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VectorMetric {
    /// Cosine similarity, highest first (zero-length vectors never match).
    Cosine,
    /// Dot product, highest first.
    Dot,
    /// Euclidean distance, lowest first.
    Euclidean,
}

/// A stored embedding as `f32`s: a `List` of numbers, or `Bytes` holding
/// little-endian `f32`s.
pub fn vector_of(value: &PropValue) -> Option<Vec<f32>> {
    match value {
        PropValue::List(items) => items
            .iter()
            .map(|v| match v {
                PropValue::Float(f) => Some(*f as f32),
                PropValue::Int(i) => Some(*i as f32),
                _ => None,
            })
            .collect(),
        PropValue::Bytes(b) if b.len() % 4 == 0 => Some(b.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect()),
        _ => None,
    }
}

/// Packs a vector as `Bytes` of little-endian `f32`s — the compact storage
/// form [`RelationalDb::search_vector`] reads.
pub fn pack_vector(v: &[f32]) -> PropValue {
    PropValue::Bytes(v.iter().flat_map(|x| x.to_le_bytes()).collect())
}

pub(super) struct Candidate {
    /// Higher = better, whatever the metric.
    pub(super) goodness: f64,
    pub(super) seq: usize,
    pub(super) score: f64,
    pub(super) row: Row,
}

impl PartialEq for Candidate {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for Candidate {}
impl PartialOrd for Candidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Candidate {
    /// Reversed, so a `BinaryHeap` pops the *worst* kept candidate first.
    fn cmp(&self, other: &Self) -> Ordering {
        other.goodness.total_cmp(&self.goodness).then_with(|| self.seq.cmp(&other.seq))
    }
}

/// `(score, goodness)` of a stored vector against the query (`qnorm` = the
/// query's length); goodness is higher-is-better whatever the metric.
/// `None` for a zero vector under cosine.
pub(crate) fn score(metric: VectorMetric, v: &[f32], query: &[f32], qnorm: f64) -> Option<(f64, f64)> {
    let dot: f64 = v.iter().zip(query).map(|(a, b)| *a as f64 * *b as f64).sum();
    match metric {
        VectorMetric::Dot => Some((dot, dot)),
        VectorMetric::Cosine => {
            let norm = v.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
            if norm == 0.0 || qnorm == 0.0 {
                return None;
            }
            let c = dot / (norm * qnorm);
            Some((c, c))
        }
        VectorMetric::Euclidean => {
            let d = v.iter().zip(query).map(|(a, b)| (*a as f64 - *b as f64).powi(2)).sum::<f64>().sqrt();
            Some((d, -d))
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn search_vector_in<R: StorageReadTx>(
    rtx: &R,
    schema: &TableSchema,
    column: &str,
    query: &[f32],
    limit: usize,
    metric: VectorMetric,
    filter: Option<&Expr>,
    options: VectorSearchOptions,
) -> Result<Vec<ScoredRow>, BknError> {
    match schema.column(column).map(|c| c.kind) {
        Some(ColumnKind::List | ColumnKind::Bytes) => {}
        Some(_) => {
            return Err(BknError::SchemaMismatch {
                table: schema.name().to_string(),
                message: format!("vector search needs a List or Bytes column; '{column}' isn't one"),
            });
        }
        None => {
            return Err(BknError::SchemaMismatch { table: schema.name().to_string(), message: format!("unknown column '{column}'") });
        }
    }
    if query.is_empty() {
        return Err(BknError::InvalidQuery("the query vector is empty".into()));
    }
    let q = validated_filter(schema, filter)?;
    if limit == 0 {
        return Ok(Vec::new());
    }
    if !options.exact
        && let Some(hits) = ann::search_in(rtx, schema, column, query, limit, metric, q.as_ref(), options.ef_search)?
    {
        return Ok(hits);
    }
    let q = q.unwrap_or_default();
    let qnorm = query.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
    let mut heap: BinaryHeap<Candidate> = BinaryHeap::with_capacity(limit + 1);
    for (seq, row) in matching_rows(rtx, schema, &q)?.enumerate() {
        let row = row?;
        let Some(v) = row.values.get(column).and_then(vector_of) else { continue };
        if v.len() != query.len() {
            return Err(BknError::InvalidQuery(format!(
                "row {:?} has a {}-dimensional vector, the query has {}",
                row.pk,
                v.len(),
                query.len()
            )));
        }
        let Some((score, goodness)) = score(metric, &v, query, qnorm) else { continue };
        heap.push(Candidate { goodness, seq, score, row });
        if heap.len() > limit {
            heap.pop();
        }
    }
    let mut best = heap.into_vec();
    best.sort_by(|a, b| b.goodness.total_cmp(&a.goodness).then_with(|| a.seq.cmp(&b.seq)));
    Ok(best.into_iter().map(|c| ScoredRow { row: c.row, score: c.score }).collect())
}
