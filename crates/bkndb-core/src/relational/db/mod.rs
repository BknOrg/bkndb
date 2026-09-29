use std::ops::Bound;
use std::sync::Arc;

use crate::relational::catalog;
use crate::relational::codec::{
    base_table, bump_pk_counter_past, decode_row, decode_sortable, encode_row, index_key,
    index_table, lower_bound_bytes, pk_from_index_key, reserve_pks, sortable_encode,
    sortable_str_prefix_bounds, upper_bound_bytes,
};
use crate::relational::query::{DeleteQuery, SelectQuery, UpdateQuery};
use crate::relational::schema::{ColumnKind, HasPrimaryKey, TableSchema};
use crate::value::{PropValue, Properties};
use crate::{BknError, StorageBackend, StorageReadTx, StorageWriteTx};

mod table;
mod write;
mod read;

pub use table::*;
pub(crate) use write::*;
pub(crate) use read::*;

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub pk: PropValue,
    pub values: Properties,
}

impl Row {
    /// Looks up a column's value by name, transparently resolving the
    /// primary-key column to `self.pk` — the row's stored `values` never
    /// contains the pk column (it lives in the row key, not the value), so
    /// callers that don't special-case it would otherwise always miss it.
    /// Accepts either a static `RelSchema` or a `TableSchema`.
    pub fn get<S: HasPrimaryKey + ?Sized>(&self, schema: &S, column: &str) -> Option<&PropValue> {
        if column == schema.primary_key_name() {
            Some(&self.pk)
        } else {
            self.values.get(column)
        }
    }
}

/// Relational (row/column) facade layered on top of a raw [`StorageBackend`],
/// sharing the same backend instance a [`crate::graph::GraphDb`] can use —
/// that shared `Arc<B>` is what makes the two layers "hybrid" rather than
/// two unrelated databases.
pub struct RelationalDb<B: StorageBackend> {
    pub(crate) backend: Arc<B>,
}

impl<B: StorageBackend> RelationalDb<B> {
    pub fn new(backend: B) -> Self {
        Self::from_arc(Arc::new(backend))
    }

    pub fn from_arc(backend: Arc<B>) -> Self {
        Self { backend }
    }

    /// A handle on the table described by `schema` (a `&RelSchema` static or
    /// a [`TableSchema`]); the table doesn't have to be registered in the
    /// catalog.
    ///
    /// Panics if the name collides with one of `bkndb-core`'s reserved
    /// internal table names (see [`crate::RESERVED_TABLE_NAMES`]) — a
    /// schema-authoring bug caught at first use, not a recoverable runtime
    /// condition. (A [`TableSchema`] built with its builder can never have a
    /// reserved name.)
    pub fn table(&self, schema: impl Into<TableSchema>) -> RelTable<'_, B> {
        let schema = schema.into();
        crate::check_table_name(schema.name()).unwrap_or_else(|e| panic!("{e}"));
        RelTable {
            db: self,
            table: TableRef {
                schema,
                by_name: false,
            },
        }
    }

    /// A handle on a table registered in the catalog, by name. The handle
    /// always uses the table's current definition, so it stays valid across
    /// migrations.
    pub fn table_named(&self, name: &str) -> Result<RelTable<'_, B>, BknError> {
        let rtx = self.backend.begin_read()?;
        let schema = catalog::require_schema_in(&rtx, name)?;
        Ok(RelTable {
            db: self,
            table: TableRef {
                schema,
                by_name: true,
            },
        })
    }

    /// Registers a new table. Returns `false` if an identical definition was
    /// already registered; errors if a different one is (see
    /// [`RelationalDb::ensure_table`] for migrating).
    pub fn create_table(&self, schema: impl Into<TableSchema>) -> Result<bool, BknError> {
        let schema = schema.into();
        self.write(|wtx| catalog::create_table_in(wtx, &schema))
    }

    /// Creates the table if needed, or migrates the registered one to match
    /// `schema`: added/removed columns (defaults are backfilled into existing
    /// rows), changed NOT NULL / UNIQUE constraints (validated against
    /// existing rows), and added/removed indexes (new ones are backfilled).
    /// The primary key and existing columns' kinds can't change. Atomic.
    ///
    /// Also the way to adopt a table previously used only through a static
    /// `RelSchema`: its indexes are rebuilt from the existing rows.
    pub fn ensure_table(&self, schema: impl Into<TableSchema>) -> Result<(), BknError> {
        let schema = schema.into();
        self.write(|wtx| catalog::ensure_table_in(wtx, &schema))
    }

    /// Deletes a registered table with all its rows and indexes. Returns
    /// `false` if no such table is registered.
    pub fn drop_table(&self, name: &str) -> Result<bool, BknError> {
        self.write(|wtx| catalog::drop_table_in(wtx, name))
    }

    /// Adds a (backfilled) secondary index to a registered table.
    pub fn create_index(&self, table: &str, column: &str) -> Result<(), BknError> {
        self.write(|wtx| {
            let schema = catalog::require_schema_in(wtx, table)?
                .to_builder()
                .index(column)
                .build()?;
            catalog::ensure_table_in(wtx, &schema)
        })
    }

    /// Removes a secondary index (and any UNIQUE constraint relying on it).
    pub fn drop_index(&self, table: &str, column: &str) -> Result<(), BknError> {
        self.write(|wtx| {
            let schema = catalog::require_schema_in(wtx, table)?
                .to_builder()
                .drop_index(column)
                .build()?;
            catalog::ensure_table_in(wtx, &schema)
        })
    }

    pub fn table_schema(&self, name: &str) -> Result<Option<TableSchema>, BknError> {
        catalog::load_schema_in(&self.backend.begin_read()?, name)
    }

    /// Runs one SQL statement (see [`crate::lang::sql`] for the supported
    /// subset): a query in a read snapshot, anything else in its own atomic
    /// write transaction.
    ///
    /// ```ignore
    /// db.sql("SELECT name FROM users WHERE age >= ? ORDER BY name", vec![18.into()])?;
    /// ```
    pub fn sql(&self, sql: &str, params: impl Into<crate::lang::Params>) -> Result<crate::lang::sql::SqlOutput, BknError> {
        let stmt = crate::lang::sql::parse(sql)?;
        let params = params.into();
        if stmt.is_read_only() {
            crate::lang::sql::execute_read(&self.backend.begin_read()?, &stmt, &params)
        } else {
            self.write(|wtx| crate::lang::sql::execute(wtx, &stmt, &params))
        }
    }

    /// Every table registered in the catalog, sorted by name.
    pub fn list_tables(&self) -> Result<Vec<TableSchema>, BknError> {
        catalog::list_schemas_in(&self.backend.begin_read()?)
    }

    pub(crate) fn write<R>(
        &self,
        f: impl for<'w> FnOnce(&mut B::WriteTx<'w>) -> Result<R, BknError>,
    ) -> Result<R, BknError> {
        let mut wtx = self.backend.begin_write()?;
        let r = f(&mut wtx)?;
        wtx.commit()?;
        Ok(r)
    }

    /// Runs `f` against one shared write transaction spanning however many
    /// tables it touches (via [`crate::relational::txn::RelWriteBatch::table`]),
    /// committing only if `f` returns `Ok`. An `Err` return leaves the write
    /// tx uncommitted and it is simply dropped — every [`StorageWriteTx`]
    /// backend buffers writes until `commit()`, so a dropped, uncommitted tx
    /// is already a no-op rollback with no extra bookkeeping needed here.
    pub fn write_tx<F, R>(&self, f: F) -> Result<R, BknError>
    where
        F: for<'w> FnOnce(
            &mut crate::relational::txn::RelWriteBatch<B::WriteTx<'w>>,
        ) -> Result<R, BknError>,
    {
        let wtx = self.backend.begin_write()?;
        let mut batch = crate::relational::txn::RelWriteBatch::new(wtx);
        let result = f(&mut batch)?;
        batch.into_inner().commit()?;
        Ok(result)
    }
}
