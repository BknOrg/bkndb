use std::path::{Path, PathBuf};
use std::process::ExitCode;

use bkndb::{RedbStorageBackend, StorageBackend, StorageReadTx, StorageWriteTx, TableSpec};

fn main() -> ExitCode {
    let path = match parse_args() {
        Ok(path) => path,
        Err(msg) => {
            eprintln!("Error: {msg}");
            eprintln!("Usage: bkndb-cli <path-to-db-file>");
            return ExitCode::FAILURE;
        }
    };

    match run(&path) {
        Ok(status) => {
            println!("Opened '{}': status = {:?}", path.display(), status);
            ExitCode::SUCCESS
        }
        Err(msg) => {
            eprintln!("Error: {msg}");
            ExitCode::FAILURE
        }
    }
}

fn parse_args() -> Result<PathBuf, String> {
    let mut args = std::env::args().skip(1);
    let raw = args
        .next()
        .ok_or_else(|| "missing required <path-to-db-file> argument".to_string())?;
    if raw.trim().is_empty() {
        return Err("<path-to-db-file> must not be empty".to_string());
    }

    let path = PathBuf::from(raw);
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        if !parent.exists() {
            return Err(format!(
                "directory '{}' does not exist; create it first",
                parent.display()
            ));
        }
    }
    Ok(path)
}

fn run(path: &Path) -> Result<Option<String>, String> {
    let backend = RedbStorageBackend::open(path)
        .map_err(|e| format!("failed to open database at '{}': {e}", path.display()))?;

    {
        let mut wtx = backend
            .begin_write()
            .map_err(|e| format!("failed to begin write transaction: {e}"))?;
        wtx.put(TableSpec("meta"), b"status", b"ok")
            .map_err(|e| format!("failed to write smoke-test key: {e}"))?;
        wtx.commit()
            .map_err(|e| format!("failed to commit write transaction: {e}"))?;
    }

    let rtx = backend
        .begin_read()
        .map_err(|e| format!("failed to begin read transaction: {e}"))?;
    let value = rtx
        .get(TableSpec("meta"), b"status")
        .map_err(|e| format!("failed to read back smoke-test key: {e}"))?;

    Ok(value.map(|v| String::from_utf8_lossy(&v).into_owned()))
}
