//! UniFFI multi-platform interface for BknDb, exposing the core graph database,
//! relational metadata layer, and batch sync engine to Kotlin (Android) and Swift (iOS).

pub mod engine;
pub mod error;
pub mod types;

pub use engine::BknDbEngine;
pub use error::FfiBknError;
pub use types::*;

uniffi::setup_scaffolding!();
