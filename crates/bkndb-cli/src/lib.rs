mod advanced;
pub mod args;
mod backend;
mod dump;
mod error;
mod files;
mod maintenance;
mod output;
mod query;
mod shell;
use args::{Cli, Command};
pub use error::{Error, Result};
pub use output::report_error;
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};
static CANCELLED: AtomicBool = AtomicBool::new(false);
pub fn cancel() {
    CANCELLED.store(true, Ordering::SeqCst);
}
pub(crate) fn reset_cancel() {
    CANCELLED.store(false, Ordering::SeqCst);
}
pub(crate) fn checkpoint() -> Result<()> {
    if CANCELLED.load(Ordering::SeqCst) {
        Err(Error::new(
            130,
            "operation cancelled; an already completed commit is not rolled back",
        ))
    } else {
        Ok(())
    }
}
pub fn run(cli: Cli) -> Result<()> {
    if cli.format == args::Format::Csv
        && !matches!(cli.command, Command::Sql { .. } | Command::Shell { .. })
    {
        return Err(Error::input(
            "CSV output is available for SQL and shell; use export --table for table files",
        ));
    }
    let operation = match &cli.command {
        Command::Create { .. } => "create",
        Command::Info(_) => "info",
        Command::Stats(_) => "stats",
        Command::Check(_) => "check",
        Command::Doctor(_) => "doctor",
        Command::Compact(_) => "compact",
        Command::Backup { .. } => "backup",
        Command::Restore { .. } => "restore",
        Command::Migrate { .. } => "migrate",
        Command::Repair { .. } => "repair",
        Command::Capabilities { .. } => "capabilities",
        Command::Version => "version",
        Command::Sql { .. } => "sql",
        Command::Shell { .. } => "shell",
        Command::Tables(_) => "tables",
        Command::Schema { .. } => "schema",
        Command::Indexes { .. } => "indexes",
        Command::Graph { .. } => "graph",
        Command::Search { .. } => "search",
        Command::Index { .. } => "index",
        Command::Export { .. } => "export",
        Command::Import { .. } => "import",
        Command::Debug { .. } => "debug",
        Command::Kv { .. } => "kv",
        Command::Bench(_) => "bench",
        Command::Stress(_) => "stress",
        Command::Completion { .. } => "completion",
    };
    let data = match &cli.command {
        Command::Create { file, backend } => maintenance::create(file, *backend)?,
        Command::Info(s) => maintenance::info(&files::Database::open(s, false)?),
        Command::Stats(s) => maintenance::stats(&files::Database::open(s, false)?)?,
        Command::Check(s) => {
            let db = files::Database::open(s, false)?;
            let r = maintenance::check(&db)?;
            if db.recovery_required {
                return Err(Error::new(
                    7,
                    format!("source requires recovery; inspection ran on a copy: {r}"),
                ));
            }
            r
        }
        Command::Doctor(s) => advanced::doctor(s)?,
        Command::Compact(s) => maintenance::compact(s)?,
        Command::Backup { source, output } | Command::Restore { source, output } => {
            maintenance::duplicate(source, output, None)?
        }
        Command::Migrate { source, to, output } => {
            maintenance::duplicate(source, output, Some(*to))?
        }
        Command::Repair { source, output } => maintenance::repair(source, output)?,
        Command::Capabilities { file } => advanced::capabilities(file.as_deref())?,
        Command::Version => {
            json!({"version":env!("CARGO_PKG_VERSION"),"candidate":true,"lsm_read_versions":[1,2,3],"lsm_write_version":3,"dump_version":1})
        }
        Command::Sql {
            source,
            query: q,
            script,
            params,
            per_statement,
        } => {
            let text = match (q, script) {
                (Some(q), _) => q.clone(),
                (_, Some(p)) => advanced::read_text(p, 16 * 1024 * 1024)?,
                _ => return Err(Error::input("SQL input missing")),
            };
            // Determine access mode before any writable opening occurs.
            let statements = bkndb::lang::split_sql_script(&text)?;
            if cli.format == args::Format::Csv && statements.len() != 1 {
                return Err(Error::input(
                    "CSV SQL output requires exactly one statement",
                ));
            }
            let mutates = statements
                .iter()
                .map(|s| bkndb::lang::sql::parse(s))
                .collect::<std::result::Result<Vec<_>, _>>()?
                .iter()
                .any(|s| !s.is_read_only());
            if mutates {
                query::require_write(cli.write)?;
            }
            let db = files::Database::open(source, mutates)?;
            query::sql(
                &db.db,
                &text,
                &query::params(params)?,
                cli.write,
                *per_statement,
            )?
        }
        Command::Shell { source, history } => {
            return shell::run(source, cli.write, cli.format, history.as_deref());
        }
        Command::Tables(s) => json!(
            files::Database::open(s, false)?
                .db
                .relational()
                .list_tables()?
                .iter()
                .map(|s| s.name().to_owned())
                .collect::<Vec<_>>()
        ),
        Command::Schema { source, table } => {
            query::schemas(&files::Database::open(source, false)?, table.as_deref())?
        }
        Command::Indexes { source, table } => {
            query::indexes(&files::Database::open(source, false)?, table.as_deref())?
        }
        Command::Graph { command } => query::graph(command, cli.write)?,
        Command::Search { command } => query::search(command)?,
        Command::Index { command } => query::index(command, cli.write)?,
        Command::Export {
            source,
            output,
            table,
        } => {
            if let Some(table) = table {
                dump::export_csv(source, output, table)?
            } else {
                dump::export(source, output)?
            }
        }
        Command::Import {
            file,
            input,
            backend,
            table,
        } => {
            query::require_write(cli.write)?;
            if let Some(table) = table {
                dump::import_csv(
                    input,
                    &args::Source {
                        file: file.clone(),
                        backend: Some(*backend),
                    },
                    table,
                )?
            } else {
                dump::import(input, file, *backend)?
            }
        }
        Command::Debug { command } => advanced::debug(command)?,
        Command::Kv { command } => advanced::kv(command, cli.write)?,
        Command::Bench(w) => advanced::workload(w, false)?,
        Command::Stress(w) => advanced::workload(w, true)?,
        Command::Completion { shell } => {
            use clap::CommandFactory;
            clap_complete::generate(*shell, &mut Cli::command(), "bkndb", &mut std::io::stdout());
            return Ok(());
        }
    };
    output::emit(operation, data, cli.format)
}

// Only test builds can deliberately pause at persistence boundaries. A parent
// test kills the subprocess after receiving the marker, without destructors.
pub(crate) fn failpoint(name: &str) {
    #[cfg(feature = "test-failpoints")]
    if std::env::var("BKNDB_CLI_FAILPOINT").as_deref() == Ok(name) {
        if let Ok(path) = std::env::var("BKNDB_CLI_MARKER") {
            std::fs::write(path, name).expect("test marker");
        }
        loop {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
    #[cfg(not(feature = "test-failpoints"))]
    let _ = name;
}
#[cfg(feature = "fuzzing")]
pub fn fuzz_inputs(data: &[u8]) {
    use clap::Parser;
    if data.len() > 65536 {
        return;
    }
    let _ = dump::csv_row(&mut std::io::Cursor::new(data));
    let _ = dump::read_dump(&mut std::io::Cursor::new(data), |_, _, _| Ok(()));
    if let Ok(text) = std::str::from_utf8(data) {
        if let Ok(statements) = bkndb::lang::split_sql_script(text) {
            for statement in statements {
                let _ = bkndb::lang::sql::parse(statement);
            }
        }
        let _ = query::params(text);
        let _ = args::Cli::try_parse_from(
            std::iter::once("bkndb").chain(text.split_whitespace().take(128)),
        );
    }
}
