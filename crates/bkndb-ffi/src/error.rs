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
}

impl From<bkndb_core::BknError> for FfiBknError {
    fn from(err: bkndb_core::BknError) -> Self {
        match err {
            bkndb_core::BknError::Backend(msg) => FfiBknError::Backend { message: msg },
            bkndb_core::BknError::TableNotFound(t) => FfiBknError::TableNotFound { table: t.to_string() },
            bkndb_core::BknError::NotFound => FfiBknError::NotFound,
            bkndb_core::BknError::Encoding(msg) => FfiBknError::Encoding { message: msg },
            bkndb_core::BknError::ReservedTableName(t) => FfiBknError::ReservedTableName { table: t.to_string() },
            bkndb_core::BknError::DatabaseLocked(path) => FfiBknError::DatabaseLocked { path },
            bkndb_core::BknError::DuplicateKey { table, key } => FfiBknError::DuplicateKey { table: table.to_string(), key },
            bkndb_core::BknError::SchemaMismatch { table, message } => {
                FfiBknError::SchemaMismatch { table: table.to_string(), message }
            }
        }
    }
}
