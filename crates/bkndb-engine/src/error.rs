use std::fmt;
/// Storage failures with explicit recovery/commit semantics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BknError {
    Backend(String),
    Encoding(String),
    Corruption(String),
    DatabaseLocked(String),
    ResourceLimit(String),
    UnsupportedFormat(String),
    InvalidArgument(String),
    RecoveryRequired(String),
    CommitUnknown(String),
}
impl fmt::Display for BknError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for BknError {}
pub type Error = BknError;
