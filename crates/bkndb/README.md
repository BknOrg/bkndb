# bkndb

[![Crates.io](https://img.shields.io/crates/v/bkndb.svg)](https://crates.io/crates/bkndb)
[![Documentation](https://docs.rs/bkndb/badge.svg)](https://docs.rs/bkndb)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)

**An embedded, zero-daemon hybrid graph-relational database engine with ACID transactions, written in Rust.**

`bkndb` unifies relational tables (with secondary indexes and prefix matching) and a property graph engine into a single storage layer with unified atomic transactions, sub-millisecond graph traversal, and native single-file `.bkndb` container persistence.

Designed for AI agents, GraphRAG, code intelligence tools, local-first applications, and embedded analytics.

---

## Key Features

- **Embedded & In-Process:** Zero network overhead, runs directly inside your application process (like SQLite/libsqlite3).
- **Unified Hybrid ACID Transactions:** Read and write relational rows and property graph nodes/edges in a single atomic transaction with snapshot isolation (`db.write_tx` / `db.read_tx`).
- **High-Performance Native Storage (.bkndb):** Single-file container using an append-only LSM architecture with memory-mapped zero-copy reading (`memmap2`) and MemTable buffering.
- **Sub-Millisecond Graph Traversal:** Built-in BFS shortest path, multi-hop radius traversal (blast radius), in-degree centrality (top hubs), and cascading node deletion.
- **Relational Indexing & Hybrid Join:** Secondary indexing, fast prefix search, and zero-cost joining of graph node traversals with relational metadata tables.
- **High-Volume Bulk Ingestion:** Batch synchronization API (`SyncBatch`) capable of ingesting 100,000+ nodes, edges, and rows in under a second.
- **Zero Heavy Dependencies:** Core uses no heavy C/C++ runtimes.

---

## Performance Highlights

In real-world code intelligence workloads (call graph traversal & AST storage) comparing `bkndb` against SQLite:

| Operation | `bkndb` (Native LSM) | SQLite (WAL mode) | Speedup |
|---|:---:|:---:|:---:|
| **BFS Shortest Path** (Call Graph) | **81.7 µs** | 2,912.0 µs | **35.6x faster** |
| **Blast Radius / Impact Traversal** | **41.8 µs** | 994.9 µs | **23.8x faster** |
| **AST Batch Sync (10k items)** | **123.4 ms** | 133.2 ms | **1.08x faster** |

---

## Installation

Add `bkndb` to your `Cargo.toml`:

```toml
[dependencies]
bkndb = "0.1"
```

### Feature Flags

| Feature | Default | Description |
|---|:---:|---|
| `lsm-backend` | **Yes** | Native single-file `.bkndb` container with zero-copy mmap. |
| `mem-backend` | **Yes** | High-throughput in-memory storage engine. |
| `graph` | **Yes** | Property graph layer (nodes, directed edges, graph algorithms). |
| `relational-layer` | **Yes** | Relational tables, primary keys, secondary indices, and prefix queries. |
| `redb-backend` | No | Optional storage backend powered by `redb`. |

---

## Quick Start

### 1. Basic Graph and Relational Usage

```rust
use bkndb::BknDb;
use bkndb::relational::{RelSchema, ColumnDef, ColumnKind};
use bkndb::value::PropValue;
use bkndb::graph::{Direction, Properties};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Open or create a persistent database file
    let db = BknDb::open("app_data.bkndb")?;
    // Or for ephemeral/testing use:
    // let db = BknDb::in_memory()?;

    // 2. Define a relational schema
    let files_schema = RelSchema {
        name: "files",
        columns: &[
            ColumnDef { name: "id", kind: ColumnKind::Int },
            ColumnDef { name: "path", kind: ColumnKind::Str },
            ColumnDef { name: "hash", kind: ColumnKind::Str },
        ],
        primary_key: "id",
        auto_increment_pk: true,
        indexed_columns: &["path"],
    };

    // 3. Atomically write relational rows and graph entities in ONE transaction
    db.write_tx(|batch| {
        // Relational insert
        let mut rel = batch.relational();
        let mut props = Properties::new();
        props.insert("path".to_string(), PropValue::Str("src/main.rs".to_string()));
        props.insert("hash".to_string(), PropValue::Str("a1b2c3d4".to_string()));
        let file_pk = rel.table(&files_schema).insert(props)?;

        // Graph entities
        let mut g = batch.graph();
        let mut main_props = Properties::new();
        main_props.insert("name".to_string(), PropValue::Str("main".to_string()));
        main_props.insert("file_id".to_string(), PropValue::Int(file_pk.as_int().unwrap()));
        let main_node = g.create_node("Function", main_props)?;

        let mut helper_props = Properties::new();
        helper_props.insert("name".to_string(), PropValue::Str("init_config".to_string()));
        let helper_node = g.create_node("Function", helper_props)?;

        // Directed edge: main() calls init_config()
        g.create_edge(main_node, "CALLS", helper_node, Properties::new())?;

        Ok(())
    })?;

    // 4. Query graph traversal
    let g = db.graph();
    let outgoing = g.neighbors(bkndb::graph::NodeId(1), Direction::Out, Some("CALLS"))?;
    println!("Node 1 calls {} function(s)", outgoing.len());

    Ok(())
}
```

### 2. Graph Algorithms: Shortest Path & Blast Radius

```rust
use bkndb::BknDb;
use bkndb::graph::{Direction, NodeId};

fn analyze(db: &BknDb, start: NodeId, target: NodeId) -> Result<(), Box<dyn std::error::Error>> {
    let g = db.graph();

    // BFS shortest path (e.g. call path analysis)
    if let Some(path) = g.find_shortest_path(start, target, Direction::Out, Some("CALLS"))? {
        println!("Shortest path length: {} hops", path.distance);
        for step in path.steps {
            println!("  Node {:?} --[{}]--> Node {:?}", step.from, step.edge_type, step.to);
        }
    }

    // Impact / blast radius analysis (traverse up to 3 hops)
    let impact = g.traverse_radius(target, Direction::In, 3, Some("CALLS"))?;
    println!("Entities impacted by changes to target: {}", impact.len());

    Ok(())
}
```

---

## Multi-Language Ecosystem

`bkn-db` also provides bindings for other languages:
- **Python**: `bindings/python/` (supports GraphRAG & AI memory structures)
- **Swift / iOS**: `bindings/bkndb_ffi.swift`
- **Kotlin / Android**: `bindings/uniffi/bkndb_ffi/` (16KB memory page size compliant)

---

## License

Licensed under the Apache License, Version 2.0 ([LICENSE](https://github.com/BknOrg/bkn-db/blob/main/LICENSE) or <http://www.apache.org/licenses/LICENSE-2.0>).
