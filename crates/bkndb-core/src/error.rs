use std::fmt;

#[derive(Debug)]
pub enum BknError {
    Backend(String),
    TableNotFound(&'static str),
    NotFound,
    Encoding(String),
    ReservedTableName(&'static str),
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
                "table name '{name}' is reserved for bkndb-core's internal use (one of: nodes, edges, adj_out, adj_in, meta)"
            ),
        }
    }
}

impl std::error::Error for BknError {}
