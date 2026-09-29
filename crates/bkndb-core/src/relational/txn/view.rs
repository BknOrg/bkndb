//! Multi-table views over an open transaction: [`RelBatchView`](super::RelBatchView), [`ReadTable`](super::ReadTable), [`RelReadView`](super::RelReadView).
use super::*;

/// Borrowing sibling of [`RelWriteBatch`], for use inside a bigger,
/// already-open batch (e.g. [`crate::db::DbWriteBatch::relational`]) that
/// also touches other models over the same transaction.
pub struct RelBatchView<'s, W: StorageWriteTx> {
    pub(super) wtx: &'s mut W,
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

    /// See [`crate::relational::RelationalDb::create_fulltext_index`].
    #[cfg(feature = "search")]
    pub fn create_fulltext_index(&mut self, table: &str, column: &str) -> Result<bool, BknError> {
        crate::relational::search::create_fulltext_index_in(self.wtx, table, column)
    }

    /// See [`crate::relational::RelationalDb::drop_fulltext_index`].
    #[cfg(feature = "search")]
    pub fn drop_fulltext_index(&mut self, table: &str, column: &str) -> Result<bool, BknError> {
        crate::relational::search::drop_fulltext_index_in(self.wtx, table, column)
    }

    /// See [`crate::relational::RelationalDb::fulltext_indexes`].
    #[cfg(feature = "search")]
    pub fn fulltext_indexes(&self, table: &str) -> Result<Vec<String>, BknError> {
        crate::relational::search::fulltext_columns_in(&*self.wtx, table)
    }

    /// See [`crate::relational::RelationalDb::search_text`] (sees this
    /// transaction's writes).
    #[cfg(feature = "search")]
    pub fn search_text(
        &self,
        table: &str,
        column: &str,
        query: &str,
        limit: usize,
        match_all: bool,
        filter: Option<&crate::relational::Expr>,
    ) -> Result<Vec<crate::relational::ScoredRow>, BknError> {
        let schema = catalog::require_schema_in(&*self.wtx, table)?;
        crate::relational::search::search_text_in(&*self.wtx, &schema, column, query, limit, match_all, filter)
    }

    /// See [`crate::relational::RelationalDb::search_vector`].
    #[cfg(feature = "search")]
    pub fn search_vector(
        &self,
        table: &str,
        column: &str,
        query: &[f32],
        limit: usize,
        metric: crate::relational::VectorMetric,
        filter: Option<&crate::relational::Expr>,
    ) -> Result<Vec<crate::relational::ScoredRow>, BknError> {
        self.search_vector_with(table, column, query, limit, metric, filter, Default::default())
    }

    /// See [`crate::relational::RelationalDb::search_vector_with`].
    #[cfg(feature = "search")]
    #[allow(clippy::too_many_arguments)]
    pub fn search_vector_with(
        &self,
        table: &str,
        column: &str,
        query: &[f32],
        limit: usize,
        metric: crate::relational::VectorMetric,
        filter: Option<&crate::relational::Expr>,
        options: crate::relational::VectorSearchOptions,
    ) -> Result<Vec<crate::relational::ScoredRow>, BknError> {
        let schema = catalog::require_schema_in(&*self.wtx, table)?;
        crate::relational::search::search_vector_in(&*self.wtx, &schema, column, query, limit, metric, filter, options)
    }

    /// See [`crate::relational::RelationalDb::create_vector_index`].
    #[cfg(feature = "search")]
    pub fn create_vector_index(
        &mut self,
        table: &str,
        column: &str,
        options: crate::relational::VectorIndexOptions,
    ) -> Result<bool, BknError> {
        crate::relational::ann::create_in(self.wtx, table, column, options)
    }

    /// See [`crate::relational::RelationalDb::drop_vector_index`].
    #[cfg(feature = "search")]
    pub fn drop_vector_index(&mut self, table: &str, column: &str) -> Result<bool, BknError> {
        crate::relational::ann::drop_in(self.wtx, table, column)
    }

    /// See [`crate::relational::RelationalDb::vector_indexes`].
    #[cfg(feature = "search")]
    pub fn vector_indexes(&self, table: &str) -> Result<Vec<crate::relational::VectorIndexInfo>, BknError> {
        crate::relational::ann::list_in(&*self.wtx, table)
    }

    /// Runs one SQL statement inside this transaction (reads see its
    /// pending writes). See [`crate::lang::sql`].
    pub fn sql(&mut self, sql: &str, params: impl Into<crate::lang::Params>) -> Result<crate::lang::sql::SqlOutput, BknError> {
        crate::lang::sql::execute(self.wtx, &crate::lang::sql::parse(sql)?, &params.into())
    }
}

/// A read-only view of a relational table over an open [`StorageReadTx`].
pub struct ReadTable<'s, R: StorageReadTx> {
    pub(super) rtx: &'s R,
    pub(super) table: TableRef,
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

    pub(super) fn resolved(&self) -> Result<TableSchema, BknError> {
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
    pub(super) rtx: &'s R,
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

    /// See [`crate::relational::RelationalDb::fulltext_indexes`].
    #[cfg(feature = "search")]
    pub fn fulltext_indexes(&self, table: &str) -> Result<Vec<String>, BknError> {
        crate::relational::search::fulltext_columns_in(self.rtx, table)
    }

    /// See [`crate::relational::RelationalDb::search_text`].
    #[cfg(feature = "search")]
    pub fn search_text(
        &self,
        table: &str,
        column: &str,
        query: &str,
        limit: usize,
        match_all: bool,
        filter: Option<&crate::relational::Expr>,
    ) -> Result<Vec<crate::relational::ScoredRow>, BknError> {
        let schema = catalog::require_schema_in(self.rtx, table)?;
        crate::relational::search::search_text_in(self.rtx, &schema, column, query, limit, match_all, filter)
    }

    /// See [`crate::relational::RelationalDb::search_vector`].
    #[cfg(feature = "search")]
    pub fn search_vector(
        &self,
        table: &str,
        column: &str,
        query: &[f32],
        limit: usize,
        metric: crate::relational::VectorMetric,
        filter: Option<&crate::relational::Expr>,
    ) -> Result<Vec<crate::relational::ScoredRow>, BknError> {
        self.search_vector_with(table, column, query, limit, metric, filter, Default::default())
    }

    /// See [`crate::relational::RelationalDb::search_vector_with`].
    #[cfg(feature = "search")]
    #[allow(clippy::too_many_arguments)]
    pub fn search_vector_with(
        &self,
        table: &str,
        column: &str,
        query: &[f32],
        limit: usize,
        metric: crate::relational::VectorMetric,
        filter: Option<&crate::relational::Expr>,
        options: crate::relational::VectorSearchOptions,
    ) -> Result<Vec<crate::relational::ScoredRow>, BknError> {
        let schema = catalog::require_schema_in(self.rtx, table)?;
        crate::relational::search::search_vector_in(self.rtx, &schema, column, query, limit, metric, filter, options)
    }

    /// See [`crate::relational::RelationalDb::vector_indexes`].
    #[cfg(feature = "search")]
    pub fn vector_indexes(&self, table: &str) -> Result<Vec<crate::relational::VectorIndexInfo>, BknError> {
        crate::relational::ann::list_in(self.rtx, table)
    }

    /// Runs one read-only SQL statement (a `SELECT`) against this snapshot.
    pub fn sql(&self, sql: &str, params: impl Into<crate::lang::Params>) -> Result<crate::lang::sql::SqlOutput, BknError> {
        crate::lang::sql::execute_read(self.rtx, &crate::lang::sql::parse(sql)?, &params.into())
    }
}
