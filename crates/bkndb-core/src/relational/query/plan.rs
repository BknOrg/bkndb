//! Query planning: choosing pk/index/scan access and streaming candidate rows.
use super::*;

/// How candidate rows are fetched before the full filter is applied.
#[derive(Debug)]
pub(super) enum Access {
    PkGet(Vec<PropValue>),
    IndexEq(String, Vec<PropValue>),
    PkRange(Bound<PropValue>, Bound<PropValue>),
    IndexPrefix(String, String),
    IndexRange(String, Bound<PropValue>, Bound<PropValue>),
    Scan,
}

pub(super) fn conjuncts<'e>(filters: &'e [Expr], out: &mut Vec<&'e Expr>) {
    for f in filters {
        match f {
            Expr::And(v) => conjuncts(v, out),
            e => out.push(e),
        }
    }
}

/// Whether `v` can be looked up in a key/index over a column of `kind`.
pub(super) fn usable_key(kind: ColumnKind, v: &PropValue) -> bool {
    ColumnKind::of(v) == kind && sortable_encode(v).is_ok()
}

pub(super) fn dedup_values(values: impl IntoIterator<Item = PropValue>) -> Vec<PropValue> {
    let mut seen = HashSet::new();
    values
        .into_iter()
        .filter(|v| sortable_encode(v).map(|k| seen.insert(k)).unwrap_or(false))
        .collect()
}

pub(super) fn tighter(current: Bound<PropValue>, new: Bound<PropValue>, lower: bool) -> Bound<PropValue> {
    let (cur_v, new_v) = match (&current, &new) {
        (Bound::Unbounded, _) => return new,
        (_, Bound::Unbounded) => return current,
        (Bound::Included(a) | Bound::Excluded(a), Bound::Included(b) | Bound::Excluded(b)) => (a, b),
    };
    match compare_values(new_v, cur_v) {
        Some(std::cmp::Ordering::Equal) => {
            if matches!(new, Bound::Excluded(_)) {
                new
            } else {
                current
            }
        }
        Some(ord) if (ord == std::cmp::Ordering::Greater) == lower => new,
        _ => current,
    }
}

pub(super) fn range_bounds(conj: &[&Expr], column: &str, kind: ColumnKind) -> Option<(Bound<PropValue>, Bound<PropValue>)> {
    let (mut lo, mut hi) = (Bound::Unbounded, Bound::Unbounded);
    for e in conj {
        if let Expr::Cmp(c, op, v) = e {
            if c != column || !usable_key(kind, v) {
                continue;
            }
            match op {
                CmpOp::Gt => lo = tighter(lo, Bound::Excluded(v.clone()), true),
                CmpOp::Ge => lo = tighter(lo, Bound::Included(v.clone()), true),
                CmpOp::Lt => hi = tighter(hi, Bound::Excluded(v.clone()), false),
                CmpOp::Le => hi = tighter(hi, Bound::Included(v.clone()), false),
                _ => {}
            }
        }
    }
    if matches!((&lo, &hi), (Bound::Unbounded, Bound::Unbounded)) {
        None
    } else {
        Some((lo, hi))
    }
}

pub(super) fn plan(schema: &TableSchema, filters: &[Expr]) -> Access {
    let mut conj = Vec::new();
    conjuncts(filters, &mut conj);
    let pk = schema.primary_key();
    let pk_kind = schema.primary_key_column().kind;
    let kind_of = |c: &str| schema.column(c).map(|c| c.kind);

    for e in &conj {
        match e {
            Expr::Cmp(c, CmpOp::Eq, v) if c == pk && usable_key(pk_kind, v) => {
                return Access::PkGet(vec![v.clone()]);
            }
            Expr::In(c, vs) if c == pk && vs.iter().all(|v| usable_key(pk_kind, v)) => {
                return Access::PkGet(dedup_values(vs.iter().cloned()));
            }
            _ => {}
        }
    }
    for e in &conj {
        match e {
            Expr::Cmp(c, CmpOp::Eq, v) if schema.is_indexed(c) && kind_of(c).is_some_and(|k| usable_key(k, v)) => {
                return Access::IndexEq(c.clone(), vec![v.clone()]);
            }
            Expr::In(c, vs) if schema.is_indexed(c) && kind_of(c).is_some_and(|k| vs.iter().all(|v| usable_key(k, v))) => {
                return Access::IndexEq(c.clone(), dedup_values(vs.iter().cloned()));
            }
            _ => {}
        }
    }
    if let Some((lo, hi)) = range_bounds(&conj, pk, pk_kind) {
        return Access::PkRange(lo, hi);
    }
    for e in &conj {
        if let Expr::Prefix(c, p) = e
            && schema.is_indexed(c)
            && kind_of(c) == Some(ColumnKind::Str)
            && !p.as_bytes().contains(&0)
        {
            return Access::IndexPrefix(c.clone(), p.clone());
        }
    }
    for c in schema.indexed_columns() {
        if let Some((lo, hi)) = kind_of(c).and_then(|k| range_bounds(&conj, c, k)) {
            return Access::IndexRange(c.clone(), lo, hi);
        }
    }
    Access::Scan
}

pub(super) type RowIter<'r> = Box<dyn Iterator<Item = Result<Row, BknError>> + 'r>;

pub(super) fn decode_base_rows<'r>(schema: &TableSchema, raw: KvIter<'r>) -> RowIter<'r> {
    let pk_kind = schema.primary_key_column().kind;
    Box::new(raw.map(move |kv| {
        let (k, v) = kv?;
        Ok(Row {
            pk: decode_sortable(pk_kind, &k)?,
            values: decode_row(&v)?,
        })
    }))
}

pub(super) fn as_byte_bound(b: &Bound<Vec<u8>>) -> Bound<&[u8]> {
    match b {
        Bound::Included(v) => Bound::Included(v.as_slice()),
        Bound::Excluded(v) => Bound::Excluded(v.as_slice()),
        Bound::Unbounded => Bound::Unbounded,
    }
}

pub(super) fn candidates<'r, R: StorageReadTx>(rtx: &'r R, schema: &TableSchema, access: Access) -> Result<RowIter<'r>, BknError> {
    Ok(match access {
        Access::PkGet(pks) => {
            let mut rows = Vec::with_capacity(pks.len());
            for pk in &pks {
                if let Some(r) = get_in(rtx, schema, pk)? {
                    rows.push(Ok(r));
                }
            }
            Box::new(rows.into_iter())
        }
        Access::IndexEq(col, values) => {
            let mut rows = Vec::new();
            for v in &values {
                rows.extend(index_lookup_eq_in(rtx, schema, &col, v)?.into_iter().map(Ok));
            }
            Box::new(rows.into_iter())
        }
        Access::PkRange(lo, hi) => {
            // Base-table keys are exactly `sortable(pk)`, so the index-key
            // bound helpers apply unchanged.
            let (lo, hi) = (lower_bound_bytes(&lo)?, upper_bound_bytes(&hi)?);
            decode_base_rows(schema, rtx.scan(base_table(schema), as_byte_bound(&lo), as_byte_bound(&hi))?)
        }
        Access::IndexPrefix(col, prefix) => {
            Box::new(index_lookup_prefix_in(rtx, schema, &col, &prefix)?.into_iter().map(Ok))
        }
        Access::IndexRange(col, lo, hi) => {
            Box::new(index_lookup_range_in(rtx, schema, &col, &lo, &hi)?.into_iter().map(Ok))
        }
        Access::Scan => {
            decode_base_rows(schema, rtx.scan(base_table(schema), Bound::Unbounded, Bound::Unbounded)?)
        }
    })
}

/// Every row matching `query`'s filters, ignoring ordering/paging/projection,
/// streamed straight from the chosen access path.
pub(crate) fn matching_rows<'r, R: StorageReadTx>(rtx: &'r R, schema: &'r TableSchema, query: &'r Query) -> Result<RowIter<'r>, BknError> {
    let rows = candidates(rtx, schema, plan(schema, &query.filters))?;
    Ok(Box::new(rows.filter(move |row| match row {
        Ok(row) => query.matches(schema, row),
        Err(_) => true,
    })))
}
