use std::ops::Bound;
use std::sync::Arc;

use crate::relational::codec::{
    base_table, decode_row, decode_sortable, encode_row, index_key, index_table,
    lower_bound_bytes, next_pk, pk_from_index_key, reserve_pks, sortable_encode,
    sortable_str_prefix_bounds, upper_bound_bytes,
};
use crate::relational::query::{DeleteQuery, SelectQuery, UpdateQuery};
use crate::relational::schema::RelSchema;
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
    pub fn get(&self, schema: &RelSchema, column: &str) -> Option<&PropValue> {
        if column == schema.primary_key {
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
    backend: Arc<B>,
}

impl<B: StorageBackend> RelationalDb<B> {
    pub fn new(backend: B) -> Self {
        Self::from_arc(Arc::new(backend))
    }

    pub fn from_arc(backend: Arc<B>) -> Self {
        Self { backend }
    }

    /// Panics if `schema.name` collides with one of `bkndb-core`'s reserved
    /// internal table names (see [`crate::RESERVED_TABLE_NAMES`]) — a
    /// schema-authoring bug caught at first use, the same tier as
    /// [`RelSchema::primary_key_column`]'s own `.expect(...)` panic, not a
    /// recoverable runtime condition worth threading a `Result` through
    /// this pervasively-called, otherwise-infallible method for.
    pub fn table<'a>(&'a self, schema: &'a RelSchema) -> RelTable<'a, B> {
        crate::check_table_name(schema.name).unwrap_or_else(|e| panic!("{e}"));
        RelTable { db: self, schema }
    }

    /// Runs `f` against one shared write transaction spanning however many
    /// tables it touches (via [`crate::relational::txn::RelWriteBatch::table`]),
    /// committing only if `f` returns `Ok`. An `Err` return leaves the write
    /// tx uncommitted and it is simply dropped — every [`StorageWriteTx`]
    /// backend buffers writes until `commit()`, so a dropped, uncommitted tx
    /// is already a no-op rollback with no extra bookkeeping needed here.
    pub fn write_tx<F, R>(&self, f: F) -> Result<R, BknError>
    where
        F: for<'w> FnOnce(&mut crate::relational::txn::RelWriteBatch<B::WriteTx<'w>>) -> Result<R, BknError>,
    {
        let wtx = self.backend.begin_write()?;
        let mut batch = crate::relational::txn::RelWriteBatch::new(wtx);
        let result = f(&mut batch)?;
        batch.into_inner().commit()?;
        Ok(result)
    }
}

pub struct RelTable<'a, B: StorageBackend> {
    pub(crate) db: &'a RelationalDb<B>,
    pub(crate) schema: &'a RelSchema,
}

// Manual impls (not `#[derive]`): `RelTable` only ever holds references, so
// it's `Copy` regardless of whether `B` itself is — a derive would
// incorrectly require `B: Clone`.
impl<'a, B: StorageBackend> Clone for RelTable<'a, B> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<'a, B: StorageBackend> Copy for RelTable<'a, B> {}

impl<'a, B: StorageBackend> RelTable<'a, B> {
    /// Inserts a new row. For an auto-increment schema, `values` should not
    /// include the primary-key column (it's allocated here); the generated
    /// PK is returned. For a non-auto-increment schema, `values` must
    /// include the primary-key column — use [`RelTable::insert_with_pk`]
    /// instead if the PK comes from elsewhere (e.g. an existing `NodeId`).
    pub fn insert(&self, values: Properties) -> Result<PropValue, BknError> {
        let mut wtx = self.db.backend.begin_write()?;
        let pk = if self.schema.auto_increment_pk {
            PropValue::Int(next_pk(&mut wtx, self.schema)?)
        } else {
            let col = self.schema.primary_key;
            values
                .get(col)
                .cloned()
                .ok_or_else(|| BknError::Encoding(format!("missing primary key column '{col}'")))?
        };
        write_row_in(&mut wtx, self.schema, &pk, &values)?;
        wtx.commit()?;
        Ok(pk)
    }

    /// Inserts multiple rows in a single batch, reserving PKs in one counter update
    /// if auto-increment is enabled.
    pub fn insert_bulk(&self, rows: impl IntoIterator<Item = Properties>) -> Result<Vec<PropValue>, BknError> {
        let mut wtx = self.db.backend.begin_write()?;
        let pks = insert_bulk_in(&mut wtx, self.schema, rows)?;
        wtx.commit()?;
        Ok(pks)
    }

    /// Inserts a row under an explicit, caller-supplied PK — the mechanism
    /// for linking a relational row to an existing id from elsewhere (e.g.
    /// a graph [`crate::graph::NodeId`]). Rejected on an auto-increment
    /// schema, where PKs must come from [`RelTable::insert`] instead.
    pub fn insert_with_pk(&self, pk: PropValue, values: Properties) -> Result<(), BknError> {
        if self.schema.auto_increment_pk {
            return Err(BknError::Encoding(
                "insert_with_pk cannot be used on an auto-increment schema".to_string(),
            ));
        }
        let mut wtx = self.db.backend.begin_write()?;
        write_row_in(&mut wtx, self.schema, &pk, &values)?;
        wtx.commit()?;
        Ok(())
    }

    /// Inserts multiple rows with caller-supplied PKs in a single batch.
    pub fn insert_with_pk_bulk(&self, rows: impl IntoIterator<Item = (PropValue, Properties)>) -> Result<(), BknError> {
        let mut wtx = self.db.backend.begin_write()?;
        insert_with_pk_bulk_in(&mut wtx, self.schema, rows)?;
        wtx.commit()?;
        Ok(())
    }

    pub fn get(&self, pk: &PropValue) -> Result<Option<Row>, BknError> {
        let rtx = self.db.backend.begin_read()?;
        get_in(&rtx, self.schema, pk)
    }

    pub fn select(&self) -> SelectQuery<'a, B> {
        SelectQuery::new(*self)
    }

    pub fn update(&self) -> UpdateQuery<'a, B> {
        UpdateQuery::new(*self)
    }

    pub fn delete(&self) -> DeleteQuery<'a, B> {
        DeleteQuery::new(*self)
    }

    /// Full base-table scan, decoding every row — the fallback path when a
    /// query has no predicate on an indexed column to narrow the search.
    pub(crate) fn scan_all(&self) -> Result<Vec<Row>, BknError> {
        let rtx = self.db.backend.begin_read()?;
        scan_all_in(&rtx, self.schema)
    }

    /// Rows whose `column` value exactly equals `value`, via that column's
    /// secondary index. Caller must have already checked `column` is
    /// indexed.
    pub(crate) fn index_lookup_eq(&self, column: &str, value: &PropValue) -> Result<Vec<Row>, BknError> {
        let rtx = self.db.backend.begin_read()?;
        index_lookup_eq_in(&rtx, self.schema, column, value)
    }

    /// Rows whose `column` value falls within `[start, end)` (per the given
    /// bound kinds), via that column's secondary index.
    pub(crate) fn index_lookup_range(
        &self,
        column: &str,
        start: &Bound<PropValue>,
        end: &Bound<PropValue>,
    ) -> Result<Vec<Row>, BknError> {
        let rtx = self.db.backend.begin_read()?;
        index_lookup_range_in(&rtx, self.schema, column, start, end)
    }

    /// Rows whose `column` string value starts with `prefix`, using that
    /// column's secondary index if available, falling back to a full scan.
    pub fn select_prefix(&self, column: &str, prefix: &str) -> Result<Vec<Row>, BknError> {
        let rtx = self.db.backend.begin_read()?;
        if self.schema.is_indexed(column) {
            index_lookup_prefix_in(&rtx, self.schema, column, prefix)
        } else {
            let rows = scan_all_in(&rtx, self.schema)?;
            Ok(rows
                .into_iter()
                .filter(|r| match r.get(self.schema, column) {
                    Some(PropValue::Str(s)) => s.starts_with(prefix),
                    _ => false,
                })
                .collect())
        }
    }

    pub(crate) fn index_lookup_prefix(&self, column: &str, prefix: &str) -> Result<Vec<Row>, BknError> {
        let rtx = self.db.backend.begin_read()?;
        index_lookup_prefix_in(&rtx, self.schema, column, prefix)
    }


    pub(crate) fn delete_row(&self, pk: &PropValue) -> Result<bool, BknError> {
        let mut wtx = self.db.backend.begin_write()?;
        let changed = delete_row_in(&mut wtx, self.schema, pk)?;
        wtx.commit()?;
        Ok(changed)
    }

    pub(crate) fn update_row(
        &self,
        pk: &PropValue,
        mutate: impl FnOnce(&mut Properties),
    ) -> Result<bool, BknError> {
        let mut wtx = self.db.backend.begin_write()?;
        let changed = update_row_in(&mut wtx, self.schema, pk, mutate)?;
        wtx.commit()?;
        Ok(changed)
    }
}

// --- Free functions parameterized over an already-open transaction ---
//
// These are the shared core of every `RelTable` method above (each of which
// is now just "open a tx, call the matching `_in` function, commit") and of
// `crate::relational::txn`'s batch API, which calls them directly against
// one caller-held transaction spanning several tables/operations instead of
// committing after each one.

pub(crate) fn write_row_in<W: StorageWriteTx>(
    wtx: &mut W,
    schema: &RelSchema,
    pk: &PropValue,
    values: &Properties,
) -> Result<(), BknError> {
    let row_key = sortable_encode(pk)?;
    let mut stored = values.clone();
    stored.remove(schema.primary_key);
    wtx.put(base_table(schema), &row_key, &encode_row(&stored)?)?;

    let row = Row {
        pk: pk.clone(),
        values: stored,
    };
    for &col in schema.indexed_columns {
        if let Some(v) = row.get(schema, col) {
            wtx.put(index_table(schema, col), &index_key(v, pk)?, &[])?;
        }
    }
    Ok(())
}

pub(crate) fn insert_bulk_in<W: StorageWriteTx>(
    wtx: &mut W,
    schema: &RelSchema,
    rows: impl IntoIterator<Item = Properties>,
) -> Result<Vec<PropValue>, BknError> {
    let items: Vec<Properties> = rows.into_iter().collect();
    if items.is_empty() {
        return Ok(Vec::new());
    }

    let mut pks = Vec::with_capacity(items.len());
    if schema.auto_increment_pk {
        let start_pk = reserve_pks(wtx, schema, items.len() as u64)?;
        for (i, values) in items.into_iter().enumerate() {
            let pk = PropValue::Int(start_pk + i as i64);
            write_row_in(wtx, schema, &pk, &values)?;
            pks.push(pk);
        }
    } else {
        let col = schema.primary_key;
        for values in items.into_iter() {
            let pk = values
                .get(col)
                .cloned()
                .ok_or_else(|| BknError::Encoding(format!("missing primary key column '{col}'")))?;
            write_row_in(wtx, schema, &pk, &values)?;
            pks.push(pk);
        }
    }
    Ok(pks)
}

pub(crate) fn insert_with_pk_bulk_in<W: StorageWriteTx>(
    wtx: &mut W,
    schema: &RelSchema,
    rows: impl IntoIterator<Item = (PropValue, Properties)>,
) -> Result<(), BknError> {
    if schema.auto_increment_pk {
        return Err(BknError::Encoding(
            "insert_with_pk_bulk cannot be used on an auto-increment schema".to_string(),
        ));
    }
    for (pk, values) in rows {
        write_row_in(wtx, schema, &pk, &values)?;
    }
    Ok(())
}

pub(crate) fn get_in<R: StorageReadTx>(rtx: &R, schema: &RelSchema, pk: &PropValue) -> Result<Option<Row>, BknError> {
    let row_key = sortable_encode(pk)?;
    match rtx.get(base_table(schema), &row_key)? {
        Some(bytes) => Ok(Some(Row {
            pk: pk.clone(),
            values: decode_row(&bytes)?,
        })),
        None => Ok(None),
    }
}

pub(crate) fn scan_all_in<R: StorageReadTx>(rtx: &R, schema: &RelSchema) -> Result<Vec<Row>, BknError> {
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
    schema: &RelSchema,
    column: &str,
    value: &PropValue,
) -> Result<Vec<Row>, BknError> {
    let prefix = sortable_encode(value)?;
    let end = crate::relational::codec::prefix_upper_bound(&prefix);
    let rows = match &end {
        Some(end) => rtx.range(index_table(schema, column), Bound::Included(&prefix), Bound::Excluded(end))?,
        None => rtx.range(index_table(schema, column), Bound::Included(&prefix), Bound::Unbounded)?,
    };
    resolve_index_rows_in(rtx, schema, column, rows)
}

pub(crate) fn index_lookup_range_in<R: StorageReadTx>(
    rtx: &R,
    schema: &RelSchema,
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
    schema: &RelSchema,
    column: &str,
    prefix: &str,
) -> Result<Vec<Row>, BknError> {
    let col = schema
        .column(column)
        .ok_or_else(|| BknError::Encoding(format!("no such column '{column}'")))?;
    if col.kind != crate::relational::schema::ColumnKind::Str {
        return Err(BknError::Encoding(format!(
            "prefix search is only supported on Str columns, '{column}' is {:?}",
            col.kind
        )));
    }
    let (lo_bound, hi_bound) = sortable_str_prefix_bounds(prefix);
    let lo_ref = match &lo_bound {
        Bound::Included(b) => Bound::Included(b.as_slice()),
        Bound::Excluded(b) => Bound::Excluded(b.as_slice()),
        Bound::Unbounded => Bound::Unbounded,
    };
    let hi_ref = match &hi_bound {
        Bound::Included(b) => Bound::Included(b.as_slice()),
        Bound::Excluded(b) => Bound::Excluded(b.as_slice()),
        Bound::Unbounded => Bound::Unbounded,
    };
    let rows = rtx.range(index_table(schema, column), lo_ref, hi_ref)?;
    resolve_index_rows_in(rtx, schema, column, rows)
}


fn resolve_index_rows_in<R: StorageReadTx>(
    rtx: &R,
    schema: &RelSchema,
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
        if let Some(row) = get_in(rtx, schema, &pk)? {
            out.push(row);
        }
    }
    Ok(out)
}

pub(crate) fn delete_row_in<W: StorageWriteTx>(wtx: &mut W, schema: &RelSchema, pk: &PropValue) -> Result<bool, BknError> {
    let row_key = sortable_encode(pk)?;
    let Some(bytes) = wtx.get(base_table(schema), &row_key)? else {
        return Ok(false);
    };
    let values: Properties = decode_row(&bytes)?;
    let row = Row {
        pk: pk.clone(),
        values,
    };
    wtx.delete(base_table(schema), &row_key)?;
    for &col in schema.indexed_columns {
        if let Some(v) = row.get(schema, col) {
            wtx.delete(index_table(schema, col), &index_key(v, pk)?)?;
        }
    }
    Ok(true)
}

pub(crate) fn update_row_in<W: StorageWriteTx>(
    wtx: &mut W,
    schema: &RelSchema,
    pk: &PropValue,
    mutate: impl FnOnce(&mut Properties),
) -> Result<bool, BknError> {
    let row_key = sortable_encode(pk)?;
    let Some(bytes) = wtx.get(base_table(schema), &row_key)? else {
        return Ok(false);
    };
    let old_values: Properties = decode_row(&bytes)?;
    let mut new_values = old_values.clone();
    mutate(&mut new_values);
    new_values.remove(schema.primary_key);

    let old_row = Row {
        pk: pk.clone(),
        values: old_values,
    };
    let new_row = Row {
        pk: pk.clone(),
        values: new_values.clone(),
    };

    // Indexed columns whose value changed need their old index entry
    // removed and the new one written — the same read-modify-write shape
    // as GraphDb::update_edge_properties, generalized across however many
    // indexed columns this schema declares.
    for &col in schema.indexed_columns {
        let old = old_row.get(schema, col);
        let new = new_row.get(schema, col);
        if old != new {
            if let Some(old_v) = old {
                wtx.delete(index_table(schema, col), &index_key(old_v, pk)?)?;
            }
            if let Some(new_v) = new {
                wtx.put(index_table(schema, col), &index_key(new_v, pk)?, &[])?;
            }
        }
    }

    wtx.put(base_table(schema), &row_key, &encode_row(&new_values)?)?;
    Ok(true)
}
