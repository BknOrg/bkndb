use crate::{
    args::Format,
    error::{Error, Result},
};
use serde_json::{Value, json};
use std::io::{self, Write};

pub fn envelope(operation: &str, data: Value) -> Value {
    json!({"schema_version":1,"status":"ok","operation":operation,"data":data})
}
pub fn emit(operation: &str, data: Value, format: Format) -> Result<()> {
    let mut out = io::stdout().lock();
    match format {
        Format::Json => {
            serde_json::to_writer(&mut out, &envelope(operation, data))?;
            writeln!(out)?;
        }
        Format::Jsonl => {
            if let Some(items) = data.as_array() {
                for item in items {
                    serde_json::to_writer(&mut out, &envelope(operation, item.clone()))?;
                    writeln!(out)?;
                }
            } else {
                serde_json::to_writer(&mut out, &envelope(operation, data))?;
                writeln!(out)?;
            }
        }
        Format::Table | Format::Csv => {
            if let (Some(cols), Some(rows)) = (
                data.get("columns").and_then(Value::as_array),
                data.get("rows").and_then(Value::as_array),
            ) {
                let headers: Vec<String> = cols
                    .iter()
                    .map(|v| v.as_str().unwrap_or("").to_owned())
                    .collect();
                write_row(&mut out, &headers, format == Format::Csv)?;
                for row in rows {
                    if let Some(cells) = row.as_array() {
                        write_row(
                            &mut out,
                            &cells.iter().map(cell).collect::<Vec<_>>(),
                            format == Format::Csv,
                        )?;
                    }
                }
                if format == Format::Table {
                    writeln!(
                        out,
                        "affected: {}",
                        data.get("affected").unwrap_or(&Value::Null)
                    )?;
                }
            } else if format == Format::Csv {
                writeln!(io::stderr().lock(), "{}", envelope(operation, data))?;
            } else {
                serde_json::to_writer_pretty(&mut out, &data)?;
                writeln!(out)?;
            }
        }
    }
    Ok(())
}
fn cell(v: &Value) -> String {
    v.as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| v.to_string())
}
pub fn write_row(out: &mut impl Write, row: &[String], csv: bool) -> Result<()> {
    for (i, s) in row.iter().enumerate() {
        if i > 0 {
            write!(out, "{}", if csv { "," } else { "\t" })?;
        }
        if csv {
            write!(out, "\"{}\"", s.replace('"', "\"\""))?;
        } else {
            for c in s.chars() {
                for e in c.escape_default() {
                    write!(out, "{e}")?;
                }
            }
        }
    }
    writeln!(out)?;
    Ok(())
}
pub fn report_error(format: Format, e: &Error) {
    if matches!(format, Format::Json | Format::Jsonl) {
        eprintln!(
            "{}",
            json!({"schema_version":1,"status":if e.code==10{"partial"}else{"error"},"error":{"code":e.code,"message":e.message,"details":e.details}})
        );
    } else {
        eprintln!("bkndb: {} (exit {})", e.message, e.code);
    }
}
