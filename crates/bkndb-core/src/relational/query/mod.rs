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

mod plan;
mod exec;

pub(crate) use plan::*;
pub(crate) use exec::*;

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
