//! Persistent schema catalog: every table created through
//! [`crate::relational::RelationalDb::create_table`] /
//! [`crate::relational::RelationalDb::ensure_table`] has its [`TableSchema`]
//! stored in the shared `meta` table, so it can be listed, looked up by name
//! (e.g. from language bindings that have no compile-time schema), migrated
//! and dropped. Tables used only through a static `RelSchema` keep working
//! without ever being registered.
use std::collections::HashSet;
use std::ops::Bound;

use crate::relational::codec::{
    base_table, catalog_key, decode_table_def, encode_row, encode_table_def, index_key, index_table, meta_table,
    prefix_upper_bound, row_counter_key, sortable_encode, CATALOG_PREFIX,
};
use crate::relational::db::{scan_all_in, validate_row, Row};
use crate::relational::schema::TableSchema;
use crate::value::PropValue;
use crate::{BknError, StorageReadTx, StorageWriteTx, TableSpec};

pub(crate) fn load_schema_in<R: StorageReadTx>(rtx: &R, name: &str) -> Result<Option<TableSchema>, BknError> {
    match rtx.get(meta_table(), &catalog_key(name))? {
        Some(bytes) => Ok(Some(TableSchema::from_def(decode_table_def(&bytes)?))),
        None => Ok(None),
    }
}

pub(crate) fn require_schema_in<R: StorageReadTx>(rtx: &R, name: &str) -> Result<TableSchema, BknError> {
    crate::check_table_name(name)?;
    load_schema_in(rtx, name)?.ok_or_else(|| BknError::TableNotFound(name.to_string()))
}

pub(crate) fn list_schemas_in<R: StorageReadTx>(rtx: &R) -> Result<Vec<TableSchema>, BknError> {
    let end = prefix_upper_bound(CATALOG_PREFIX).expect("catalog prefix is not all 0xFF");
    rtx.range(meta_table(), Bound::Included(CATALOG_PREFIX), Bound::Excluded(&end))?
        .into_iter()
        .map(|(_, v)| Ok(TableSchema::from_def(decode_table_def(&v)?)))
        .collect()
}

/// Registers `schema`. Returns `false` (and changes nothing) if an identical
/// definition is already registered; errors if a *different* one is.
pub(crate) fn create_table_in<W: StorageWriteTx>(wtx: &mut W, schema: &TableSchema) -> Result<bool, BknError> {
    crate::check_table_name(schema.name())?;
    match load_schema_in(wtx, schema.name())? {
        Some(existing) if existing == *schema => Ok(false),
        Some(_) => Err(mismatch(
            schema,
            "table already exists with a different definition (use ensure_table to migrate it)".to_string(),
        )),
        None => {
            adopt_in(wtx, schema)?;
            Ok(true)
        }
    }
}

/// Makes the stored table match `schema`: creates it if absent, otherwise
/// migrates existing rows and indexes to the new definition. Atomic with the
/// caller's transaction, so a failed migration changes nothing.
pub(crate) fn ensure_table_in<W: StorageWriteTx>(wtx: &mut W, schema: &TableSchema) -> Result<(), BknError> {
    crate::check_table_name(schema.name())?;
    match load_schema_in(wtx, schema.name())? {
        None => adopt_in(wtx, schema),
        Some(old) if old == *schema => Ok(()),
        Some(old) => migrate_in(wtx, &old, schema),
    }
}

/// Removes a registered table: rows, indexes, pk counter and catalog entry.
/// Returns `false` if no such table is registered.
pub(crate) fn drop_table_in<W: StorageWriteTx>(wtx: &mut W, name: &str) -> Result<bool, BknError> {
    crate::check_table_name(name)?;
    let Some(schema) = load_schema_in(wtx, name)? else {
        return Ok(false);
    };
    clear_table(wtx, base_table(&schema))?;
    for col in schema.indexed_columns() {
        clear_table(wtx, index_table(&schema, col))?;
    }
    wtx.delete(meta_table(), &row_counter_key(name))?;
    wtx.delete(meta_table(), &catalog_key(name))?;
    Ok(true)
}

fn mismatch(schema: &TableSchema, message: String) -> BknError {
    BknError::SchemaMismatch { table: schema.name().to_string(), message }
}

fn store_def<W: StorageWriteTx>(wtx: &mut W, schema: &TableSchema) -> Result<(), BknError> {
    wtx.put(meta_table(), &catalog_key(schema.name()), &encode_table_def(schema.def())?)
}

/// Registers a table not yet in the catalog. Rows may already exist (a table
/// previously used only through a static `RelSchema`); their indexes are
/// rebuilt, which also repairs indexes added to a static schema after rows
/// were written (those were never backfilled).
fn adopt_in<W: StorageWriteTx>(wtx: &mut W, schema: &TableSchema) -> Result<(), BknError> {
    let rows = scan_all_in(wtx, schema)?;
    if !rows.is_empty() {
        for col in schema.indexed_columns() {
            rebuild_index(wtx, schema, col, &rows)?;
        }
    }
    store_def(wtx, schema)
}

fn migrate_in<W: StorageWriteTx>(wtx: &mut W, old: &TableSchema, new: &TableSchema) -> Result<(), BknError> {
    if old.primary_key() != new.primary_key()
        || old.primary_key_column().kind != new.primary_key_column().kind
        || old.auto_increment_pk() != new.auto_increment_pk()
    {
        return Err(mismatch(new, "the primary key and auto-increment setting cannot be changed".to_string()));
    }
    for c in new.columns() {
        if let Some(o) = old.column(&c.name)
            && o.kind != c.kind
        {
            return Err(mismatch(new, format!("column '{}' cannot change kind from {:?} to {:?}", c.name, o.kind, c.kind)));
        }
    }

    // Rewrite rows: drop removed columns, fill defaults of added ones, and
    // re-validate against the new constraints (e.g. a column made NOT NULL).
    let mut rows = scan_all_in(wtx, old)?;
    for row in &mut rows {
        let before = row.values.clone();
        row.values.retain(|k, _| new.column(k).is_some());
        for c in new.columns() {
            if old.column(&c.name).is_none()
                && !row.values.contains_key(&c.name)
                && let Some(d) = &c.default
            {
                row.values.insert(c.name.clone(), d.clone());
            }
        }
        validate_row(new, &row.pk, &row.values)?;
        if row.values != before {
            wtx.put(base_table(new), &sortable_encode(&row.pk)?, &encode_row(&row.values)?)?;
        }
    }

    for col in old.indexed_columns() {
        if !new.is_indexed(col) {
            clear_table(wtx, index_table(old, col))?;
        }
    }
    for col in new.indexed_columns() {
        let newly_unique = new.column(col).is_some_and(|c| c.unique) && !old.column(col).is_some_and(|c| c.unique);
        if !old.is_indexed(col) || newly_unique {
            rebuild_index(wtx, new, col, &rows)?;
        }
    }
    store_def(wtx, new)
}

/// Rewrites one column's index from scratch over `rows`, enforcing UNIQUE.
fn rebuild_index<W: StorageWriteTx>(wtx: &mut W, schema: &TableSchema, col: &str, rows: &[Row]) -> Result<(), BknError> {
    let table = index_table(schema, col);
    clear_table(wtx, table)?;
    let unique = schema.column(col).is_some_and(|c| c.unique);
    let mut seen = HashSet::new();
    for row in rows {
        let Some(v) = row.get(schema, col).filter(|v| !matches!(v, PropValue::Null)) else {
            continue;
        };
        if unique && !seen.insert(sortable_encode(v)?) {
            return Err(BknError::ConstraintViolation {
                table: schema.name().to_string(),
                message: format!("cannot make '{col}' UNIQUE: value {v:?} appears more than once"),
            });
        }
        wtx.put(table, &index_key(v, &row.pk)?, &[])?;
    }
    Ok(())
}

fn clear_table<W: StorageWriteTx>(wtx: &mut W, table: TableSpec) -> Result<(), BknError> {
    for (k, _) in wtx.range(table, Bound::Unbounded, Bound::Unbounded)? {
        wtx.delete(table, &k)?;
    }
    Ok(())
}
