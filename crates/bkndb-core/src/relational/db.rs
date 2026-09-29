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

    fn write<R>(
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

/// What [`write_row_in`] does when a row with the same primary key already
/// exists (including one written earlier in the same transaction).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OnConflict {
    /// Plain insert semantics: fail with [`BknError::DuplicateKey`].
    Error,
    /// Upsert semantics: replace the old row, dropping its index entries.
    Replace,
}

/// Checks a row (pk + non-pk values) against the schema: every column must
/// be declared and hold a value of its kind (or null), NOT NULL columns must
/// be present and non-null, and the pk must be non-null and of its kind.
pub(crate) fn validate_row(
    schema: &TableSchema,
    pk: &PropValue,
    values: &Properties,
) -> Result<(), BknError> {
    let table = || schema.name().to_string();
    let mismatch = |message: String| BknError::SchemaMismatch {
        table: table(),
        message,
    };
    let check = |name: &str, value: &PropValue| -> Result<(), BknError> {
        let col = schema
            .column(name)
            .ok_or_else(|| mismatch(format!("column '{name}' is not declared in the schema")))?;
        if !matches!(value, PropValue::Null) && ColumnKind::of(value) != col.kind {
            return Err(mismatch(format!(
                "column '{name}' expects {:?}, got {:?}",
                col.kind,
                ColumnKind::of(value)
            )));
        }
        Ok(())
    };
    if matches!(pk, PropValue::Null) {
        return Err(mismatch(format!(
            "primary key '{}' cannot be null",
            schema.primary_key()
        )));
    }
    check(schema.primary_key(), pk)?;
    for (name, value) in values {
        check(name, value)?;
    }
    for c in schema.columns() {
        let missing = values
            .get(&c.name)
            .is_none_or(|v| matches!(v, PropValue::Null));
        if !c.nullable && c.name != schema.primary_key() && missing {
            return Err(BknError::ConstraintViolation {
                table: table(),
                message: format!("column '{}' is NOT NULL", c.name),
            });
        }
    }
    Ok(())
}

fn pk_display(pk: &PropValue) -> String {
    match pk {
        PropValue::Int(i) => i.to_string(),
        PropValue::Str(s) => format!("{s:?}"),
        other => format!("{other:?}"),
    }
}

/// The value to index for `column`, if any: nulls are never indexed.
fn indexable<'r>(row: &'r Row, schema: &TableSchema, column: &str) -> Option<&'r PropValue> {
    row.get(schema, column)
        .filter(|v| !matches!(v, PropValue::Null))
}

/// Fails if another row already holds any of `row`'s UNIQUE column values.
/// With `changed_from`, only columns whose value differs from it are checked.
fn check_unique<R: StorageReadTx>(
    rtx: &R,
    schema: &TableSchema,
    row: &Row,
    changed_from: Option<&Row>,
) -> Result<(), BknError> {
    for c in schema.columns().iter().filter(|c| c.unique) {
        let Some(v) = indexable(row, schema, &c.name) else {
            continue;
        };
        if changed_from.is_some_and(|old| old.get(schema, &c.name) == Some(v)) {
            continue;
        }
        if index_lookup_eq_in(rtx, schema, &c.name, v)?
            .iter()
            .any(|other| other.pk != row.pk)
        {
            return Err(BknError::ConstraintViolation {
                table: schema.name().to_string(),
                message: format!("UNIQUE column '{}' already has value {v:?}", c.name),
            });
        }
    }
    Ok(())
}

pub(crate) fn write_row_in<W: StorageWriteTx>(
    wtx: &mut W,
    schema: &TableSchema,
    pk: &PropValue,
    values: &Properties,
    on_conflict: OnConflict,
) -> Result<(), BknError> {
    let mut stored = values.clone();
    stored.remove(schema.primary_key());
    for c in schema.columns() {
        if let Some(d) = &c.default
            && c.name != schema.primary_key()
            && !stored.contains_key(&c.name)
        {
            stored.insert(c.name.clone(), d.clone());
        }
    }
    validate_row(schema, pk, &stored)?;

    let row_key = sortable_encode(pk)?;
    let old = match wtx.get(base_table(schema), &row_key)? {
        Some(_) if on_conflict == OnConflict::Error => {
            return Err(BknError::DuplicateKey {
                table: schema.name().to_string(),
                key: pk_display(pk),
            });
        }
        Some(bytes) => Some(Row {
            pk: pk.clone(),
            values: decode_row(&bytes)?,
        }),
        None => None,
    };
    let row = Row {
        pk: pk.clone(),
        values: stored,
    };
    check_unique(wtx, schema, &row, None)?;

    if let Some(old) = &old {
        // Replacing: the old row's index entries must go, or index lookups
        // on its old values would keep returning this pk.
        for col in schema.indexed_columns() {
            if let Some(v) = indexable(old, schema, col) {
                wtx.delete(index_table(schema, col), &index_key(v, pk)?)?;
            }
        }
    }
    wtx.put(base_table(schema), &row_key, &encode_row(&row.values)?)?;
    for col in schema.indexed_columns() {
        if let Some(v) = indexable(&row, schema, col) {
            wtx.put(index_table(schema, col), &index_key(v, pk)?, &[])?;
        }
    }
    Ok(())
}

fn explicit_pk(schema: &TableSchema, values: &Properties) -> Option<PropValue> {
    values
        .get(schema.primary_key())
        .filter(|v| !matches!(v, PropValue::Null))
        .cloned()
}

/// Whether an insert of `values` gets a freshly allocated pk.
fn allocates_pk(schema: &TableSchema, values: &Properties, on_conflict: OnConflict) -> bool {
    schema.auto_increment_pk()
        && !(on_conflict == OnConflict::Replace && explicit_pk(schema, values).is_some())
}

pub(crate) fn insert_in<W: StorageWriteTx>(
    wtx: &mut W,
    schema: &TableSchema,
    values: Properties,
    on_conflict: OnConflict,
) -> Result<PropValue, BknError> {
    Ok(insert_bulk_in(wtx, schema, [values], on_conflict)?.remove(0))
}

pub(crate) fn insert_bulk_in<W: StorageWriteTx>(
    wtx: &mut W,
    schema: &TableSchema,
    rows: impl IntoIterator<Item = Properties>,
    on_conflict: OnConflict,
) -> Result<Vec<PropValue>, BknError> {
    let items: Vec<Properties> = rows.into_iter().collect();
    let to_allocate = items
        .iter()
        .filter(|v| allocates_pk(schema, v, on_conflict))
        .count();
    let mut next = if to_allocate > 0 {
        reserve_pks(wtx, schema, to_allocate as u64)?
    } else {
        0
    };

    let mut pks = Vec::with_capacity(items.len());
    for values in items {
        let pk = if allocates_pk(schema, &values, on_conflict) {
            next += 1;
            PropValue::Int(next - 1)
        } else {
            let pk = explicit_pk(schema, &values).ok_or_else(|| {
                BknError::Encoding(format!(
                    "missing primary key column '{}'",
                    schema.primary_key()
                ))
            })?;
            if let (true, PropValue::Int(i)) = (schema.auto_increment_pk(), &pk) {
                bump_pk_counter_past(wtx, schema, *i)?;
            }
            pk
        };
        write_row_in(wtx, schema, &pk, &values, on_conflict)?;
        pks.push(pk);
    }
    Ok(pks)
}

pub(crate) fn insert_with_pk_bulk_in<W: StorageWriteTx>(
    wtx: &mut W,
    schema: &TableSchema,
    rows: impl IntoIterator<Item = (PropValue, Properties)>,
    on_conflict: OnConflict,
) -> Result<(), BknError> {
    if schema.auto_increment_pk() && on_conflict == OnConflict::Error {
        return Err(BknError::Encoding(
            "insert_with_pk cannot be used on an auto-increment schema (use insert, or upsert_with_pk)".to_string(),
        ));
    }
    for (pk, values) in rows {
        if let (true, PropValue::Int(i)) = (schema.auto_increment_pk(), &pk) {
            bump_pk_counter_past(wtx, schema, *i)?;
        }
        write_row_in(wtx, schema, &pk, &values, on_conflict)?;
    }
    Ok(())
}

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

fn resolve_index_rows_in<R: StorageReadTx>(
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

pub(crate) fn delete_row_in<W: StorageWriteTx>(
    wtx: &mut W,
    schema: &TableSchema,
    pk: &PropValue,
) -> Result<bool, BknError> {
    let Some(row) = get_in(wtx, schema, pk)? else {
        return Ok(false);
    };
    wtx.delete(base_table(schema), &sortable_encode(pk)?)?;
    for col in schema.indexed_columns() {
        if let Some(v) = indexable(&row, schema, col) {
            wtx.delete(index_table(schema, col), &index_key(v, pk)?)?;
        }
    }
    Ok(true)
}

pub(crate) fn update_row_in<W: StorageWriteTx>(
    wtx: &mut W,
    schema: &TableSchema,
    pk: &PropValue,
    mutate: impl FnOnce(&mut Properties),
) -> Result<bool, BknError> {
    let Some(old_row) = get_in(wtx, schema, pk)? else {
        return Ok(false);
    };
    let mut new_values = old_row.values.clone();
    mutate(&mut new_values);
    new_values.remove(schema.primary_key());
    validate_row(schema, pk, &new_values)?;
    let new_row = Row {
        pk: pk.clone(),
        values: new_values,
    };
    check_unique(wtx, schema, &new_row, Some(&old_row))?;

    // Indexed columns whose value changed need their old index entry
    // removed and the new one written.
    for col in schema.indexed_columns() {
        let old = indexable(&old_row, schema, col);
        let new = indexable(&new_row, schema, col);
        if old != new {
            if let Some(old_v) = old {
                wtx.delete(index_table(schema, col), &index_key(old_v, pk)?)?;
            }
            if let Some(new_v) = new {
                wtx.put(index_table(schema, col), &index_key(new_v, pk)?, &[])?;
            }
        }
    }

    wtx.put(
        base_table(schema),
        &sortable_encode(pk)?,
        &encode_row(&new_row.values)?,
    )?;
    Ok(true)
}
