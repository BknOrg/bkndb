//! Independent transactional LSM key-value engine. SQL, graph and search live
//! in other crates; native and capability-based hosts share this format.
mod bloom;
mod compaction;
mod container;
mod engine;
mod keys;
mod manifest;
mod memtable;
mod sstable;
mod wal;

pub use engine::{
    Diagnostics, SstableDiagnostic, IntegrityReport, LsmOptions, LsmReadTx, LsmStats, LsmStorageBackend, LsmWriteTx, RepairReport,
};

mod error;
mod storage;
pub use error::{BknError, Error};
pub use storage::{KvIter, KvPairs, StorageBackend, StorageReadTx, StorageWriteTx, TableSpec};
pub type Engine = LsmStorageBackend;

pub mod io;

mod decode;

#[cfg(feature = "test-io")]
pub mod testing;

#[cfg(feature = "fuzzing")]
pub mod fuzzing;
