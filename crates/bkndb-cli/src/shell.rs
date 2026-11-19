use crate::{
    advanced,
    args::{Format, Source},
    backend, checkpoint,
    error::{Error, Result},
    files::Database,
    maintenance, output, query, reset_cancel,
};
use bkndb::{DbWriteBatch, lang::split_sql_script};
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{self, ClearType},
};
use serde_json::json;
use std::{
    fs::{File, OpenOptions},
    io::{self, BufRead, IsTerminal, Read, Write},
    path::Path,
};

pub fn run(source: &Source, write: bool, mut format: Format, history: Option<&Path>) -> Result<()> {
    let db = Database::open(source, write)?;
    let interactive = io::stdin().is_terminal() && io::stderr().is_terminal();
    let mut editor = Editor::new(interactive, history)?;
    let mut transaction: Option<DbWriteBatch<backend::Write<'_>>> = None;
    let mut failed = false;
    let mut buffer = String::new();
    let mut timer = false;
    if interactive {
        eprintln!(
            "BknDb {} validation candidate; {}. .help for commands",
            env!("CARGO_PKG_VERSION"),
            if write { "writes enabled" } else { "read-only" }
        );
    }
    loop {
        let words = completion_words(&db)?;
        let prompt = if !buffer.is_empty() {
            "...> "
        } else if failed {
            "failed> "
        } else if transaction.is_some() {
            "tx> "
        } else {
            "bkndb> "
        };
        let line = match editor.read(prompt, &words)? {
            Input::End => {
                if !buffer.trim().is_empty()
                    && !split_sql_script(&buffer).is_ok_and(|s| s.is_empty())
                {
                    return Err(Error::input("incomplete SQL at end of input"));
                }
                break;
            }
            Input::Interrupt => {
                transaction.take();
                failed = false;
                buffer.clear();
                reset_cancel();
                if interactive {
                    eprintln!("pending transaction rolled back");
                    continue;
                } else {
                    return Err(Error::new(130, "shell cancelled"));
                }
            }
            Input::Line(l) => l,
        };
        if line.trim().is_empty() {
            continue;
        }
        if buffer.is_empty() && line.trim_start().starts_with('.') {
            let text = line.trim();
            let (command, arg) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
            let arg = arg.trim();
            let action: Result<bool> = (|| {
                if transaction.is_some() || failed {
                    if !matches!(command, ".help" | ".timer" | ".mode" | ".quit" | ".read") {
                        return Err(Error::input(
                            "finish or roll back the transaction before shell metadata commands",
                        ));
                    }
                }
                match command {
                    ".quit" => return Ok(true),
                    ".help" => output::emit(
                        "shell.help",
                        json!({"commands":[".help",".info",".stats",".tables",".schema [TABLE]",".indexes [TABLE]",".mode table|json|jsonl|csv",".timer on|off",".read FILE",".quit"],"transactions":"BEGIN; COMMIT; ROLLBACK; --write required for BEGIN; errors require ROLLBACK","input":"SQL statements terminate with semicolons; EOF rolls back pending writes"}),
                        format,
                    )?,
                    ".info" => output::emit("info", maintenance::info(&db), format)?,
                    ".stats" => output::emit("stats", maintenance::stats(&db)?, format)?,
                    ".tables" => output::emit(
                        "tables",
                        json!(
                            db.db
                                .relational()
                                .list_tables()?
                                .iter()
                                .map(|s| s.name())
                                .collect::<Vec<_>>()
                        ),
                        format,
                    )?,
                    ".schema" => output::emit(
                        "schema",
                        query::schemas(&db, (!arg.is_empty()).then_some(arg))?,
                        format,
                    )?,
                    ".indexes" => output::emit(
                        "indexes",
                        query::indexes(&db, (!arg.is_empty()).then_some(arg))?,
                        format,
                    )?,
                    ".mode" => {
                        format = match arg {
                            "table" => Format::Table,
                            "json" => Format::Json,
                            "jsonl" => Format::Jsonl,
                            "csv" => Format::Csv,
                            _ => return Err(Error::input("expected table, json, jsonl or csv")),
                        }
                    }
                    ".timer" => {
                        timer = match arg {
                            "on" => true,
                            "off" => false,
                            _ => return Err(Error::input("expected .timer on|off")),
                        }
                    }
                    ".read" => {
                        if failed {
                            return Err(Error::input("transaction failed; ROLLBACK required"));
                        }
                        let text = advanced::read_text(Path::new(arg), 16 * 1024 * 1024)?;
                        let r = if let Some(tx) = transaction.as_mut() {
                            execute_in(tx, &text)?
                        } else {
                            query::sql(&db.db, &text, &bkndb::lang::Params::none(), write, false)?
                        };
                        output::emit("sql", r, format)?;
                    }
                    _ => return Err(Error::input("unknown shell command; use .help")),
                }
                Ok(false)
            })();
            match action {
                Ok(true) => break,
                Ok(false) => (),
                Err(e) => {
                    if transaction.take().is_some() {
                        failed = true;
                    }
                    if !interactive || matches!(e.code, 7 | 8) {
                        return Err(e);
                    }
                    output::report_error(format, &e);
                }
            }
            continue;
        }
        if buffer.len() + line.len() > 16 * 1024 * 1024 {
            return Err(Error::new(9, "shell statement buffer exceeds 16 MiB"));
        }
        buffer.push_str(&line);
        buffer.push('\n');
        match bkndb::lang::sql_script_is_terminated(&buffer) {
            Ok(false) => continue,
            Err(e) if e.to_string().contains("unterminated") => continue,
            _ => (),
        }
        // An unfinished quote/comment can contain the apparent final delimiter.
        match split_sql_script(&buffer) {
            Err(e) if e.to_string().contains("unterminated") => continue,
            Err(e) => {
                buffer.clear();
                if transaction.take().is_some() {
                    failed = true;
                }
                if !interactive {
                    return Err(e.into());
                }
                output::report_error(format, &e.into());
                continue;
            }
            _ => (),
        }
        let start = std::time::Instant::now();
        let input = std::mem::take(&mut buffer);
        let action: Result<serde_json::Value> = (|| {
            let control = bkndb::lang::sql_transaction_control(&input)?;
            if control == Some(bkndb::lang::TransactionControl::Rollback) {
                transaction.take();
                failed = false;
                return Ok(json!({"rolled_back":true}));
            }
            if failed {
                return Err(Error::input("transaction failed; ROLLBACK required"));
            }
            if control == Some(bkndb::lang::TransactionControl::Begin) {
                query::require_write(write)?;
                if transaction.is_some() {
                    return Err(Error::input("nested transactions are not supported"));
                }
                transaction = Some(db.db.begin_write()?);
                return Ok(json!({"transaction":"open"}));
            }
            if control == Some(bkndb::lang::TransactionControl::Commit) {
                checkpoint()?;
                transaction
                    .take()
                    .ok_or_else(|| Error::input("no active transaction"))?
                    .commit()?;
                return Ok(json!({"committed":true}));
            }
            if let Some(tx) = transaction.as_mut() {
                execute_in(tx, &input)
            } else {
                query::sql(&db.db, &input, &bkndb::lang::Params::none(), write, false)
            }
        })();
        match action {
            Ok(value) => output::emit("shell", value, format)?,
            Err(e) => {
                if transaction.take().is_some() {
                    failed = true;
                }
                if !interactive || matches!(e.code, 7 | 8) {
                    return Err(e);
                }
                output::report_error(format, &e);
                reset_cancel();
            }
        }
        if timer {
            eprintln!("elapsed: {:.3} ms", start.elapsed().as_secs_f64() * 1000.0);
        }
    }
    if transaction.is_some() && interactive {
        eprintln!("pending transaction rolled back on exit");
    }
    Ok(())
}
fn execute_in(tx: &mut DbWriteBatch<backend::Write<'_>>, text: &str) -> Result<serde_json::Value> {
    let statements = split_sql_script(text)?;
    for statement in &statements {
        bkndb::lang::sql::parse(statement)?;
    }
    let mut out = Vec::new();
    for statement in statements {
        checkpoint()?;
        out.push(query::result(tx.relational().sql(statement, ())?));
    }
    Ok(if out.len() == 1 {
        out.remove(0)
    } else {
        json!({"statements":out,"pending_commit":true})
    })
}
fn completion_words(db: &Database) -> Result<Vec<String>> {
    let mut words = [
        "SELECT", "FROM", "WHERE", "INSERT", "UPDATE", "DELETE", "CREATE", "TABLE", "BEGIN",
        "COMMIT", "ROLLBACK", ".help", ".info", ".stats", ".tables", ".schema", ".indexes",
        ".mode", ".timer", ".read", ".quit",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect::<Vec<_>>();
    words.extend(
        db.db
            .relational()
            .list_tables()?
            .iter()
            .map(|s| s.name().to_owned()),
    );
    Ok(words)
}
enum Input {
    Line(String),
    End,
    Interrupt,
}
struct RawMode;
impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
    }
}
struct Editor {
    interactive: bool,
    history: Vec<String>,
    file: Option<File>,
}
impl Editor {
    fn new(interactive: bool, path: Option<&Path>) -> Result<Self> {
        let mut history = Vec::new();
        let mut file = None;
        if let Some(path) = path {
            if std::fs::symlink_metadata(path)
                .is_ok_and(|m| !m.is_file() || m.file_type().is_symlink())
            {
                return Err(Error::input("history must be a regular non-symlink file"));
            }
            if path.exists() {
                history = advanced::read_text(path, 1024 * 1024)?
                    .lines()
                    .map(str::to_owned)
                    .collect();
            }
            let mut options = OpenOptions::new();
            options.create(true).append(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            file = Some(options.open(path)?);
        }
        Ok(Self {
            interactive,
            history,
            file,
        })
    }
    fn remember(&mut self, line: &str) -> Result<()> {
        if self.history.len() >= 1000 {
            self.history.remove(0);
        }
        self.history.push(line.to_owned());
        if let Some(file) = self.file.as_mut() {
            writeln!(file, "{line}")?;
            file.flush()?;
        }
        Ok(())
    }
    fn read(&mut self, prompt: &str, words: &[String]) -> Result<Input> {
        if !self.interactive {
            let mut line = String::new();
            let size = io::stdin()
                .lock()
                .take(16 * 1024 * 1024 + 1)
                .read_line(&mut line)?;
            if size > 16 * 1024 * 1024 {
                return Err(Error::new(9, "input line exceeds 16 MiB"));
            }
            if size == 0 {
                return Ok(Input::End);
            }
            self.remember(line.trim_end())?;
            return Ok(Input::Line(line));
        }
        terminal::enable_raw_mode()?;
        let raw = RawMode;
        let mut err = io::stderr();
        write!(err, "{prompt}")?;
        err.flush()?;
        let mut line = String::new();
        let mut position = self.history.len();
        loop {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Release {
                    continue;
                }
                match key.code {
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        write!(err, "^C\r\n")?;
                        drop(raw);
                        return Ok(Input::Interrupt);
                    }
                    KeyCode::Char('d')
                        if key.modifiers.contains(KeyModifiers::CONTROL) && line.is_empty() =>
                    {
                        write!(err, "\r\n")?;
                        drop(raw);
                        return Ok(Input::End);
                    }
                    KeyCode::Enter => {
                        write!(err, "\r\n")?;
                        drop(raw);
                        self.remember(&line)?;
                        return Ok(Input::Line(line));
                    }
                    KeyCode::Backspace => {
                        line.pop();
                    }
                    KeyCode::Up => {
                        position = position.saturating_sub(1);
                        line = self.history.get(position).cloned().unwrap_or_default();
                    }
                    KeyCode::Down => {
                        position = (position + 1).min(self.history.len());
                        line = self.history.get(position).cloned().unwrap_or_default();
                    }
                    KeyCode::Tab => {
                        let start = line.rfind(char::is_whitespace).map_or(0, |p| p + 1);
                        let part = &line[start..];
                        let candidates = words
                            .iter()
                            .filter(|w| w.to_lowercase().starts_with(&part.to_lowercase()))
                            .collect::<Vec<_>>();
                        if candidates.len() == 1 {
                            let completed = candidates[0].clone();
                            line.truncate(start);
                            line.push_str(&completed);
                        } else if !candidates.is_empty() {
                            write!(
                                err,
                                "\r\n{}\r\n",
                                candidates
                                    .iter()
                                    .map(|s| s.as_str())
                                    .collect::<Vec<_>>()
                                    .join("  ")
                            )?;
                        }
                    }
                    KeyCode::Char(c)
                        if !c.is_control() && !key.modifiers.contains(KeyModifiers::CONTROL) =>
                    {
                        if line.len() < 16 * 1024 * 1024 {
                            line.push(c);
                        }
                    }
                    _ => continue,
                }
                execute!(
                    err,
                    cursor::MoveToColumn(0),
                    terminal::Clear(ClearType::CurrentLine)
                )?;
                // History may contain control characters from externally edited
                // files; never print them directly into the terminal.
                write!(
                    err,
                    "{prompt}{}",
                    line.chars().filter(|c| !c.is_control()).collect::<String>()
                )?;
                err.flush()?;
            }
        }
    }
}
