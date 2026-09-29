//! Answering `search_vector` from an index, with filter handling.
use super::*;

/// Answers a vector search from an index, or `None` when there's no usable
/// index (none on the column, another metric, empty) or a selective filter
/// makes scanning the filtered rows the better plan.
#[allow(clippy::too_many_arguments)]
pub(crate) fn search_in<R: StorageReadTx>(
    rtx: &R,
    schema: &TableSchema,
    column: &str,
    query: &[f32],
    limit: usize,
    metric: VectorMetric,
    filter: Option<&Query>,
    ef_search: Option<usize>,
) -> Result<Option<Vec<ScoredRow>>, BknError> {
    let Some(mut index) = Hnsw::load(rtx, schema.name(), column)? else { return Ok(None) };
    if index.metric != metric || index.meta.count == 0 {
        return Ok(None);
    }
    if query.len() != index.meta.dim as usize {
        return Err(BknError::InvalidQuery(format!(
            "vector index '{}' holds {}-dimensional vectors, the query has {}",
            index.name,
            index.meta.dim,
            query.len()
        )));
    }
    let Some(q) = index.prepare(query.to_vec()) else { return Ok(Some(Vec::new())) };
    let qnorm = query.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
    let pk_kind = schema.primary_key_column().kind;
    let total = index.meta.count as usize;
    let mut ef = ef_search.unwrap_or((limit.saturating_mul(4)).max(64)).max(limit).max(1);
    loop {
        let found = index.search(rtx, &q, ef)?;
        let mut hits: Vec<(f64, Vec<u8>, ScoredRow)> = Vec::new();
        for pk in &found {
            let Some(row) = get_in(rtx, schema, &decode_sortable(pk_kind, pk)?)? else { continue };
            if filter.is_some_and(|f| !f.filters.iter().all(|e| e.eval(schema, &row))) {
                continue;
            }
            let Some(v) = row.values.get(column).and_then(vector_of) else { continue };
            let Some((s, goodness)) = score(metric, &v, query, qnorm) else { continue };
            hits.push((goodness, pk.clone(), ScoredRow { row, score: s }));
        }
        if hits.len() >= limit || filter.is_none() || found.len() < ef {
            hits.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
            hits.truncate(limit);
            return Ok(Some(hits.into_iter().map(|(_, _, r)| r).collect()));
        }
        // The filter rejected too many candidates: widen, or scan instead.
        if ef >= total || ef >= MAX_EF {
            return Ok(None);
        }
        ef = ef.saturating_mul(4);
    }
}
