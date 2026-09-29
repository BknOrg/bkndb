//! UniFFI multi-language interface for BknDb (Kotlin, Swift and Python),
//! exposing the graph layer, the relational layer (runtime schemas, queries,
//! aggregates), explicit transactions, and the batch sync engine.

pub mod engine;
pub mod error;
mod ops;
pub mod relational;
pub mod transaction;
pub mod types;

pub use engine::BknDbEngine;
pub use error::FfiBknError;
pub use relational::*;
pub use transaction::BknDbTransaction;
pub use types::*;

uniffi::setup_scaffolding!();
