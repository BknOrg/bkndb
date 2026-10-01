# bkndb-storage-mem

[![Crates.io](https://img.shields.io/crates/v/bkndb-storage-mem.svg)](https://crates.io/crates/bkndb-storage-mem)
[![Documentation](https://docs.rs/bkndb-storage-mem/badge.svg)](https://docs.rs/bkndb-storage-mem)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)

**High-throughput, concurrent in-memory storage engine backend for BknDb.**

`bkndb-storage-mem` implements the `StorageEngine` trait using purely in-memory data structures. It provides full transactional snapshot isolation, zero disk I/O, and microsecond-level query latencies.

Ideal for:
- Fast unit testing and integration test suites.
- Ephemeral in-process graphs and temporary analytical workloads.
- High-frequency local caches.

---

## Usage

### Via Top-Level Facade (`BknDb::in_memory`)

```rust
use bkndb::BknDb;

// Create an ephemeral in-memory database
let db = BknDb::in_memory();

// Supports all relational, graph, vector, and SQL features identically to on-disk storage
let alice = db.graph().create_node("Person", Default::default())?;
```

### Standalone Usage

```rust
use bkndb_storage_mem::MemoryStorageBackend;
use bkndb_core::Db;

let backend = MemoryStorageBackend::new();
let db = Db::new(backend);
```

---

## Concurrency & Isolation

- **Concurrent Reads:** Multiple reader transactions (`begin_read()`) can run concurrently without blocking each other.
- **ACID Writes:** Write transactions (`begin_write()`) are serialized and isolated; mutations staged in memory are committed atomically or discarded on rollback.

---

## License

Licensed under the Apache License, Version 2.0 (see [LICENSE](../../LICENSE)).
