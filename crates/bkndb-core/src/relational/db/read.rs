//! Row reads: point gets, full scans and secondary-index lookups.
use super::*;

pub(crate) fn get_in<R: StorageReadTx>(
    rtx: &R,
    schema: &TableSchema,
    pk: &PropValue,
) -> Result<Option<Row>, BknError> {
    let Ok(row_key) = sortable_encode(pk) else {
        // A value that can't be a key (wrong kind, NUL byte) can't be stored.
        return Ok(None);
    };
    match rtx.get(base_table(schema), &row_key)? {
        Some(bytes) => Ok(Some(Row {
            pk: pk.clone(),
            values: decode_row(&bytes)?,
        })),
        None => Ok(None),
    }
}

pub(crate) fn scan_all_in<R: StorageReadTx>(
    rtx: &R,
    schema: &TableSchema,
) -> Result<Vec<Row>, BknError> {
    let rows = rtx.range(base_table(schema), Bound::Unbounded, Bound::Unbounded)?;
    let pk_kind = schema.primary_key_column().kind;
    rows.into_iter()
        .map(|(k, v)| {
            Ok(Row {
                pk: decode_sortable(pk_kind, &k)?,
                values: decode_row(&v)?,
            })
        })
        .collect()
}

pub(crate) fn index_lookup_eq_in<R: StorageReadTx>(
    rtx: &R,
    schema: &TableSchema,
    column: &str,
    value: &PropValue,
) -> Result<Vec<Row>, BknError> {
    let prefix = sortable_encode(value)?;
    let end = crate::relational::codec::prefix_upper_bound(&prefix);
    let rows = match &end {
        Some(end) => rtx.range(
            index_table(schema, column),
            Bound::Included(&prefix),
            Bound::Excluded(end),
        )?,
        None => rtx.range(
            index_table(schema, column),
            Bound::Included(&prefix),
            Bound::Unbounded,
        )?,
    };
    resolve_index_rows_in(rtx, schema, column, rows)
}

pub(crate) fn index_lookup_range_in<R: StorageReadTx>(
    rtx: &R,
    schema: &TableSchema,
    column: &str,
    start: &Bound<PropValue>,
    end: &Bound<PropValue>,
) -> Result<Vec<Row>, BknError> {
    let lo = lower_bound_bytes(start)?;
    let hi = upper_bound_bytes(end)?;
    let rows = rtx.range(
        index_table(schema, column),
        lo.as_ref().map(|v| v.as_slice()),
        hi.as_ref().map(|v| v.as_slice()),
    )?;
    resolve_index_rows_in(rtx, schema, column, rows)
}

pub(crate) fn index_lookup_prefix_in<R: StorageReadTx>(
    rtx: &R,
    schema: &TableSchema,
    column: &str,
    prefix: &str,
) -> Result<Vec<Row>, BknError> {
    let col = schema
        .column(column)
        .ok_or_else(|| BknError::Encoding(format!("no such column '{column}'")))?;
    if col.kind != ColumnKind::Str {
        return Err(BknError::Encoding(format!(
            "prefix search is only supported on Str columns, '{column}' is {:?}",
            col.kind
        )));
    }
    let (lo_bound, hi_bound) = sortable_str_prefix_bounds(prefix);
    let rows = rtx.range(
        index_table(schema, column),
        lo_bound.as_ref().map(|v| v.as_slice()),
        hi_bound.as_ref().map(|v| v.as_slice()),
    )?;
    resolve_index_rows_in(rtx, schema, column, rows)
}

pub(super) fn resolve_index_rows_in<R: StorageReadTx>(
    rtx: &R,
    schema: &TableSchema,
    column: &str,
    rows: Vec<(Vec<u8>, Vec<u8>)>,
) -> Result<Vec<Row>, BknError> {
    let indexed_kind = schema
        .column(column)
        .ok_or_else(|| BknError::Encoding(format!("no such column '{column}'")))?
        .kind;
    let pk_kind = schema.primary_key_column().kind;
    let mut out = Vec::with_capacity(rows.len());
    for (key, _) in rows {
        let pk = pk_from_index_key(indexed_kind, pk_kind, &key)?;
        let Some(row) = get_in(rtx, schema, &pk)? else {
            continue;
        };
        // Only trust an index entry that the row still agrees with. Guards
        // against stale entries in files written before overwrites cleaned
        // up their old index keys.
        let current = indexable(&row, schema, column).and_then(|v| index_key(v, &pk).ok());
        if current.as_deref() == Some(key.as_slice()) {
            out.push(row);
        }
    }
    Ok(out)
}
