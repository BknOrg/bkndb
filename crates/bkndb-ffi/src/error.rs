#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum FfiBknError {
    #[error("Storage backend error: {message}")]
    Backend { message: String },
    #[error("Table not found: {table}")]
    TableNotFound { table: String },
    #[error("Entity or key not found")]
    NotFound,
    #[error("Encoding/decoding error: {message}")]
    Encoding { message: String },
    #[error("Reserved table name '{table}'")]
    ReservedTableName { table: String },
    #[error("Database '{path}' is already open by another handle or process")]
    DatabaseLocked { path: String },
    #[error("Duplicate primary key {key} in table '{table}'")]
    DuplicateKey { table: String, key: String },
    #[error("Schema mismatch in table '{table}': {message}")]
    SchemaMismatch { table: String, message: String },
    #[error("Constraint violation in table '{table}': {message}")]
    ConstraintViolation { table: String, message: String },
    #[error("Data corruption detected: {message}")]
    Corruption { message: String },
    #[error("Invalid query: {message}")]
    InvalidQuery { message: String },
    #[error("The database has been closed")]
    DatabaseClosed,
    #[error("A transaction is open on this database; use it, or commit/roll it back first")]
    TransactionInProgress,
    #[error("The transaction has already been committed or rolled back")]
    TransactionClosed,
    #[error("An earlier operation in this transaction failed; roll it back: {message}")]
    TransactionAborted { message: String },
    #[error("Invalid argument: {message}")]
    InvalidArgument { message: String },
}

impl From<bkndb_core::BknError> for FfiBknError {
    fn from(err: bkndb_core::BknError) -> Self {
        match err {
            bkndb_core::BknError::Backend(msg) => FfiBknError::Backend { message: msg },
            bkndb_core::BknError::TableNotFound(table) => FfiBknError::TableNotFound { table },
            bkndb_core::BknError::NotFound => FfiBknError::NotFound,
            bkndb_core::BknError::Encoding(msg) => FfiBknError::Encoding { message: msg },
            bkndb_core::BknError::ReservedTableName(table) => FfiBknError::ReservedTableName { table },
            bkndb_core::BknError::DatabaseLocked(path) => FfiBknError::DatabaseLocked { path },
            bkndb_core::BknError::DuplicateKey { table, key } => FfiBknError::DuplicateKey { table, key },
            bkndb_core::BknError::SchemaMismatch { table, message } => FfiBknError::SchemaMismatch { table, message },
            bkndb_core::BknError::ConstraintViolation { table, message } => {
                FfiBknError::ConstraintViolation { table, message }
            }
            bkndb_core::BknError::Corruption(message) => FfiBknError::Corruption { message },
            bkndb_core::BknError::InvalidQuery(message) => FfiBknError::InvalidQuery { message },
        }
    }
}
