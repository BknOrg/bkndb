use std::collections::{HashMap, HashSet};
use std::ops::Bound;

use crate::relational::codec::{base_table, decode_row, decode_sortable, lower_bound_bytes, sortable_encode, upper_bound_bytes};
use crate::relational::db::{
    delete_row_in, get_in, index_lookup_eq_in, index_lookup_prefix_in, index_lookup_range_in, update_row_in, RelTable,
    Row,
};
use crate::relational::expr::{compare_values, resolve, root_column, total_cmp, Acc, Agg, AggregateRow, CmpOp, Expr, Order};
use crate::relational::schema::{ColumnKind, TableSchema};
use crate::value::PropValue;
use crate::{BknError, KvIter, StorageBackend, StorageReadTx, StorageWriteTx};

/// A transaction-independent query description: filters (implicitly
/// ANDed), ordering, paging and projection. Run it through
/// [`RelTable::select`]'s builder, or directly with
/// [`crate::relational::BatchTable::find`] /
/// [`crate::relational::ReadTable::find`] inside a transaction.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Query {
    pub(crate) filters: Vec<Expr>,
    pub(crate) order: Vec<(String, Order)>,
    pub(crate) offset: usize,
    pub(crate) limit: Option<usize>,
    pub(crate) columns: Option<Vec<String>>,
}

impl Query {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn filter(mut self, expr: Expr) -> Self {
        self.filters.push(expr);
        self
    }

    pub fn where_eq(self, column: &str, value: impl Into<PropValue>) -> Self {
        self.filter(Expr::Cmp(column.to_string(), CmpOp::Eq, value.into()))
    }

    pub fn where_prefix(self, column: &str, prefix: &str) -> Self {
        self.filter(Expr::Prefix(column.to_string(), prefix.to_string()))
    }

    pub fn where_range(mut self, column: &str, start: Bound<PropValue>, end: Bound<PropValue>) -> Self {
        match start {
            Bound::Included(v) => self.filters.push(Expr::Cmp(column.to_string(), CmpOp::Ge, v)),
            Bound::Excluded(v) => self.filters.push(Expr::Cmp(column.to_string(), CmpOp::Gt, v)),
            Bound::Unbounded => {}
        }
        match end {
            Bound::Included(v) => self.filters.push(Expr::Cmp(column.to_string(), CmpOp::Le, v)),
            Bound::Excluded(v) => self.filters.push(Expr::Cmp(column.to_string(), CmpOp::Lt, v)),
            Bound::Unbounded => {}
        }
        self
    }

    pub fn order_by(mut self, column: &str, order: Order) -> Self {
        self.order.push((column.to_string(), order));
        self
    }

    pub fn order_by_asc(self, column: &str) -> Self {
        self.order_by(column, Order::Asc)
    }

    pub fn order_by_desc(self, column: &str) -> Self {
        self.order_by(column, Order::Desc)
    }

    pub fn offset(mut self, n: usize) -> Self {
        self.offset = n;
        self
    }

    pub fn limit(mut self, n: usize) -> Self {
        self.limit = Some(n);
        self
    }

    /// Projection: returned rows only carry these columns (plus the pk,
    /// which always lives in [`Row::pk`]).
    pub fn columns<S: Into<String>>(mut self, columns: impl IntoIterator<Item = S>) -> Self {
        self.columns = Some(columns.into_iter().map(Into::into).collect());
        self
    }

    pub(crate) fn check_columns<'q>(&'q self, schema: &TableSchema, extra: impl IntoIterator<Item = &'q str>) -> Result<(), BknError> {
        // Filters, ordering and grouping may use dotted paths into a column;
        // projection and aggregates name whole columns.
        let mut paths: Vec<&str> = Vec::new();
        for f in &self.filters {
            f.columns(&mut paths);
        }
        paths.extend(self.order.iter().map(|(c, _)| c.as_str()));
        let mut names: Vec<&str> = Vec::new();
        if let Some(cols) = &self.columns {
            names.extend(cols.iter().map(String::as_str));
        }
        names.extend(extra);
        let unknown = paths
            .into_iter()
            .find(|p| root_column(schema, p).is_none())
            .or_else(|| names.into_iter().find(|n| schema.column(n).is_none()));
        match unknown {
            Some(unknown) => Err(BknError::SchemaMismatch {
                table: schema.name().to_string(),
                message: format!("unknown column '{unknown}'"),
            }),
            None => Ok(()),
        }
    }

    fn matches(&self, schema: &TableSchema, row: &Row) -> bool {
        self.filters.iter().all(|f| f.eval(schema, row))
    }
}

// --- Planning ---

/// How candidate rows are fetched before the full filter is applied.
#[derive(Debug)]
enum Access {
    PkGet(Vec<PropValue>),
    IndexEq(String, Vec<PropValue>),
    PkRange(Bound<PropValue>, Bound<PropValue>),
    IndexPrefix(String, String),
    IndexRange(String, Bound<PropValue>, Bound<PropValue>),
    Scan,
}

fn conjuncts<'e>(filters: &'e [Expr], out: &mut Vec<&'e Expr>) {
    for f in filters {
        match f {
            Expr::And(v) => conjuncts(v, out),
            e => out.push(e),
        }
    }
}

/// Whether `v` can be looked up in a key/index over a column of `kind`.
fn usable_key(kind: ColumnKind, v: &PropValue) -> bool {
    ColumnKind::of(v) == kind && sortable_encode(v).is_ok()
}

fn dedup_values(values: impl IntoIterator<Item = PropValue>) -> Vec<PropValue> {
    let mut seen = HashSet::new();
    values
        .into_iter()
        .filter(|v| sortable_encode(v).map(|k| seen.insert(k)).unwrap_or(false))
        .collect()
}

fn tighter(current: Bound<PropValue>, new: Bound<PropValue>, lower: bool) -> Bound<PropValue> {
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

fn range_bounds(conj: &[&Expr], column: &str, kind: ColumnKind) -> Option<(Bound<PropValue>, Bound<PropValue>)> {
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

fn plan(schema: &TableSchema, filters: &[Expr]) -> Access {
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

type RowIter<'r> = Box<dyn Iterator<Item = Result<Row, BknError>> + 'r>;

fn decode_base_rows<'r>(schema: &TableSchema, raw: KvIter<'r>) -> RowIter<'r> {
    let pk_kind = schema.primary_key_column().kind;
    Box::new(raw.map(move |kv| {
        let (k, v) = kv?;
        Ok(Row {
            pk: decode_sortable(pk_kind, &k)?,
            values: decode_row(&v)?,
        })
    }))
}

fn as_byte_bound(b: &Bound<Vec<u8>>) -> Bound<&[u8]> {
    match b {
        Bound::Included(v) => Bound::Included(v.as_slice()),
        Bound::Excluded(v) => Bound::Excluded(v.as_slice()),
        Bound::Unbounded => Bound::Unbounded,
    }
}

fn candidates<'r, R: StorageReadTx>(rtx: &'r R, schema: &TableSchema, access: Access) -> Result<RowIter<'r>, BknError> {
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

// --- Execution (shared by every table/transaction flavor) ---

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

// --- Fluent builders bound to a `RelTable` (each runs its own transaction) ---

macro_rules! filter_methods {
    () => {
        pub fn filter(mut self, expr: Expr) -> Self {
            self.query = self.query.filter(expr);
            self
        }

        pub fn where_eq(mut self, column: &str, value: impl Into<PropValue>) -> Self {
            self.query = self.query.where_eq(column, value);
            self
        }

        /// Prefix predicate on a string column — uses the column's secondary
        /// index if it has one, otherwise filters during a table scan.
        pub fn where_prefix(mut self, column: &str, prefix: &str) -> Self {
            self.query = self.query.where_prefix(column, prefix);
            self
        }

        /// Range predicate — uses the pk or a secondary index when possible,
        /// otherwise filters during a table scan.
        pub fn where_range(mut self, column: &str, start: Bound<PropValue>, end: Bound<PropValue>) -> Self {
            self.query = self.query.where_range(column, start, end);
            self
        }

        pub fn order_by(mut self, column: &str, order: Order) -> Self {
            self.query = self.query.order_by(column, order);
            self
        }

        pub fn limit(mut self, n: usize) -> Self {
            self.query = self.query.limit(n);
            self
        }
    };
}

pub struct SelectQuery<'a, B: StorageBackend> {
    table: RelTable<'a, B>,
    query: Query,
}

impl<'a, B: StorageBackend> SelectQuery<'a, B> {
    pub(crate) fn new(table: RelTable<'a, B>) -> Self {
        Self { table, query: Query::new() }
    }

    filter_methods!();

    pub fn order_by_asc(self, column: &str) -> Self {
        self.order_by(column, Order::Asc)
    }

    pub fn order_by_desc(self, column: &str) -> Self {
        self.order_by(column, Order::Desc)
    }

    pub fn offset(mut self, n: usize) -> Self {
        self.query = self.query.offset(n);
        self
    }

    pub fn columns<S: Into<String>>(mut self, columns: impl IntoIterator<Item = S>) -> Self {
        self.query = self.query.columns(columns);
        self
    }

    pub fn query(&self) -> &Query {
        &self.query
    }

    pub fn run(self) -> Result<Vec<Row>, BknError> {
        let rtx = self.table.db.backend.begin_read()?;
        select_in(&rtx, &self.table.table.resolve(&rtx)?, &self.query)
    }

    pub fn count(self) -> Result<usize, BknError> {
        let rtx = self.table.db.backend.begin_read()?;
        count_in(&rtx, &self.table.table.resolve(&rtx)?, &self.query)
    }

    /// Ungrouped aggregates: one value per `aggs` entry.
    pub fn aggregate(self, aggs: &[Agg]) -> Result<Vec<PropValue>, BknError> {
        let rtx = self.table.db.backend.begin_read()?;
        let mut rows = aggregate_in(&rtx, &self.table.table.resolve(&rtx)?, &self.query, &[], aggs)?;
        Ok(rows.pop().map(|r| r.values).unwrap_or_default())
    }

    /// `GROUP BY group_by` aggregates.
    pub fn aggregate_by(self, group_by: &[&str], aggs: &[Agg]) -> Result<Vec<AggregateRow>, BknError> {
        let rtx = self.table.db.backend.begin_read()?;
        let group_by: Vec<String> = group_by.iter().map(|s| s.to_string()).collect();
        aggregate_in(&rtx, &self.table.table.resolve(&rtx)?, &self.query, &group_by, aggs)
    }
}

pub struct UpdateQuery<'a, B: StorageBackend> {
    table: RelTable<'a, B>,
    query: Query,
    sets: Vec<(String, PropValue)>,
}

impl<'a, B: StorageBackend> UpdateQuery<'a, B> {
    pub(crate) fn new(table: RelTable<'a, B>) -> Self {
        Self { table, query: Query::new(), sets: Vec::new() }
    }

    filter_methods!();

    pub fn set(mut self, column: &str, value: impl Into<PropValue>) -> Self {
        self.sets.push((column.to_string(), value.into()));
        self
    }

    /// Applies `.set(...)` mutations to every row matching the predicates,
    /// returning how many rows were changed. Matching zero rows is not an
    /// error — it returns `Ok(0)` — since this is a predicate-based bulk
    /// operation, not a point lookup by id.
    ///
    /// Atomic: matching and every row change happen in one write
    /// transaction, so an error part-way leaves the table untouched.
    pub fn run(self) -> Result<usize, BknError> {
        let mut wtx = self.table.db.backend.begin_write()?;
        let schema = self.table.table.resolve(&wtx)?;
        let n = update_in(&mut wtx, &schema, &self.query, &self.sets)?;
        wtx.commit()?;
        Ok(n)
    }
}

pub struct DeleteQuery<'a, B: StorageBackend> {
    table: RelTable<'a, B>,
    query: Query,
}

impl<'a, B: StorageBackend> DeleteQuery<'a, B> {
    pub(crate) fn new(table: RelTable<'a, B>) -> Self {
        Self { table, query: Query::new() }
    }

    filter_methods!();

    /// Deletes every row matching the predicates, returning how many rows
    /// were removed. Matching zero rows is not an error. Atomic, like
    /// [`UpdateQuery::run`].
    pub fn run(self) -> Result<usize, BknError> {
        let mut wtx = self.table.db.backend.begin_write()?;
        let schema = self.table.table.resolve(&wtx)?;
        let n = delete_in(&mut wtx, &schema, &self.query)?;
        wtx.commit()?;
        Ok(n)
    }
}
