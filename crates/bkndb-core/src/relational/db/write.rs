//! Row writes: validation, constraints, index maintenance, insert/update/delete.
use super::*;

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

pub(super) fn pk_display(pk: &PropValue) -> String {
    match pk {
        PropValue::Int(i) => i.to_string(),
        PropValue::Str(s) => format!("{s:?}"),
        other => format!("{other:?}"),
    }
}

/// The value to index for `column`, if any: nulls are never indexed.
pub(super) fn indexable<'r>(row: &'r Row, schema: &TableSchema, column: &str) -> Option<&'r PropValue> {
    row.get(schema, column)
        .filter(|v| !matches!(v, PropValue::Null))
}

/// Fails if another row already holds any of `row`'s UNIQUE column values.
/// With `changed_from`, only columns whose value differs from it are checked.
pub(super) fn check_unique<R: StorageReadTx>(
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
    #[cfg(feature = "search")]
    crate::relational::search::on_row_change(wtx, schema, pk, old.as_ref().map(|r| &r.values), Some(&row.values))?;
    Ok(())
}

pub(super) fn explicit_pk(schema: &TableSchema, values: &Properties) -> Option<PropValue> {
    values
        .get(schema.primary_key())
        .filter(|v| !matches!(v, PropValue::Null))
        .cloned()
}

/// Whether an insert of `values` gets a freshly allocated pk: only on an
/// auto-increment table, and only when the row doesn't name one itself. An
/// explicit pk is always honored (as in SQL), bumping the counter past it,
/// and — for a plain insert — fails with `DuplicateKey` if it's taken.
pub(super) fn allocates_pk(schema: &TableSchema, values: &Properties) -> bool {
    schema.auto_increment_pk() && explicit_pk(schema, values).is_none()
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
        .filter(|v| allocates_pk(schema, v))
        .count();
    let mut next = if to_allocate > 0 {
        reserve_pks(wtx, schema, to_allocate as u64)?
    } else {
        0
    };

    let mut pks = Vec::with_capacity(items.len());
    for values in items {
        let pk = if allocates_pk(schema, &values) {
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
    #[cfg(feature = "search")]
    crate::relational::search::on_row_change(wtx, schema, pk, Some(&row.values), None)?;
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
    #[cfg(feature = "search")]
    crate::relational::search::on_row_change(wtx, schema, pk, Some(&old_row.values), Some(&new_row.values))?;
    Ok(true)
}
