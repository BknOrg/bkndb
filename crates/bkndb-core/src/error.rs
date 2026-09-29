use std::fmt;

#[derive(Debug)]
pub enum BknError {
    Backend(String),
    TableNotFound(String),
    NotFound,
    Encoding(String),
    ReservedTableName(String),
    /// Another handle (in this or another process) already has the
    /// database file open for writing. Carries the path that was locked.
    DatabaseLocked(String),
    /// An insert with an explicit primary key collided with an existing row.
    DuplicateKey { table: String, key: String },
    /// A row's value doesn't match its declared column kind, or names a
    /// column the schema doesn't declare.
    SchemaMismatch { table: String, message: String },
    /// A NOT NULL or UNIQUE constraint would be violated.
    ConstraintViolation { table: String, message: String },
    /// Stored data failed an integrity check (checksum mismatch, truncated
    /// or malformed on-disk structure). Never retried or silently skipped.
    Corruption(String),
    /// A query-language statement (SQL, graph pattern) failed to parse or
    /// doesn't make sense — carries a message pointing at the problem.
    InvalidQuery(String),
}

impl fmt::Display for BknError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BknError::Backend(msg) => write!(f, "storage backend error: {msg}"),
            BknError::TableNotFound(name) => write!(f, "table not found: {name}"),
            BknError::NotFound => write!(f, "key not found"),
            BknError::Encoding(msg) => write!(f, "encoding error: {msg}"),
            BknError::ReservedTableName(name) => write!(
                f,
                "table name '{name}' is reserved for bkndb-core's internal use (one of: {})",
                crate::RESERVED_TABLE_NAMES.join(", ")
            ),
            BknError::DatabaseLocked(path) => {
                write!(f, "database '{path}' is already open by another handle or process")
            }
            BknError::DuplicateKey { table, key } => {
                write!(f, "duplicate primary key {key} in table '{table}'")
            }
            BknError::SchemaMismatch { table, message } => {
                write!(f, "schema mismatch in table '{table}': {message}")
            }
            BknError::ConstraintViolation { table, message } => {
                write!(f, "constraint violation in table '{table}': {message}")
            }
            BknError::Corruption(msg) => write!(f, "data corruption detected: {msg}"),
            BknError::InvalidQuery(msg) => write!(f, "invalid query: {msg}"),
        }
    }
}

impl std::error::Error for BknError {}
