use crate::BknError;
use bincode::Options;
use serde::de::DeserializeOwned;
pub fn bounded<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, BknError> {
    // Explicit fixed-width LE encoding preserves files from bincode::serialize.
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(bytes.len() as u64)
        .reject_trailing_bytes()
        .deserialize(bytes)
        .map_err(|e| BknError::Corruption(format!("invalid encoded data: {e}")))
}
