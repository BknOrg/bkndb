//! Relational methods of [`BknDbEngine`](super::BknDbEngine): schemas and rows.
use super::*;

#[uniffi::export]
impl BknDbEngine {
    // ---- relational: schema ----

    /// Registers a table. Returns `false` if an identical definition already
    /// exists; fails if a different one does (use `ensure_table` to migrate).
    pub fn create_table(&self, schema: FfiTableSchema) -> Result<bool, FfiBknError> {
        let schema = TableSchema::try_from(schema)?;
        write!(self, |b| ops::create_table(b, schema))
    }

    /// Creates the table, or migrates the existing one to `schema` (columns,
    /// constraints and indexes; existing rows are backfilled/validated).
    pub fn ensure_table(&self, schema: FfiTableSchema) -> Result<(), FfiBknError> {
        let schema = TableSchema::try_from(schema)?;
        write!(self, |b| ops::ensure_table(b, schema))
    }

    /// Deletes a table and all its rows; returns whether it existed.
    pub fn drop_table(&self, name: String) -> Result<bool, FfiBknError> {
        write!(self, |b| ops::drop_table(b, &name))
    }

    /// Adds a (backfilled) secondary index.
    pub fn create_index(&self, table: String, column: String) -> Result<(), FfiBknError> {
        write!(self, |b| ops::set_index(b, &table, &column, true))
    }

    pub fn drop_index(&self, table: String, column: String) -> Result<(), FfiBknError> {
        write!(self, |b| ops::set_index(b, &table, &column, false))
    }

    pub fn list_tables(&self) -> Result<Vec<FfiTableSchema>, FfiBknError> {
        read!(self, |r| ops::list_tables(r))?.iter().map(FfiTableSchema::try_from).collect()
    }

    pub fn table_schema(&self, name: String) -> Result<Option<FfiTableSchema>, FfiBknError> {
        read!(self, |r| ops::table_schema(r, &name))?.as_ref().map(FfiTableSchema::try_from).transpose()
    }

    // ---- relational: rows ----

    /// Inserts a row; returns its primary key (generated for auto-increment
    /// tables). Fails with `DuplicateKey` if the key is taken.
    pub fn insert(&self, table: String, values: HashMap<String, FfiPropValue>) -> Result<FfiPropValue, FfiBknError> {
        write!(self, |b| ops::insert(b, &table, values, false)).map(Into::into)
    }

    pub fn insert_many(&self, table: String, rows: Vec<HashMap<String, FfiPropValue>>) -> Result<Vec<FfiPropValue>, FfiBknError> {
        Ok(write!(self, |b| ops::insert_many(b, &table, rows, false))?.into_iter().map(Into::into).collect())
    }

    /// Inserts a row or replaces the one with the same primary key.
    pub fn upsert(&self, table: String, values: HashMap<String, FfiPropValue>) -> Result<FfiPropValue, FfiBknError> {
        write!(self, |b| ops::insert(b, &table, values, true)).map(Into::into)
    }

    pub fn upsert_many(&self, table: String, rows: Vec<HashMap<String, FfiPropValue>>) -> Result<Vec<FfiPropValue>, FfiBknError> {
        Ok(write!(self, |b| ops::insert_many(b, &table, rows, true))?.into_iter().map(Into::into).collect())
    }

    pub fn get_row(&self, table: String, pk: FfiPropValue) -> Result<Option<FfiRow>, FfiBknError> {
        let pk = pk.into();
        read!(self, |r| ops::get_row(r, &table, pk))
    }

    pub fn select(&self, table: String, query: FfiQuery) -> Result<Vec<FfiRow>, FfiBknError> {
        let query = Query::try_from(query)?;
        read!(self, |r| ops::select(r, &table, &query))
    }

    pub fn count(&self, table: String, query: FfiQuery) -> Result<u64, FfiBknError> {
        let query = Query::try_from(query)?;
        read!(self, |r| ops::count(r, &table, &query))
    }

    /// `GROUP BY group_by` aggregates over the rows matching `query`. With no
    /// `group_by`, returns exactly one row.
    pub fn aggregate(
        &self,
        table: String,
        query: FfiQuery,
        group_by: Vec<String>,
        aggregates: Vec<FfiAgg>,
    ) -> Result<Vec<FfiAggregateRow>, FfiBknError> {
        let query = Query::try_from(query)?;
        let aggs = aggregates.into_iter().map(Agg::try_from).collect::<Result<Vec<_>, _>>()?;
        read!(self, |r| ops::aggregate(r, &table, &query, &group_by, &aggs))
    }

    /// Sets the columns in `set` on every row matching `query`; returns how
    /// many rows changed.
    pub fn update_rows(&self, table: String, query: FfiQuery, set: HashMap<String, FfiPropValue>) -> Result<u64, FfiBknError> {
        let query = Query::try_from(query)?;
        write!(self, |b| ops::update_rows(b, &table, &query, set))
    }

    /// Deletes every row matching `query`; returns how many were removed.
    pub fn delete_rows(&self, table: String, query: FfiQuery) -> Result<u64, FfiBknError> {
        let query = Query::try_from(query)?;
        write!(self, |b| ops::delete_rows(b, &table, &query))
    }

}
