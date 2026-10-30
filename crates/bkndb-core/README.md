# bkndb-core

[![Crates.io](https://img.shields.io/crates/v/bkndb-core.svg)](https://crates.io/crates/bkndb-core)
[![Documentation](https://docs.rs/bkndb-core/badge.svg)](https://docs.rs/bkndb-core)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)

**Core primitives, hybrid transactional interface, graph engine abstractions, and relational schema layer for the `bkndb` database engine.**

`bkndb-core` provides the foundational building blocks for BknDb:
- The abstract storage traits (`StorageBackend`, `StorageReadTx`, `StorageWriteTx`).
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
├── storage.rs        # StorageBackend, StorageReadTx, StorageWriteTx, TableSpec
├── db/               # Generic Db<B>, DbWriteBatch<W>, DbReadBatch<R>
├── graph/            # Property graph: nodes, edges, BFS, Dijkstra, top hubs, cascade
├── relational/       # Relational catalog, TableSchema, ColumnSchema, query builder
│   ├── ann/          # HNSW approximate nearest neighbor vector indexing
│   ├── search/       # Full-text inverted index with BM25 ranking
│   ├── codec.rs      # Big-endian and sortable key/value encodings
│   └── expr.rs       # Filter expressions: col("a").eq(1), .is_in(), .like()
├── lang/             # Query text parsers & executors
│   ├── sql/          # SQL subset: SELECT, INSERT, UPDATE, DELETE, CREATE/DROP TABLE
│   └── graph/        # Cypher MATCH pattern parser and projection engine
├── hybrid.rs         # Graph-relational join helpers; bulk sync is in db/sync.rs
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

Any key-value engine that supports ordered byte-string keys and transactional snapshots can be used as a backend for BknDb by implementing the `StorageBackend` trait:

The skeleton below assumes `MyReadTx` / `MyWriteTx` implement the read/write
traits and `MyCustomEngine` is `Send + Sync`.

```text
use bkndb_core::{StorageBackend, StorageReadTx, StorageWriteTx, TableSpec, BknError};

pub struct MyCustomEngine { /* ... */ }

impl StorageBackend for MyCustomEngine {
    type ReadTx<'a> = MyReadTx<'a> where Self: 'a;
    type WriteTx<'a> = MyWriteTx<'a> where Self: 'a;

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

Once implemented, wrap it with `Db::new(backend)`. Enable `graph`,
`relational`, and `search` as needed; `bkndb-core` defaults to `std` only.
The `db/sync.rs` module provides `SyncBatch`, `NodeRef`, and `SyncBatchResult`.

---

## Conformance Testing

To verify that a storage backend conforms to the raw KV transaction contract, use the conformance suites provided under `bkndb_core::test_util`:

Enable the `test-util` feature on `bkndb-core`. This example assumes your backend type is defined:

```text
#[test]
fn test_my_backend_conformance() {
    let engine = MyCustomEngine::new();
    bkndb_core::test_util::conformance_suite(&engine);
}
```

---

Additional graph, relational, search, and hybrid suites are exported from
`test_util`; see the existing backend conformance tests for the matching setup.

## License

Licensed under the Apache License, Version 2.0 (see [LICENSE](../../LICENSE)).
