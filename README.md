# BknDb

[![Crates.io](https://img.shields.io/crates/v/bkndb.svg)](https://crates.io/crates/bkndb)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)

An embedded hybrid graph-relational database engine with ACID transactions, written in Rust.

`bkn-db` unifies relational tables (with secondary indexes and prefix matching) and a property graph engine into a single storage layer with unified atomic transactions, sub-millisecond graph traversal, and native single-file `.bkndb` container persistence.

---

## Features

- **Embedded & In-Process:** Zero network overhead, runs directly inside your application process (like SQLite/libsqlite3).
- **Unified Hybrid ACID Transactions:** Read and write relational rows and property graph nodes/edges in a single atomic transaction with snapshot isolation (`Db::write_tx` / `Db::read_tx`).
- **High-Performance Native Storage (.bkndb):** Single-file container using an append-only LSM architecture with memory-mapped zero-copy reading (`memmap2`) and MemTable buffering.
- **Sub-Millisecond Graph Traversal:** Built-in BFS shortest path, multi-hop radius traversal (blast radius), in-degree centrality (top hubs), and cascading node deletion.
- **Relational Indexing & Hybrid Join:** Secondary indexing, fast prefix search, and zero-cost joining of graph node traversals with relational metadata tables.
- **High-Volume Bulk Ingestion:** Batch synchronization API (`SyncBatch`) capable of ingesting 100,000 nodes, edges, and rows in under a second.
- **Cross-Platform:** Pure Rust core, with UniFFI mobile bindings (Android Kotlin AAR with 16KB page size compliance & iOS Swift XCFramework) and Python bindings.

---

## Quick Start

Add `bkndb` to your `Cargo.toml`:

```toml
[dependencies]
bkndb = "0.1"
```

### Basic Example

```rust
use bkndb::BknDb;
use bkndb::relational::{RelSchema, ColumnDef, ColumnKind};
use bkndb::value::PropValue;
use bkndb::graph::{Direction, Properties};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Open or create a single-file .bkndb database
    let db = BknDb::open("my_data.bkndb")?;

    // 2. Define a relational schema
    let users_schema = RelSchema {
        name: "users",
        columns: &[
            ColumnDef { name: "id", kind: ColumnKind::Int },
            ColumnDef { name: "name", kind: ColumnKind::Str },
            ColumnDef { name: "email", kind: ColumnKind::Str },
        ],
        primary_key: "id",
        auto_increment_pk: true,
        indexed_columns: &["email"],
    };

    // 3. Atomically write relational rows and graph entities in ONE transaction
    db.write_tx(|batch| {
        // Relational insert
        let mut rel = batch.relational();
        let mut props = Properties::new();
        props.insert("name".to_string(), PropValue::Str("Alice".to_string()));
        props.insert("email".to_string(), PropValue::Str("alice@example.com".to_string()));
        let alice_pk = rel.table(&users_schema).insert(props)?;

        // Graph nodes & edges
        let mut g = batch.graph();
        let mut node_props = Properties::new();
        node_props.insert("name".to_string(), PropValue::Str("Alice".to_string()));
        let alice_node = g.create_node("Person", node_props)?;

        let mut bob_props = Properties::new();
        bob_props.insert("name".to_string(), PropValue::Str("Bob".to_string()));
        let bob_node = g.create_node("Person", bob_props)?;

        g.create_edge(alice_node, "KNOWS", bob_node, Properties::new())?;

        Ok(())
    })?;

    // 4. Query graph traversal
    let g = db.graph();
    let neighbors = g.neighbors(bkndb::graph::NodeId(1), Direction::Out, Some("KNOWS"))?;
    println!("Found {} connections", neighbors.len());

    Ok(())
}
```

---

## License

This project is licensed under the Apache License, Version 2.0 (see [LICENSE](LICENSE)).
