//! [`RelTable`](super::RelTable): a table handle running each call in its own transaction.
use super::*;

/// A table handle's schema plus how it was obtained. Every operation
/// resolves it against the catalog inside its own transaction, so a handle
/// can never act on a stale definition (e.g. query an index a migration
/// has since dropped):
///
/// - obtained by name → always the catalog's current definition;
/// - given an explicit schema for a *registered* table → that schema must
///   equal the catalog's, else [`BknError::SchemaMismatch`];
/// - given an explicit schema for an unregistered table → used as is (the
///   classic static-`RelSchema` mode).
#[derive(Debug, Clone)]
pub(crate) struct TableRef {
    pub(crate) schema: TableSchema,
    pub(crate) by_name: bool,
}

impl TableRef {
    pub(crate) fn resolve<R: StorageReadTx>(&self, rtx: &R) -> Result<TableSchema, BknError> {
        match catalog::load_schema_in(rtx, self.schema.name())? {
            Some(current) if self.by_name || current == self.schema => Ok(current),
            Some(_) => Err(BknError::SchemaMismatch {
                table: self.schema.name().to_string(),
                message: "the table is registered with a different definition; use table_named() or ensure_table() first"
                    .to_string(),
            }),
            None if self.by_name => Err(BknError::TableNotFound(self.schema.name().to_string())),
            None => Ok(self.schema.clone()),
        }
    }
}

/// Resolves an explicitly supplied schema against the catalog (see [`TableRef`]).
#[cfg_attr(not(feature = "graph"), allow(dead_code))]
pub(crate) fn resolve_schema<R: StorageReadTx>(
    rtx: &R,
    schema: TableSchema,
) -> Result<TableSchema, BknError> {
    TableRef {
        schema,
        by_name: false,
    }
    .resolve(rtx)
}

/// One table of a [`RelationalDb`]. Every method runs (and commits) its own
/// transaction; use [`RelationalDb::write_tx`] or `Db::write_tx` to group
/// several operations atomically.
pub struct RelTable<'a, B: StorageBackend> {
    pub(crate) db: &'a RelationalDb<B>,
    pub(crate) table: TableRef,
}

// Manual impl (not `#[derive]`): a derive would require `B: Clone`.
impl<'a, B: StorageBackend> Clone for RelTable<'a, B> {
    fn clone(&self) -> Self {
        Self {
            db: self.db,
            table: self.table.clone(),
        }
    }
}

impl<'a, B: StorageBackend> RelTable<'a, B> {
    /// The schema this handle was created with (for a handle from
    /// [`RelationalDb::table_named`], the definition at that time —
    /// operations always use the current one).
    pub fn schema(&self) -> &TableSchema {
        &self.table.schema
    }

    pub(super) fn write<R>(
        &self,
        f: impl for<'w> FnOnce(&mut B::WriteTx<'w>, &TableSchema) -> Result<R, BknError>,
    ) -> Result<R, BknError> {
        self.db.write(|wtx| {
            let schema = self.table.resolve(wtx)?;
            f(wtx, &schema)
        })
    }

    /// Inserts a new row. For an auto-increment schema, `values` should not
    /// include the primary-key column (it's allocated here); the generated
    /// PK is returned. For a non-auto-increment schema, `values` must
    /// include the primary-key column — use [`RelTable::insert_with_pk`]
    /// instead if the PK comes from elsewhere (e.g. an existing `NodeId`).
    /// Fails with [`BknError::DuplicateKey`] if the pk is taken.
    pub fn insert(&self, values: Properties) -> Result<PropValue, BknError> {
        self.write(|wtx, schema| insert_in(wtx, schema, values, OnConflict::Error))
    }

    /// Inserts multiple rows in a single transaction, reserving PKs in one
    /// counter update if auto-increment is enabled.
    pub fn insert_bulk(
        &self,
        rows: impl IntoIterator<Item = Properties>,
    ) -> Result<Vec<PropValue>, BknError> {
        self.write(|wtx, schema| insert_bulk_in(wtx, schema, rows, OnConflict::Error))
    }

    /// Inserts a row under an explicit, caller-supplied PK — the mechanism
    /// for linking a relational row to an existing id from elsewhere (e.g.
    /// a graph [`crate::graph::NodeId`]). Rejected on an auto-increment
    /// schema, where PKs must come from [`RelTable::insert`] instead.
    pub fn insert_with_pk(&self, pk: PropValue, values: Properties) -> Result<(), BknError> {
        self.insert_with_pk_bulk([(pk, values)])
    }

    /// Inserts multiple rows with caller-supplied PKs in a single transaction.
    pub fn insert_with_pk_bulk(
        &self,
        rows: impl IntoIterator<Item = (PropValue, Properties)>,
    ) -> Result<(), BknError> {
        self.write(|wtx, schema| insert_with_pk_bulk_in(wtx, schema, rows, OnConflict::Error))
    }

    /// Like [`RelTable::insert`], but replaces an existing row with the same
    /// pk instead of failing. On an auto-increment schema, a row without a
    /// pk gets a fresh one and a row with a pk is upserted under it.
    pub fn upsert(&self, values: Properties) -> Result<PropValue, BknError> {
        self.write(|wtx, schema| insert_in(wtx, schema, values, OnConflict::Replace))
    }

    pub fn upsert_bulk(
        &self,
        rows: impl IntoIterator<Item = Properties>,
    ) -> Result<Vec<PropValue>, BknError> {
        self.write(|wtx, schema| insert_bulk_in(wtx, schema, rows, OnConflict::Replace))
    }

    /// Inserts the row, or replaces the existing row with the same PK
    /// (keeping every secondary index consistent). Works on any schema,
    /// including auto-increment ones.
    pub fn upsert_with_pk(&self, pk: PropValue, values: Properties) -> Result<(), BknError> {
        self.upsert_with_pk_bulk([(pk, values)])
    }

    pub fn upsert_with_pk_bulk(
        &self,
        rows: impl IntoIterator<Item = (PropValue, Properties)>,
    ) -> Result<(), BknError> {
        self.write(|wtx, schema| insert_with_pk_bulk_in(wtx, schema, rows, OnConflict::Replace))
    }

    pub fn get(&self, pk: &PropValue) -> Result<Option<Row>, BknError> {
        let rtx = self.db.backend.begin_read()?;
        get_in(&rtx, &self.table.resolve(&rtx)?, pk)
    }

    pub fn select(&self) -> SelectQuery<'a, B> {
        SelectQuery::new(self.clone())
    }

    pub fn update(&self) -> UpdateQuery<'a, B> {
        UpdateQuery::new(self.clone())
    }

    pub fn delete(&self) -> DeleteQuery<'a, B> {
        DeleteQuery::new(self.clone())
    }

    /// Rows whose `column` string value starts with `prefix`, using that
    /// column's secondary index if available, falling back to a full scan.
    pub fn select_prefix(&self, column: &str, prefix: &str) -> Result<Vec<Row>, BknError> {
        self.select().where_prefix(column, prefix).run()
    }
}

// --- Free functions parameterized over an already-open transaction ---
//
// The shared core of every `RelTable` method above (each of which is just
// "open a tx, call the matching `_in` function, commit"), of the query
// executor, and of `crate::relational::txn`'s batch API, which calls them
// directly against one caller-held transaction.
