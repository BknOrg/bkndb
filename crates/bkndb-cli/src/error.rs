use bkndb::BknError;
#[derive(Debug)]
pub struct Error {
    pub code: i32,
    pub message: String,
    pub details: Option<serde_json::Value>,
}
pub type Result<T> = std::result::Result<T, Error>;
impl Error {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: None,
        }
    }
    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }
    pub fn input(message: impl Into<String>) -> Self {
        Self::new(2, message)
    }
    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::new(6, message)
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.message.fmt(f)
    }
}
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::new(3, e.to_string())
    }
}
impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Self::input(e.to_string())
    }
}
impl From<BknError> for Error {
    fn from(e: BknError) -> Self {
        let code = match &e {
            BknError::Corruption(_) | BknError::Encoding(_) => 5,
            BknError::DatabaseLocked(_) => 4,
            BknError::UnsupportedFormat(_) => 6,
            BknError::RecoveryRequired(_) => 7,
            BknError::CommitUnknown(_) => 8,
            BknError::ResourceLimit(_) => 9,
            BknError::Backend(_) => 3,
            _ => 2,
        };
        Self::new(code, e.to_string())
    }
}
impl From<bkndb_engine::Error> for Error {
    fn from(e: bkndb_engine::Error) -> Self {
        use bkndb_engine::Error as E;
        let code = match &e {
            E::Corruption(_) | E::Encoding(_) => 5,
            E::DatabaseLocked(_) => 4,
            E::UnsupportedFormat(_) => 6,
            E::RecoveryRequired(_) => 7,
            E::CommitUnknown(_) => 8,
            E::ResourceLimit(_) => 9,
            E::InvalidArgument(_) => 2,
            E::Backend(_) => 3,
        };
        Self::new(code, e.to_string())
    }
}
