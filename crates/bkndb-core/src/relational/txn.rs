use std::ops::Bound;

use crate::relational::codec::next_pk;
use crate::relational::db::{
    delete_row_in, get_in, index_lookup_eq_in, index_lookup_prefix_in, index_lookup_range_in,
    insert_bulk_in, insert_with_pk_bulk_in, scan_all_in, update_row_in, write_row_in, Row,
};
use crate::relational::schema::RelSchema;
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

    pub fn table<'s>(&'s mut self, schema: &'s RelSchema) -> BatchTable<'s, W> {
        BatchTable::new(&mut self.wtx, schema)
    }
}

/// A single table's view into an in-progress [`RelWriteBatch`]. Mirrors
/// [`crate::relational::RelTable`]'s operations, but every method here reads
/// and writes through the batch's already-open transaction instead of
/// opening/committing its own — nothing is durable until the enclosing
/// [`crate::relational::RelationalDb::write_tx`] closure returns `Ok` and its
/// single `commit()` runs.
pub struct BatchTable<'s, W: StorageWriteTx> {
    wtx: &'s mut W,
    schema: &'s RelSchema,
}

impl<'s, W: StorageWriteTx> BatchTable<'s, W> {
    /// Panics on a reserved `schema.name` — see
    /// [`crate::relational::RelationalDb::table`] for why this is a panic,
    /// not a `Result`. The single enforcement point shared by both
    /// [`RelWriteBatch::table`] and [`crate::relational::txn::RelBatchView::table`].
    pub(crate) fn new(wtx: &'s mut W, schema: &'s RelSchema) -> Self {
        crate::check_table_name(schema.name).unwrap_or_else(|e| panic!("{e}"));
        Self { wtx, schema }
    }

    /// See [`crate::relational::RelTable::insert`].
    pub fn insert(&mut self, values: Properties) -> Result<PropValue, BknError> {
        let pk = if self.schema.auto_increment_pk {
            PropValue::Int(next_pk(self.wtx, self.schema)?)
        } else {
            let col = self.schema.primary_key;
            values
                .get(col)
                .cloned()
                .ok_or_else(|| BknError::Encoding(format!("missing primary key column '{col}'")))?
        };
        write_row_in(self.wtx, self.schema, &pk, &values)?;
        Ok(pk)
    }

    /// Inserts multiple rows in a single batch, reserving PKs in one counter update
    /// if auto-increment is enabled.
    pub fn insert_bulk(&mut self, rows: impl IntoIterator<Item = Properties>) -> Result<Vec<PropValue>, BknError> {
        insert_bulk_in(self.wtx, self.schema, rows)
    }

    /// See [`crate::relational::RelTable::insert_with_pk`].
    pub fn insert_with_pk(&mut self, pk: PropValue, values: Properties) -> Result<(), BknError> {
        if self.schema.auto_increment_pk {
            return Err(BknError::Encoding(
                "insert_with_pk cannot be used on an auto-increment schema".to_string(),
            ));
        }
        write_row_in(self.wtx, self.schema, &pk, &values)?;
        Ok(())
    }

    /// Inserts multiple rows with caller-supplied PKs in a single batch.
    pub fn insert_with_pk_bulk(&mut self, rows: impl IntoIterator<Item = (PropValue, Properties)>) -> Result<(), BknError> {
        insert_with_pk_bulk_in(self.wtx, self.schema, rows)
    }

    /// See [`crate::relational::RelTable::get`]. Reads the batch's own
    /// pending writes (read-your-own-writes within the open transaction),
    /// not just the pre-batch snapshot.
    pub fn get(&self, pk: &PropValue) -> Result<Option<Row>, BknError> {
        get_in(self.wtx, self.schema, pk)
    }

    /// Rows whose `column` value exactly equals `value` — uses that
    /// column's secondary index when one exists, otherwise falls back to a
    /// full table scan filtered in Rust, mirroring
    /// [`crate::relational::SelectQuery`]'s own index-or-scan fallback rule.
    pub fn select_eq(&self, column: &str, value: &PropValue) -> Result<Vec<Row>, BknError> {
        if self.schema.is_indexed(column) {
            index_lookup_eq_in(self.wtx, self.schema, column, value)
        } else {
            let rows = scan_all_in(self.wtx, self.schema)?;
            Ok(rows.into_iter().filter(|r| r.get(self.schema, column) == Some(value)).collect())
        }
    }

    /// See [`crate::relational::RelTable::update`]/`update_row`.
    pub fn update(&mut self, pk: &PropValue, mutate: impl FnOnce(&mut Properties)) -> Result<bool, BknError> {
        update_row_in(self.wtx, self.schema, pk, mutate)
    }

    /// Applies `sets` (column, value) pairs to every row whose `column`
    /// value equals `value`, returning how many rows changed. See
    /// [`crate::relational::UpdateQuery`] for the non-batch equivalent.
    pub fn update_where_eq(&mut self, column: &str, value: &PropValue, sets: &[(&str, PropValue)]) -> Result<usize, BknError> {
        let rows = self.select_eq(column, value)?;
        let mut count = 0;
        for row in rows {
            let sets = sets.to_vec();
            let changed = update_row_in(self.wtx, self.schema, &row.pk, |values| {
                for (col, val) in sets {
                    values.insert(col.to_string(), val);
                }
            })?;
            if changed {
                count += 1;
            }
        }
        Ok(count)
    }

    /// See [`crate::relational::RelTable::delete`]/`delete_row`.
    pub fn delete(&mut self, pk: &PropValue) -> Result<bool, BknError> {
        delete_row_in(self.wtx, self.schema, pk)
    }

    /// Deletes every row whose `column` value equals `value`, returning how
    /// many rows were removed — the cascade-delete building block for
    /// clearing a table's rows belonging to one parent id (e.g. `file_id`).
    pub fn delete_where_eq(&mut self, column: &str, value: &PropValue) -> Result<usize, BknError> {
        let rows = self.select_eq(column, value)?;
        let mut count = 0;
        for row in rows {
            if delete_row_in(self.wtx, self.schema, &row.pk)? {
                count += 1;
            }
        }
        Ok(count)
    }

    /// Unbounded full-table scan, matching
    /// [`crate::relational::SelectQuery::run`] with no predicates.
    pub fn select_all(&self) -> Result<Vec<Row>, BknError> {
        scan_all_in(self.wtx, self.schema)
    }

    /// Range scan over an indexed column.
    pub fn select_range(
        &self,
        column: &str,
        start: &Bound<PropValue>,
        end: &Bound<PropValue>,
    ) -> Result<Vec<Row>, BknError> {
        if self.schema.is_indexed(column) {
            index_lookup_range_in(self.wtx, self.schema, column, start, end)
        } else {
            Err(BknError::Encoding(format!("cannot range scan unindexed column '{column}'")))
        }
    }

    /// Rows whose `column` string value starts with `prefix`, using that
    /// column's secondary index if available, falling back to a full scan.
    pub fn select_prefix(&self, column: &str, prefix: &str) -> Result<Vec<Row>, BknError> {
        if self.schema.is_indexed(column) {
            index_lookup_prefix_in(self.wtx, self.schema, column, prefix)
        } else {
            let rows = scan_all_in(self.wtx, self.schema)?;
            Ok(rows
                .into_iter()
                .filter(|r| match r.get(self.schema, column) {
                    Some(PropValue::Str(s)) => s.starts_with(prefix),
                    _ => false,
                })
                .collect())
        }
    }
}

/// Borrowing sibling of [`RelWriteBatch`], for use inside a bigger,
/// already-open batch (e.g. [`crate::db::DbWriteBatch::relational`]) that
/// also touches other models over the same transaction — relational still
/// needs this extra `.table()` indirection (unlike graph's flat
/// [`crate::graph::txn::BatchGraph`]) because it's multi-table.
pub struct RelBatchView<'s, W: StorageWriteTx> {
    wtx: &'s mut W,
}

impl<'s, W: StorageWriteTx> RelBatchView<'s, W> {
    pub(crate) fn new(wtx: &'s mut W) -> Self {
        Self { wtx }
    }

    pub fn table<'t>(&'t mut self, schema: &'t RelSchema) -> BatchTable<'t, W> {
        BatchTable::new(self.wtx, schema)
    }
}

/// A read-only view of a relational table over an open [`StorageReadTx`].
pub struct ReadTable<'s, R: StorageReadTx> {
    rtx: &'s R,
    schema: &'s RelSchema,
}

impl<'s, R: StorageReadTx> ReadTable<'s, R> {
    pub(crate) fn new(rtx: &'s R, schema: &'s RelSchema) -> Self {
        crate::check_table_name(schema.name).unwrap_or_else(|e| panic!("{e}"));
        Self { rtx, schema }
    }

    pub fn get(&self, pk: &PropValue) -> Result<Option<Row>, BknError> {
        get_in(self.rtx, self.schema, pk)
    }

    pub fn select_eq(&self, column: &str, value: &PropValue) -> Result<Vec<Row>, BknError> {
        if self.schema.is_indexed(column) {
            index_lookup_eq_in(self.rtx, self.schema, column, value)
        } else {
            let rows = scan_all_in(self.rtx, self.schema)?;
            Ok(rows.into_iter().filter(|r| r.get(self.schema, column) == Some(value)).collect())
        }
    }

    pub fn select_all(&self) -> Result<Vec<Row>, BknError> {
        scan_all_in(self.rtx, self.schema)
    }

    pub fn select_range(
        &self,
        column: &str,
        start: &Bound<PropValue>,
        end: &Bound<PropValue>,
    ) -> Result<Vec<Row>, BknError> {
        if self.schema.is_indexed(column) {
            index_lookup_range_in(self.rtx, self.schema, column, start, end)
        } else {
            Err(BknError::Encoding(format!("cannot range scan unindexed column '{column}'")))
        }
    }

    /// Rows whose `column` string value starts with `prefix`, using that
    /// column's secondary index if available, falling back to a full scan.
    pub fn select_prefix(&self, column: &str, prefix: &str) -> Result<Vec<Row>, BknError> {
        if self.schema.is_indexed(column) {
            index_lookup_prefix_in(self.rtx, self.schema, column, prefix)
        } else {
            let rows = scan_all_in(self.rtx, self.schema)?;
            Ok(rows
                .into_iter()
                .filter(|r| match r.get(self.schema, column) {
                    Some(PropValue::Str(s)) => s.starts_with(prefix),
                    _ => false,
                })
                .collect())
        }
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

    pub fn table(&self, schema: &'s RelSchema) -> ReadTable<'s, R> {
        ReadTable::new(self.rtx, schema)
    }
}

