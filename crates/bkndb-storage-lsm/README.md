# bkndb-storage-lsm

[![Crates.io](https://img.shields.io/crates/v/bkndb-storage-lsm.svg)](https://crates.io/crates/bkndb-storage-lsm)
[![Documentation](https://docs.rs/bkndb-storage-lsm/badge.svg)](https://docs.rs/bkndb-storage-lsm)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)

**Native single-file Log-Structured Merge-tree (LSM) storage engine with memory-mapped zero-copy reading (`memmap2`) for BknDb.**

`bkndb-storage-lsm` is the default on-disk storage engine for `bkndb`. It packs all transactional data, graph topology, relational tables, and vector indexes into a single proprietary `.bkndb` container file with high write throughput, sub-millisecond point lookups, and minimal memory overhead.

---

## Architectural Highlights

- **Single-File Container:** Everything resides within one `.bkndb` file. No directory sprawl, no multiple log files left behind.
- **Append-Only Write-Ahead Log (WAL):** ACID write transactions are committed sequentially to the WAL with CRC32 checksum verification and hardware `fsync` durability.
- **MemTable Buffering:** Active writes are buffered in an in-memory sorted MemTable (default 16MB threshold) before being flushed into immutable SSTables.
- **Zero-Copy Memory-Mapped Reading (`memmap2`):** SSTable reads bypass operating system buffer copies and user-space allocations, delivering instantaneous lookups directly from the OS page cache.
- **Bloom Filters:** Fast probabilistic negative-lookup checks (`bloom.rs`) prevent unnecessary disk reads for non-existent keys.
- **LZ4 Compression:** Transparent block compression via `lz4_flex` reduces disk footprint while maintaining ultra-low CPU decompression latency.
- **Online Hot Backup:** `backup_to(dest)` creates a consistent, compacted snapshot of the database while concurrent reads and writes continue uninterrupted.
- **CRC32 Checksum Integrity Verification:** `verify_integrity()` validates the checksum of every block in the container to detect silent disk corruption.

---

## Configuration (`LsmOptions`)

You can customize the storage engine parameters when opening a database:

```rust
use bkndb_storage_lsm::{LsmOptions, LsmStorageBackend};
use bkndb_core::Db;

let options = LsmOptions {
    memtable_flush_bytes: 32 * 1024 * 1024, // 32MB MemTable buffer for bulk writes
    compaction_trigger_files: 8,             // Trigger compaction after 8 SSTables
    block_size: 4096,                        // 4KB SSTable block size
    ..Default::default()
};

let backend = LsmStorageBackend::open_with_options("app_data.bkndb", options)?;
let db = Db::new(backend);
```

---

## Storage Operations

### 1. Compaction
Reclaims disk space occupied by deleted records (tombstones) and older overwritten versions:

```rust
backend.force_compact()?;
```

### 2. Hot Online Backup
Creates an atomic, consistent point-in-time copy to a new file:

```rust
backend.backup_to("backups/snapshot_20261001.bkndb")?;
```

### 3. Checksum Verification
Scans the entire database file and verifies all CRC32 checksums:

```rust
let report = backend.verify_integrity()?;
if report.is_ok() {
    println!("Database is 100% healthy!");
}
```

---

## License

Licensed under the Apache License, Version 2.0 (see [LICENSE](../../LICENSE)).
