//! Executing queries: select, count, aggregate, update and delete.
use super::*;

pub(crate) fn select_in<R: StorageReadTx>(rtx: &R, schema: &TableSchema, query: &Query) -> Result<Vec<Row>, BknError> {
    query.check_columns(schema, [])?;
    let access = plan(schema, &query.filters);
    // Scans and pk ranges already yield rows in ascending pk order, so
    // `ORDER BY pk ASC` over them needs no sort — which keeps keyset
    // pagination (`pk > last ORDER BY pk LIMIT n`) streaming.
    let presorted = matches!(access, Access::Scan | Access::PkRange(..))
        && matches!(query.order.as_slice(), [(c, Order::Asc)] if c == schema.primary_key());
    let mut rows = if query.order.is_empty() || presorted {
        // No sort needed: stop decoding as soon as offset + limit rows matched.
        let wanted = query.limit.map(|l| l.saturating_add(query.offset));
        let mut out = Vec::new();
        for row in candidates(rtx, schema, access)? {
            if wanted.is_some_and(|w| out.len() >= w) {
                break;
            }
            let row = row?;
            if query.matches(schema, &row) {
                out.push(row);
            }
        }
        out.drain(..query.offset.min(out.len()));
        out
    } else {
        let mut all = matching_rows(rtx, schema, query)?.collect::<Result<Vec<_>, _>>()?;
        all.sort_by(|a, b| {
            for (c, order) in &query.order {
                let ord = total_cmp(resolve(schema, a, c), resolve(schema, b, c));
                let ord = if *order == Order::Desc { ord.reverse() } else { ord };
                if ord.is_ne() {
                    return ord;
                }
            }
            std::cmp::Ordering::Equal
        });
        let start = query.offset.min(all.len());
        let end = query.limit.map_or(all.len(), |l| start.saturating_add(l).min(all.len()));
        all.truncate(end);
        all.drain(..start);
        all
    };
    if let Some(cols) = &query.columns {
        for row in &mut rows {
            row.values.retain(|k, _| cols.iter().any(|c| c == k));
        }
    }
    Ok(rows)
}

/// Number of rows matching `query`'s filters (ordering, paging and
/// projection are ignored, as for SQL `SELECT COUNT(*)`).
pub(crate) fn count_in<R: StorageReadTx>(rtx: &R, schema: &TableSchema, query: &Query) -> Result<usize, BknError> {
    query.check_columns(schema, [])?;
    let mut n = 0;
    for row in matching_rows(rtx, schema, query)? {
        row?;
        n += 1;
    }
    Ok(n)
}

/// Grouped aggregation over the rows matching `query`'s filters. With an
/// empty `group_by` there is always exactly one output row (even over zero
/// matching rows); otherwise one row per distinct group, sorted by group key.
pub(crate) fn aggregate_in<R: StorageReadTx>(
    rtx: &R,
    schema: &TableSchema,
    query: &Query,
    group_by: &[String],
    aggs: &[Agg],
) -> Result<Vec<AggregateRow>, BknError> {
    query.check_columns(schema, aggs.iter().filter_map(Agg::column))?;
    // Group keys may be dotted paths into a column, like filters.
    if let Some(unknown) = group_by.iter().find(|g| root_column(schema, g).is_none()) {
        return Err(BknError::SchemaMismatch {
            table: schema.name().to_string(),
            message: format!("unknown column '{unknown}'"),
        });
    }
    for a in aggs {
        a.check(schema)?;
    }
    let mut groups: Vec<(Vec<PropValue>, Vec<Acc>)> = Vec::new();
    let mut index: HashMap<Vec<u8>, usize> = HashMap::new();
    if group_by.is_empty() {
        groups.push((Vec::new(), aggs.iter().map(Acc::new).collect()));
    }
    for row in matching_rows(rtx, schema, query)? {
        let row = row?;
        let slot = if group_by.is_empty() {
            0
        } else {
            let key: Vec<PropValue> = group_by
                .iter()
                .map(|c| resolve(schema, &row, c).cloned().unwrap_or(PropValue::Null))
                .collect();
            let encoded = bincode::serialize(&key).map_err(|e| BknError::Encoding(e.to_string()))?;
            *index.entry(encoded).or_insert_with(|| {
                groups.push((key, aggs.iter().map(Acc::new).collect()));
                groups.len() - 1
            })
        };
        for (acc, agg) in groups[slot].1.iter_mut().zip(aggs) {
            acc.add(agg, schema, &row);
        }
    }
    groups.sort_by(|(a, _), (b, _)| {
        a.iter()
            .zip(b)
            .map(|(x, y)| total_cmp(Some(x), Some(y)))
            .find(|o| o.is_ne())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    groups
        .into_iter()
        .map(|(group, accs)| {
            Ok(AggregateRow {
                group,
                values: accs.into_iter().map(|a| a.finish(schema.name())).collect::<Result<_, _>>()?,
            })
        })
        .collect()
}

/// Applies `sets` to every row selected by `query` (filters, and ordering +
/// limit if given), returning how many rows changed. Runs entirely in the
/// caller's transaction.
pub(crate) fn update_in<W: StorageWriteTx>(
    wtx: &mut W,
    schema: &TableSchema,
    query: &Query,
    sets: &[(String, PropValue)],
) -> Result<usize, BknError> {
    if let Some((c, _)) = sets.iter().find(|(c, _)| c == schema.primary_key()) {
        return Err(BknError::SchemaMismatch {
            table: schema.name().to_string(),
            message: format!("cannot set primary key column '{c}' via update"),
        });
    }
    let targets = select_in(wtx, schema, &Query { columns: None, ..query.clone() })?;
    let mut count = 0;
    for row in targets {
        let changed = update_row_in(wtx, schema, &row.pk, |values| {
            for (c, v) in sets {
                values.insert(c.clone(), v.clone());
            }
        })?;
        count += changed as usize;
    }
    Ok(count)
}

/// Deletes every row selected by `query`, returning how many were removed.
pub(crate) fn delete_in<W: StorageWriteTx>(wtx: &mut W, schema: &TableSchema, query: &Query) -> Result<usize, BknError> {
    let targets = select_in(wtx, schema, &Query { columns: None, ..query.clone() })?;
    let mut count = 0;
    for row in targets {
        count += delete_row_in(wtx, schema, &row.pk)? as usize;
    }
    Ok(count)
}
