# bkndb-core

[![Crates.io](https://img.shields.io/crates/v/bkndb-core.svg)](https://crates.io/crates/bkndb-core)
[![Documentation](https://docs.rs/bkndb-core/badge.svg)](https://docs.rs/bkndb-core)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)

**Core primitives, hybrid transactional interface, graph engine abstractions, and relational schema layer for the `bkndb` database engine.**

`bkndb-core` provides the foundational building blocks for BknDb:
- The abstract storage traits (`StorageEngine`, `StorageReadTx`, `StorageWriteTx`).
- The property graph data model, traversal engine, and graph algorithms.
- The typed relational catalog, secondary indexing, and query builder.
- The text query language parsers (SQL subset and Cypher `MATCH`).
- Native full-text search (BM25) and approximate nearest neighbor vector search (HNSW).
- Binary sortable codecs for keys, rows, and graph topologies.

Most application developers should depend on the top-level [`bkndb`](https://crates.io/crates/bkndb) crate instead of using `bkndb-core` directly.

---

## Crate Subsystems

```
bkndb-core/src/
├── storage.rs        # StorageEngine, StorageReadTx, StorageWriteTx, TableSpec
├── db/               # Generic Db<B>, WriteBatch<B>, DbReadBatch<B>
├── graph/            # Property graph: nodes, edges, BFS, Dijkstra, top hubs, cascade
├── relational/       # Relational catalog, TableSchema, ColumnSchema, query builder
│   ├── ann/          # HNSW approximate nearest neighbor vector indexing
│   ├── search/       # Full-text inverted index with BM25 ranking
│   ├── codec.rs      # Big-endian and sortable key/value encodings
│   └── expr.rs       # Filter expressions: col("a") == 1, .is_in(), .like()
├── lang/             # Query text parsers & executors
│   ├── sql/          # SQL subset: SELECT, INSERT, UPDATE, DELETE, CREATE/DROP TABLE
│   └── graph/        # Cypher MATCH pattern parser and projection engine
├── hybrid.rs         # SyncBatch bulk sync and graph-relational join helpers
├── kv/               # Raw key-value namespace operations
└── value.rs          # PropValue, Properties, and dynamic typing
```

---

## Feature Flags

| Feature | Description |
| :--- | :--- |
| `std` *(default)* | Standard library support. |
| `value` | Enables `PropValue`, `Properties`, and `serde` / `bincode` serialization. |
| `graph` | Enables property graph engine, algorithms, and `MATCH` parser. |
| `relational` | Enables relational tables, constraints, indices, and SQL parser. |
| `search` | Enables BM25 full-text and HNSW vector search (requires `relational`). |
| `test-util` | Conformance test suites for storage engine implementers. |

---

## Implementing a Custom Storage Backend

Any key-value engine that supports ordered byte-string keys and transactional snapshots can be used as a backend for BknDb by implementing the `StorageEngine` trait:

```rust
use bkndb_core::{StorageEngine, StorageReadTx, StorageWriteTx, TableSpec, BknError};

pub struct MyCustomEngine { /* ... */ }

impl StorageEngine for MyCustomEngine {
    type ReadTx<'a> = MyReadTx<'a>;
    type WriteTx<'a> = MyWriteTx<'a>;

    fn begin_read(&self) -> Result<Self::ReadTx<'_>, BknError> {
        // Return a snapshot-isolated read transaction
        todo!()
    }

    fn begin_write(&self) -> Result<Self::WriteTx<'_>, BknError> {
        // Return an atomic write transaction
        todo!()
    }
}
```

Once implemented, you can wrap it with `Db::new(backend)` to gain access to the full graph, relational, SQL, vector, and hybrid query capabilities of BknDb.

---

## Conformance Testing

To verify that a storage backend conforms to all BknDb ACID and query requirements, use the conformance suites provided under `bkndb_core::test_util`:

```rust
#[test]
fn test_my_backend_conformance() {
    let engine = MyCustomEngine::new();
    bkndb_core::test_util::run_all_conformance_suites(&engine);
}
```

---

## License

Licensed under the Apache License, Version 2.0 (see [LICENSE](../../LICENSE)).
