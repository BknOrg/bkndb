//! Custom LSM-Tree storage engine implementing `bkndb_core::StorageBackend`
//! — no `redb` dependency. Memtable + write-ahead log + SSTables +
//! size-tiered compaction, in the RocksDB/LevelDB family of design.
//!
//! Because it implements the same `StorageBackend` trait as
//! `bkndb-storage-redb`/`bkndb-storage-mem`, everything layered on top
//! (`bkndb_core::graph`, `bkndb_core::relational`) works against this
//! backend unchanged.
mod bloom;
mod container;
mod compaction;
mod engine;
mod keys;
mod manifest;
mod memtable;
mod sstable;
mod wal;

pub use engine::{LsmOptions, LsmStorageBackend};
