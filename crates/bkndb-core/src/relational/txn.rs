use std::ops::Bound;

use crate::relational::catalog;
use crate::relational::db::{
    delete_row_in, get_in, insert_bulk_in, insert_in, insert_with_pk_bulk_in, update_row_in, OnConflict, Row, TableRef,
};
use crate::relational::expr::{Agg, AggregateRow};
use crate::relational::query::{aggregate_in, count_in, delete_in, select_in, update_in, Query};
use crate::relational::schema::TableSchema;
use crate::value::{PropValue, Properties};
use crate::{BknError, StorageReadTx, StorageWriteTx};

/// One open write transaction shared across several [`BatchTable`]s (one per
/// call to [`RelWriteBatch::table`]), so that inserts/updates/deletes spread
/// across multiple relational tables commit together atomically — the
/// capability [`crate::relational::RelTable`]'s own methods deliberately
/// don't offer, since each of *those* opens and commits its own transaction.
/// Built via [`crate::relational::RelationalDb::write_tx`].
pub struct RelWriteBatch<W: StorageWriteTx> {
    wtx: W,
}

impl<W: StorageWriteTx> RelWriteBatch<W> {
    pub(crate) fn new(wtx: W) -> Self {
        Self { wtx }
    }

    pub(crate) fn into_inner(self) -> W {
        self.wtx
    }

    pub fn table(&mut self, schema: impl Into<TableSchema>) -> BatchTable<'_, W> {
        BatchTable::new(&mut self.wtx, TableRef { schema: schema.into(), by_name: false })
    }

    /// Borrows this batch as a [`RelBatchView`], for its catalog operations.
    pub fn view(&mut self) -> RelBatchView<'_, W> {
        RelBatchView::new(&mut self.wtx)
    }
}

/// A single table's view into an in-progress write transaction. Mirrors
/// [`crate::relational::RelTable`]'s operations, but every method here reads
/// and writes through the batch's already-open transaction instead of
/// opening/committing its own — nothing is durable until the enclosing
/// `write_tx` closure returns `Ok` and its single `commit()` runs. Reads see
/// the transaction's own pending writes.
pub struct BatchTable<'s, W: StorageWriteTx> {
    wtx: &'s mut W,
    table: TableRef,
}

impl<'s, W: StorageWriteTx> BatchTable<'s, W> {
    /// Panics on a reserved table name — see
    /// [`crate::relational::RelationalDb::table`] for why this is a panic,
    /// not a `Result`. The single enforcement point shared by every way of
    /// getting a `BatchTable`.
    pub(crate) fn new(wtx: &'s mut W, table: TableRef) -> Self {
        crate::check_table_name(table.schema.name()).unwrap_or_else(|e| panic!("{e}"));
        Self { wtx, table }
    }

    /// The schema this handle was created with.
    pub fn schema(&self) -> &TableSchema {
        &self.table.schema
    }

    fn resolved(&self) -> Result<TableSchema, BknError> {
        self.table.resolve(&*self.wtx)
    }

    /// See [`crate::relational::RelTable::insert`].
    pub fn insert(&mut self, values: Properties) -> Result<PropValue, BknError> {
        let schema = self.resolved()?;
        insert_in(self.wtx, &schema, values, OnConflict::Error)
    }

    /// See [`crate::relational::RelTable::insert_bulk`].
    pub fn insert_bulk(&mut self, rows: impl IntoIterator<Item = Properties>) -> Result<Vec<PropValue>, BknError> {
        let schema = self.resolved()?;
        insert_bulk_in(self.wtx, &schema, rows, OnConflict::Error)
    }

    /// See [`crate::relational::RelTable::insert_with_pk`].
    pub fn insert_with_pk(&mut self, pk: PropValue, values: Properties) -> Result<(), BknError> {
        self.insert_with_pk_bulk([(pk, values)])
    }

    /// See [`crate::relational::RelTable::insert_with_pk_bulk`].
    pub fn insert_with_pk_bulk(&mut self, rows: impl IntoIterator<Item = (PropValue, Properties)>) -> Result<(), BknError> {
        let schema = self.resolved()?;
        insert_with_pk_bulk_in(self.wtx, &schema, rows, OnConflict::Error)
    }

    /// See [`crate::relational::RelTable::upsert`].
    pub fn upsert(&mut self, values: Properties) -> Result<PropValue, BknError> {
        let schema = self.resolved()?;
        insert_in(self.wtx, &schema, values, OnConflict::Replace)
    }

    /// See [`crate::relational::RelTable::upsert_bulk`].
    pub fn upsert_bulk(&mut self, rows: impl IntoIterator<Item = Properties>) -> Result<Vec<PropValue>, BknError> {
        let schema = self.resolved()?;
        insert_bulk_in(self.wtx, &schema, rows, OnConflict::Replace)
    }

    /// See [`crate::relational::RelTable::upsert_with_pk`].
    pub fn upsert_with_pk(&mut self, pk: PropValue, values: Properties) -> Result<(), BknError> {
        self.upsert_with_pk_bulk([(pk, values)])
    }

    /// See [`crate::relational::RelTable::upsert_with_pk_bulk`].
    pub fn upsert_with_pk_bulk(&mut self, rows: impl IntoIterator<Item = (PropValue, Properties)>) -> Result<(), BknError> {
        let schema = self.resolved()?;
        insert_with_pk_bulk_in(self.wtx, &schema, rows, OnConflict::Replace)
    }

    pub fn get(&self, pk: &PropValue) -> Result<Option<Row>, BknError> {
        get_in(&*self.wtx, &self.resolved()?, pk)
    }

    /// Rows matching `query` (filters, ordering, paging, projection).
    pub fn find(&self, query: &Query) -> Result<Vec<Row>, BknError> {
        select_in(&*self.wtx, &self.resolved()?, query)
    }

    pub fn count(&self, query: &Query) -> Result<usize, BknError> {
        count_in(&*self.wtx, &self.resolved()?, query)
    }

    /// `GROUP BY group_by` aggregates over the rows matching `query`.
    pub fn aggregate(&self, query: &Query, group_by: &[&str], aggs: &[Agg]) -> Result<Vec<AggregateRow>, BknError> {
        let group_by: Vec<String> = group_by.iter().map(|s| s.to_string()).collect();
        aggregate_in(&*self.wtx, &self.resolved()?, query, &group_by, aggs)
    }

    /// Rows whose `column` value exactly equals `value` — via that column's
    /// secondary index when one exists, otherwise a filtered table scan.
    pub fn select_eq(&self, column: &str, value: &PropValue) -> Result<Vec<Row>, BknError> {
        self.find(&Query::new().where_eq(column, value.clone()))
    }

    pub fn select_all(&self) -> Result<Vec<Row>, BknError> {
        self.find(&Query::new())
    }

    pub fn select_range(&self, column: &str, start: &Bound<PropValue>, end: &Bound<PropValue>) -> Result<Vec<Row>, BknError> {
        self.find(&Query::new().where_range(column, start.clone(), end.clone()))
    }

    /// Rows whose `column` string value starts with `prefix`.
    pub fn select_prefix(&self, column: &str, prefix: &str) -> Result<Vec<Row>, BknError> {
        self.find(&Query::new().where_prefix(column, prefix))
    }

    /// Point update by pk; returns whether the row existed.
    pub fn update(&mut self, pk: &PropValue, mutate: impl FnOnce(&mut Properties)) -> Result<bool, BknError> {
        let schema = self.resolved()?;
        update_row_in(self.wtx, &schema, pk, mutate)
    }

    /// Applies `sets` to every row selected by `query`; returns how many changed.
    pub fn update_where(&mut self, query: &Query, sets: &[(&str, PropValue)]) -> Result<usize, BknError> {
        let schema = self.resolved()?;
        let sets: Vec<(String, PropValue)> = sets.iter().map(|(c, v)| (c.to_string(), v.clone())).collect();
        update_in(self.wtx, &schema, query, &sets)
    }

    /// Applies `sets` to every row whose `column` equals `value`.
    pub fn update_where_eq(&mut self, column: &str, value: &PropValue, sets: &[(&str, PropValue)]) -> Result<usize, BknError> {
        self.update_where(&Query::new().where_eq(column, value.clone()), sets)
    }

    /// Point delete by pk; returns whether the row existed.
    pub fn delete(&mut self, pk: &PropValue) -> Result<bool, BknError> {
        let schema = self.resolved()?;
        delete_row_in(self.wtx, &schema, pk)
    }

    /// Deletes every row selected by `query`; returns how many were removed.
    pub fn delete_where(&mut self, query: &Query) -> Result<usize, BknError> {
        let schema = self.resolved()?;
        delete_in(self.wtx, &schema, query)
    }

    /// Deletes every row whose `column` equals `value` — the cascade-delete
    /// building block for clearing a table's rows belonging to one parent id.
    pub fn delete_where_eq(&mut self, column: &str, value: &PropValue) -> Result<usize, BknError> {
        self.delete_where(&Query::new().where_eq(column, value.clone()))
    }
}

/// Borrowing sibling of [`RelWriteBatch`], for use inside a bigger,
/// already-open batch (e.g. [`crate::db::DbWriteBatch::relational`]) that
/// also touches other models over the same transaction.
pub struct RelBatchView<'s, W: StorageWriteTx> {
    wtx: &'s mut W,
}

impl<'s, W: StorageWriteTx> RelBatchView<'s, W> {
    pub(crate) fn new(wtx: &'s mut W) -> Self {
        Self { wtx }
    }

    pub fn table(&mut self, schema: impl Into<TableSchema>) -> BatchTable<'_, W> {
        BatchTable::new(self.wtx, TableRef { schema: schema.into(), by_name: false })
    }

    /// A registered table, by name.
    pub fn table_named(&mut self, name: &str) -> Result<BatchTable<'_, W>, BknError> {
        let schema = catalog::require_schema_in(&*self.wtx, name)?;
        Ok(BatchTable::new(self.wtx, TableRef { schema, by_name: true }))
    }

    /// See [`crate::relational::RelationalDb::create_table`].
    pub fn create_table(&mut self, schema: impl Into<TableSchema>) -> Result<bool, BknError> {
        catalog::create_table_in(self.wtx, &schema.into())
    }

    /// See [`crate::relational::RelationalDb::ensure_table`].
    pub fn ensure_table(&mut self, schema: impl Into<TableSchema>) -> Result<(), BknError> {
        catalog::ensure_table_in(self.wtx, &schema.into())
    }

    /// See [`crate::relational::RelationalDb::drop_table`].
    pub fn drop_table(&mut self, name: &str) -> Result<bool, BknError> {
        catalog::drop_table_in(self.wtx, name)
    }

    pub fn table_schema(&self, name: &str) -> Result<Option<TableSchema>, BknError> {
        catalog::load_schema_in(&*self.wtx, name)
    }

    pub fn list_tables(&self) -> Result<Vec<TableSchema>, BknError> {
        catalog::list_schemas_in(&*self.wtx)
    }
}

/// A read-only view of a relational table over an open [`StorageReadTx`].
pub struct ReadTable<'s, R: StorageReadTx> {
    rtx: &'s R,
    table: TableRef,
}

impl<'s, R: StorageReadTx> ReadTable<'s, R> {
    pub(crate) fn new(rtx: &'s R, table: TableRef) -> Self {
        crate::check_table_name(table.schema.name()).unwrap_or_else(|e| panic!("{e}"));
        Self { rtx, table }
    }

    /// The schema this handle was created with.
    pub fn schema(&self) -> &TableSchema {
        &self.table.schema
    }

    fn resolved(&self) -> Result<TableSchema, BknError> {
        self.table.resolve(self.rtx)
    }

    pub fn get(&self, pk: &PropValue) -> Result<Option<Row>, BknError> {
        get_in(self.rtx, &self.resolved()?, pk)
    }

    pub fn find(&self, query: &Query) -> Result<Vec<Row>, BknError> {
        select_in(self.rtx, &self.resolved()?, query)
    }

    pub fn count(&self, query: &Query) -> Result<usize, BknError> {
        count_in(self.rtx, &self.resolved()?, query)
    }

    pub fn aggregate(&self, query: &Query, group_by: &[&str], aggs: &[Agg]) -> Result<Vec<AggregateRow>, BknError> {
        let group_by: Vec<String> = group_by.iter().map(|s| s.to_string()).collect();
        aggregate_in(self.rtx, &self.resolved()?, query, &group_by, aggs)
    }

    pub fn select_eq(&self, column: &str, value: &PropValue) -> Result<Vec<Row>, BknError> {
        self.find(&Query::new().where_eq(column, value.clone()))
    }

    pub fn select_all(&self) -> Result<Vec<Row>, BknError> {
        self.find(&Query::new())
    }

    pub fn select_range(&self, column: &str, start: &Bound<PropValue>, end: &Bound<PropValue>) -> Result<Vec<Row>, BknError> {
        self.find(&Query::new().where_range(column, start.clone(), end.clone()))
    }

    /// Rows whose `column` string value starts with `prefix`.
    pub fn select_prefix(&self, column: &str, prefix: &str) -> Result<Vec<Row>, BknError> {
        self.find(&Query::new().where_prefix(column, prefix))
    }
}

/// A read-only multi-table relational view over an open [`StorageReadTx`].
pub struct RelReadView<'s, R: StorageReadTx> {
    rtx: &'s R,
}

impl<'s, R: StorageReadTx> RelReadView<'s, R> {
    pub(crate) fn new(rtx: &'s R) -> Self {
        Self { rtx }
    }

    pub fn table(&self, schema: impl Into<TableSchema>) -> ReadTable<'s, R> {
        ReadTable::new(self.rtx, TableRef { schema: schema.into(), by_name: false })
    }

    /// A registered table, by name.
    pub fn table_named(&self, name: &str) -> Result<ReadTable<'s, R>, BknError> {
        let schema = catalog::require_schema_in(self.rtx, name)?;
        Ok(ReadTable::new(self.rtx, TableRef { schema, by_name: true }))
    }

    pub fn table_schema(&self, name: &str) -> Result<Option<TableSchema>, BknError> {
        catalog::load_schema_in(self.rtx, name)
    }

    pub fn list_tables(&self) -> Result<Vec<TableSchema>, BknError> {
        catalog::list_schemas_in(self.rtx)
    }
}
