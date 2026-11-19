//! Versioned, streaming physical dump. Byte encoding preserves counters, IDs,
//! schemas, application KV and derived indexes exactly. This is intentionally
//! bound to the current core encoding, not an unversioned JSON interchange.
use crate::{
    args::{Backend as Kind, Source},
    backend::Backend,
    checkpoint,
    error::{Error, Result},
    files::{Database, Stage},
    maintenance,
};
use bkndb_engine::{StorageWriteTx as _, TableSpec};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs::File,
    io::{BufRead, BufReader, BufWriter, Read, Write},
    path::Path,
};
const MAX_LINE: usize = 32 * 1024 * 1024;
const MAX_RECORD: usize = 16 * 1024 * 1024;
const MAX_CSV_COLUMNS: usize = 16384;
const CORE_FORMAT: &str = "bkndb-core-0.3.1";

pub fn export(source: &Source, output: &Path) -> Result<Value> {
    let source = Database::open(source, false)?;
    maintenance::check(&source)?;
    let stage = Stage::new(output)?;
    let mut out = BufWriter::new(File::create(&stage.path)?);
    let mut hasher = Sha256::new();
    let mut count = 0u64;
    let mut tables = HashSet::new();
    write_line(
        &mut out,
        &mut hasher,
        &json!({"format":"bkndb-physical-dump","version":1,"core_format":CORE_FORMAT}),
    )?;
    let mut row = |table: &str, key: &[u8], value: &[u8]| -> Result<()> {
        checkpoint()?;
        if table.is_empty() || table.len() > 255 || table.contains('\0') {
            return Err(Error::new(
                9,
                "physical table cannot be represented by dump v1; use native backup",
            ));
        }
        if tables.insert(table.to_owned()) && tables.len() > 16384 {
            return Err(Error::new(
                9,
                "dump exceeds 16384 tables; use native backup",
            ));
        }
        if table.len() + key.len() + value.len() > MAX_RECORD {
            return Err(Error::new(
                9,
                "dump record exceeds 16 MiB; use native backup instead",
            ));
        }
        let v = json!({"table":table,"key":hex::encode(key),"value":hex::encode(value)});
        write_line(&mut out, &mut hasher, &v)?;
        count += 1;
        Ok(())
    };
    let mut callback_error = None;
    let exported: Result<()> = match source.db.backend() {
        Backend::Redb(b) => b
            .export_to(|t, k, v| {
                row(t, k, v).map_err(|e| {
                    let message = e.to_string();
                    callback_error = Some(e);
                    bkndb::BknError::Backend(message)
                })
            })
            .map_err(Error::from),
        Backend::Lsm(b) => b
            .engine()
            .export_to(|t, k, v| {
                row(t, k, v).map_err(|e| {
                    let message = e.to_string();
                    callback_error = Some(e);
                    bkndb_engine::Error::Backend(message)
                })
            })
            .map_err(Error::from),
    };
    if let Some(error) = callback_error {
        return Err(error);
    }
    exported?;
    serde_json::to_writer(
        &mut out,
        &json!({"end":true,"records":count,"sha256":hex::encode(hasher.finalize())}),
    )?;
    out.write_all(b"\n")?;
    out.flush()?;
    drop(out);
    stage.publish()?;
    Ok(json!({"output":output,"records":count,"core_format":CORE_FORMAT}))
}
fn write_line(out: &mut impl Write, hash: &mut Sha256, value: &Value) -> Result<()> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    if bytes.len() > MAX_LINE {
        return Err(Error::new(9, "encoded dump line exceeds 32 MiB"));
    }
    out.write_all(&bytes)?;
    hash.update(&bytes);
    Ok(())
}
fn line(reader: &mut impl BufRead) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take((MAX_LINE + 1) as u64)
        .read_until(b'\n', &mut bytes)?;
    if bytes.len() > MAX_LINE {
        return Err(Error::new(9, "dump/CSV line exceeds 32 MiB"));
    }
    if bytes.last() != Some(&b'\n') {
        return Err(Error::input("truncated dump: expected newline and footer"));
    }
    Ok(bytes)
}
pub(crate) fn read_dump(
    reader: &mut impl BufRead,
    mut row: impl FnMut(String, Vec<u8>, Vec<u8>) -> Result<()>,
) -> Result<u64> {
    let header = line(reader)?;
    let v: Value = serde_json::from_slice(&header)?;
    if v.get("format") != Some(&json!("bkndb-physical-dump"))
        || v.get("version") != Some(&json!(1))
        || v.get("core_format") != Some(&json!(CORE_FORMAT))
    {
        return Err(Error::unsupported(
            "unsupported dump version or core encoding",
        ));
    }
    let mut hash = Sha256::new();
    hash.update(&header);
    let mut count = 0u64;
    let mut tables = HashSet::new();
    let mut previous_table = String::new();
    let mut previous_key: Option<Vec<u8>> = None;
    loop {
        checkpoint()?;
        let raw = line(reader)?;
        let value: Value = serde_json::from_slice(&raw)?;
        if value.get("end") == Some(&Value::Bool(true)) {
            if value.get("records") != Some(&json!(count))
                || value.get("sha256") != Some(&json!(hex::encode(hash.finalize())))
            {
                return Err(Error::new(5, "dump checksum or record count mismatch"));
            }
            if !reader.fill_buf()?.is_empty() {
                return Err(Error::input("unexpected bytes after dump footer"));
            }
            break;
        }
        hash.update(&raw);
        let table = value
            .get("table")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::input("dump table is missing"))?;
        if table.is_empty() || table.len() > 255 || table.contains('\0') {
            return Err(Error::input("invalid physical table name"));
        }
        let decode = |name| -> Result<Vec<u8>> {
            hex::decode(
                value
                    .get(name)
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::input("missing encoded key/value"))?,
            )
            .map_err(|e| Error::input(e.to_string()))
        };
        let key = decode("key")?;
        let bytes = decode("value")?;
        if table.len() + key.len() + bytes.len() > MAX_RECORD {
            return Err(Error::new(9, "dump record exceeds 16 MiB"));
        }
        if table != previous_table {
            if !tables.insert(table.to_owned()) {
                return Err(Error::input("dump repeats a completed table"));
            }
            if tables.len() > 16384 {
                return Err(Error::new(9, "too many dump tables"));
            }
            previous_table = table.to_owned();
            previous_key = None;
        }
        if previous_key.as_ref().is_some_and(|p| p >= &key) {
            return Err(Error::input("duplicate or unordered dump key"));
        }
        previous_key = Some(key.clone());
        row(table.to_owned(), key, bytes)?;
        count += 1;
    }
    Ok(count)
}

pub fn import(input: &Path, output: &Path, kind: Kind) -> Result<Value> {
    let stage = Stage::new(output)?;
    crate::files::create(&stage.path, kind)?;
    let mut reader = BufReader::new(File::open(input)?);
    let db = Database::open(&crate::files::source(&stage.path), true)?;
    let mut batch = Vec::new();
    let mut batch_bytes = 0usize;
    let count = read_dump(&mut reader, |table, key, bytes| {
        batch_bytes += table.len() + key.len() + bytes.len();
        batch.push((table, key, bytes));
        if batch_bytes >= 4 * 1024 * 1024 {
            import_batch(db.db.backend(), &batch)?;
            batch.clear();
            batch_bytes = 0;
        }
        Ok(())
    })?;
    import_batch(db.db.backend(), &batch)?;
    // Imported physical index records are not treated as proof that derived
    // indexes agree with canonical rows. Rebuild in the private destination.
    maintenance::check(&db)?;
    rebuild_indexes(&db)?;
    drop(db);
    maintenance::validate(&stage.path)?;
    stage.publish()?;
    Ok(json!({"output":output,"records":count,"backend":kind,"derived_indexes_rebuilt":true}))
}
fn rebuild_indexes(db: &Database) -> Result<()> {
    checkpoint()?;
    db.db.graph().rebuild_indexes()?;
    let rel = db.db.relational();
    for schema in rel.list_tables()? {
        checkpoint()?;
        let text = rel.fulltext_indexes(schema.name())?;
        let vectors = rel.vector_indexes(schema.name())?;
        rel.rebuild_indexes(schema.name())?;
        for column in text {
            checkpoint()?;
            rel.drop_fulltext_index(schema.name(), &column)?;
            rel.create_fulltext_index(schema.name(), &column)?;
        }
        for index in vectors {
            checkpoint()?;
            if !(2..=16384).contains(&index.m) || !(1..=16384).contains(&index.ef_construction) {
                return Err(Error::new(
                    9,
                    "imported vector index parameters exceed CLI resource limits",
                ));
            }
            rel.drop_vector_index(schema.name(), &index.column)?;
            rel.create_vector_index(
                schema.name(),
                &index.column,
                bkndb::relational::VectorIndexOptions {
                    metric: index.metric,
                    m: index.m,
                    ef_construction: index.ef_construction,
                },
            )?;
        }
    }
    Ok(())
}
fn import_batch(backend: &Backend, batch: &[(String, Vec<u8>, Vec<u8>)]) -> Result<()> {
    if batch.is_empty() {
        return Ok(());
    }
    match backend {
        Backend::Redb(b) => b.import_batch(batch)?,
        Backend::Lsm(b) => {
            let mut tx = b.engine().begin_write_owned()?;
            for (t, k, v) in batch {
                tx.put(TableSpec(t), k, v)?;
            }
            tx.commit()?;
        }
    }
    Ok(())
}

/// CSV is table interchange. Each cell is a JSON-encoded PropValue, so NULL,
/// empty text, binary and nested values never depend on ambiguous sentinels.
pub fn export_csv(source: &Source, output: &Path, table: &str) -> Result<Value> {
    let db = Database::open(source, false)?;
    let schema = db
        .db
        .relational()
        .table_schema(table)?
        .ok_or_else(|| Error::input("table not found"))?;
    let stage = Stage::new(output)?;
    let mut out = BufWriter::new(File::create(&stage.path)?);
    let cols: Vec<String> = schema.columns().iter().map(|c| c.name.clone()).collect();
    write_csv_row(&mut out, &cols)?;
    let mut rows = 0u64;
    db.db.relational().scan_rows::<Error>(table, |row| {
        checkpoint()?;
        let cells = cols
            .iter()
            .map(|c| {
                serde_json::to_string(&crate::query::to_wire(
                    row.get(&schema, c)
                        .cloned()
                        .unwrap_or(bkndb::value::PropValue::Null),
                ))
            })
            .collect::<std::result::Result<Vec<_>, _>>()?;
        write_csv_row(&mut out, &cells)?;
        rows += 1;
        Ok(())
    })?;
    out.flush()?;
    drop(out);
    stage.publish()?;
    Ok(
        json!({"output":output,"rows":rows,"cell_encoding":"serde PropValue JSON","schema_required":true}),
    )
}
fn write_csv_row(out: &mut impl Write, fields: &[String]) -> Result<()> {
    if fields.len() > MAX_CSV_COLUMNS {
        return Err(Error::new(
            9,
            "CSV exceeds 16384 columns; use native backup",
        ));
    }
    // Every field is quoted, embedded quotes are doubled, and the comma/LF
    // separator costs one byte per field. Match the importer's record limit.
    let bytes = fields.iter().try_fold(0usize, |total, field| {
        total
            .checked_add(field.len())?
            .checked_add(field.bytes().filter(|&b| b == b'"').count())?
            .checked_add(3)
    });
    if bytes.is_none_or(|size| size > MAX_LINE) {
        return Err(Error::new(
            9,
            "CSV record exceeds 32 MiB; use native backup",
        ));
    }
    crate::output::write_row(out, fields, true)
}
// Streaming RFC4180-style reader with bounded records and embedded newlines.
pub fn csv_row(reader: &mut impl BufRead) -> Result<Option<Vec<String>>> {
    let mut fields = Vec::new();
    let mut field = Vec::new();
    let mut quoted = false;
    let mut closed = false;
    let mut total = 0usize;
    let mut started = false;
    loop {
        let mut byte = [0u8];
        let n = reader.read(&mut byte)?;
        if n == 0 {
            if quoted {
                return Err(Error::input("unterminated CSV quote"));
            }
            if !started {
                return Ok(None);
            }
            fields.push(String::from_utf8(field).map_err(|e| Error::input(e.to_string()))?);
            return Ok(Some(fields));
        }
        started = true;
        total += 1;
        if total > MAX_LINE {
            return Err(Error::new(9, "CSV record exceeds 32 MiB"));
        }
        let b = byte[0];
        if quoted {
            if b == b'"' {
                if reader.fill_buf()?.first() == Some(&b'"') {
                    reader.consume(1);
                    field.push(b'"');
                    total += 1;
                    if total > MAX_LINE {
                        return Err(Error::new(9, "CSV record exceeds 32 MiB"));
                    }
                } else {
                    quoted = false;
                    closed = true;
                }
            } else {
                field.push(b)
            }
        } else {
            match b {
                b'"' if field.is_empty() && !closed => quoted = true,
                b',' => {
                    if fields.len() >= MAX_CSV_COLUMNS - 1 {
                        return Err(Error::new(9, "CSV exceeds 16384 columns"));
                    }
                    fields.push(
                        String::from_utf8(std::mem::take(&mut field))
                            .map_err(|e| Error::input(e.to_string()))?,
                    );
                    closed = false;
                }
                b'\n' => {
                    if !closed && field.last() == Some(&b'\r') {
                        field.pop();
                    }
                    fields.push(String::from_utf8(field).map_err(|e| Error::input(e.to_string()))?);
                    return Ok(Some(fields));
                }
                b'\r' if closed => {
                    if reader.fill_buf()?.first() != Some(&b'\n') {
                        return Err(Error::input("expected LF after CR"));
                    }
                }
                _ if closed => return Err(Error::input("unexpected character after CSV quote")),
                b'"' => return Err(Error::input("quote inside unquoted CSV field")),
                _ => field.push(b),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoted_cr_is_data_and_record_crlf_is_a_delimiter() {
        for record in [b"\"value\r\"\n".as_slice(), b"\"value\r\"\r\n"] {
            assert_eq!(
                csv_row(&mut std::io::Cursor::new(record)).unwrap(),
                Some(vec!["value\r".into()])
            );
        }
        assert_eq!(
            csv_row(&mut std::io::Cursor::new(b"value\r\n")).unwrap(),
            Some(vec!["value".into()])
        );
    }
}
pub fn import_csv(input: &Path, source: &Source, table: &str) -> Result<Value> {
    let db = Database::open(source, true)?;
    let schema = db
        .db
        .relational()
        .table_schema(table)?
        .ok_or_else(|| Error::input("CSV import requires an existing schema"))?;
    let mut reader = BufReader::new(File::open(input)?);
    let columns = csv_row(&mut reader)?.ok_or_else(|| Error::input("CSV has no header"))?;
    let unique: HashSet<_> = columns.iter().collect();
    if unique.len() != columns.len()
        || columns.len() != schema.columns().len()
        || columns
            .iter()
            .any(|c| !schema.columns().iter().any(|s| s.name == *c))
    {
        return Err(Error::input(
            "CSV header must name every schema column exactly once",
        ));
    }
    let mut tx = db.db.begin_write()?;
    let mut count = 0u64;
    while let Some(row) = csv_row(&mut reader)? {
        checkpoint()?;
        if row.len() != columns.len() {
            return Err(Error::input("CSV column count mismatch"));
        }
        let mut values = bkndb::value::Properties::new();
        for (col, text) in columns.iter().zip(row) {
            let value = crate::query::from_wire(serde_json::from_str(&text)?)?;
            values.insert(col.clone(), value);
        }
        tx.relational().table_named(table)?.insert(values)?;
        count += 1;
    }
    checkpoint()?;
    tx.commit()?;
    Ok(json!({"rows":count,"atomic":true}))
}
