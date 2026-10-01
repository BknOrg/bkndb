# BknDb

[![Crates.io](https://img.shields.io/crates/v/bkndb.svg)](https://crates.io/crates/bkndb)
[![Documentation](https://docs.rs/bkndb/badge.svg)](https://docs.rs/bkndb)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)
[![Python Version](https://img.shields.io/pypi/pyversions/bkndb.svg)](https://pypi.org/project/bkndb/)

**An embedded, zero-daemon hybrid graph-relational-vector database engine with ACID transactions, written in Rust.**

`bkn-db` unifies relational tables (with secondary indexes and prefix matching), a property graph engine (with sub-millisecond multi-hop traversal), full-text search (BM25), and vector similarity search (HNSW ANN) into a single, unified storage layer with atomic transactions and native single-file `.bkndb` persistence.

Designed for **AI agents**, **GraphRAG**, **code intelligence**, **local-first desktop/mobile applications**, and **embedded analytics** without the operational complexity of hosting multiple databases.

---

## Architecture Overview

```
                      ┌────────────────────────────────────────┐
                      │            Client Interfaces           │
                      │  Rust Crate  •  Python  •  Kotlin/AAR  │
                      │  Swift/iOS   •  CLI (Coming Soon)      │
                      └───────────────────┬────────────────────┘
                                          │
                      ┌───────────────────▼────────────────────┐
                      │         Query & Language Layer         │
                      │   SQL Engine (SELECT, DDL, Params)     │
                      │   Cypher MATCH (Graph Patterns)        │
                      │   Hybrid Joins (Graph Node ⟕ Table)    │
                      └───────────────────┬────────────────────┘
                                          │
                      ┌───────────────────▼────────────────────┐
                      │               BknDb Core               │
                      │  Unified ACID Transactions (WriteTx)   │
                      │  Snapshot Isolation (ReadTx)           │
                      │  Property Graph  •  Relational Engine  │
                      │  Full-Text BM25  •  HNSW Vector ANN    │
                      └───────────────────┬────────────────────┘
                                          │
                      ┌───────────────────▼────────────────────┐
                      │            Storage Backends            │
                      │  ► Native .bkndb LSM (mmap2, WAL)      │
                      │  ► In-Memory Backend (testing/cache)   │
                      │  ► Redb Engine (optional backend)      │
                      └────────────────────────────────────────┘
```

---

## Why BknDb?

1. **Embedded & Zero-Daemon:** Runs directly inside your application process (like SQLite/DuckDB). No server setup, no port configuration, zero network latency.
2. **Unified ACID Transactions:** Write relational records, graph nodes, directed edges, and search vectors in a single atomic transaction. Either everything commits, or nothing does.
3. **Single-File Native Persistence (`.bkndb`):** Powered by an append-only LSM architecture with memory-mapped zero-copy reading (`memmap2`), WAL crash durability with CRC32 checksums, and adaptive MemTable buffering.
4. **Sub-Millisecond Graph Traversal:** Built-in BFS shortest path, Dijkstra weighted path, blast radius traversal (impact analysis), in-degree centrality (top hubs), and cascading node deletion.
5. **Built-in Full-Text & Vector Search:** Native BM25 inverted index and HNSW (Hierarchical Navigable Small World) vector index for fast semantic similarity search in the same database.
6. **Zero-Cost Hybrid Joins:** Effortlessly join graph traversal results with relational metadata tables in one read transaction.
7. **High-Volume Bulk Ingestion:** `SyncBatch` API capable of ingesting 100,000+ nodes, edges, and rows in under a second with block counter reservation.
8. **Multi-Language Support:** First-class Rust crate, Python package (`pip install bkndb` with Pandas & NetworkX support), and UniFFI mobile bindings (Android Kotlin AAR with 16KB page size compliance & iOS Swift XCFramework).

---

## Performance Benchmark

In real-world code intelligence workloads (call graph traversal & AST storage) comparing `bkndb` (native `.bkndb` LSM engine) against SQLite (WAL mode):

| Operation | `bkndb` (Native LSM) | SQLite (WAL mode) | Speedup |
| :--- | :---: | :---: | :---: |
| **BFS Shortest Path** (Call Graph) | **81.7 µs** | 2,912.0 µs | **35.6x faster** |
| **Blast Radius / Impact Traversal** | **41.8 µs** | 994.9 µs | **23.8x faster** |
| **Symbol Prefix Search** | **120.6 µs** | 180.7 µs | **1.50x faster** |
| **AST Bulk Sync (10,000 items)** | **123.4 ms** | 133.2 ms | **1.08x faster** |
| **HNSW Vector Search (100k vectors, 128d)** | **~5.1 ms** | ~117.0 ms (Linear Scan) | **22.9x faster** |

---

## Installation

### Rust

Add `bkndb` to your `Cargo.toml`:

```toml
[dependencies]
bkndb = "0.2"
```

#### Cargo Feature Flags

| Feature | Default | Description |
| :--- | :---: | :--- |
| `lsm-backend` | **Yes** | Native single-file `.bkndb` storage engine with zero-copy mmap. |
| `mem-backend` | **Yes** | Ephemeral high-throughput in-memory storage engine. |
| `graph` | **Yes** | Property graph layer, traversal algorithms, and Cypher `MATCH` queries. |
| `relational-layer` | **Yes** | Relational tables, schema constraints, query builder, and SQL engine. |
| `search` | **Yes** | Full-text search (BM25) and exact / HNSW vector similarity search. |
| `redb-backend` | No | Optional storage backend backed by `redb`. |

### Python

```bash
pip install bkndb                     # Core wheel (Windows, Linux, macOS)
pip install "bkndb[pandas,networkx]"  # With Pandas and NetworkX support
```

### Mobile (Android & iOS)

- **Android (Kotlin):** Pre-compiled AAR generated via UniFFI, strictly compliant with Android 15+ 16KB memory page size. See [scripts/build_android.ps1](scripts/build_android.ps1).
- **iOS (Swift):** Packaged XCFramework for iOS devices and simulators. See [scripts/build_ios.sh](scripts/build_ios.sh) and [bindings/bkndb_ffi.swift](bindings/bkndb_ffi.swift).

---

## Quick Start (Rust)

```rust
use bkndb::BknDb;
use bkndb::relational::{TableSchema, ColumnSchema, ColumnKind};
use bkndb::graph::{Direction, Properties};
use bkndb::value::PropValue;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Open or create a single-file .bkndb database
    let db = BknDb::open("app_data.bkndb")?;
    // Or for fast in-memory testing:
    // let db = BknDb::in_memory();

    // 2. Define a relational schema with constraints and index
    let users_schema = TableSchema::builder("users")
        .column(ColumnSchema::new("id", ColumnKind::Int))
        .column(ColumnSchema::new("name", ColumnKind::Str).not_null())
        .column(ColumnSchema::new("email", ColumnKind::Str).not_null().unique())
        .primary_key("id")
        .auto_increment()
        .build()?;
    db.relational().create_table(&users_schema)?;

    // 3. Atomically write relational rows and graph entities in ONE transaction
    db.write_tx(|b| {
        // Relational table insert
        let mut rel = b.relational();
        let mut row = Properties::new();
        row.insert("name".to_string(), "Alice".into());
        row.insert("email".to_string(), "alice@example.com".into());
        let alice_pk = rel.table(&users_schema).insert(row)?;

        // Property graph nodes & edges
        let mut g = b.graph();
        let alice_node = g.create_node("Person", Properties::from([
            ("name".to_string(), "Alice".into()),
            ("user_id".to_string(), alice_pk),
        ]))?;

        let bob_node = g.create_node("Person", Properties::from([
            ("name".to_string(), "Bob".into()),
        ]))?;

        g.create_edge(alice_node, "KNOWS", bob_node, Properties::from([
            ("since".to_string(), 2026.into()),
        ]))?;

        Ok(())
    })?;

    // 4. Graph traversal
    let g = db.graph();
    let neighbors = g.neighbors(bkndb::graph::NodeId(1), Direction::Out, Some("KNOWS"))?;
    println!("Alice knows {} person(s)", neighbors.len());

    // 5. Declarative Cypher-style query
    let result = g.query("MATCH (a:Person)-[:KNOWS]->(b:Person) RETURN a.name, b.name", ())?;
    for row in result.rows {
        println!("{:?} knows {:?}", row[0], row[1]);
    }

    Ok(())
}
```

---

## Quick Start (Python)

```python
import bkndb
from bkndb import TableSchema, Column, col

with bkndb.open("app_data.bkndb") as db:
    # 1. Create a relational table
    db.create_table(TableSchema(
        "users",
        [
            Column("id", int),
            Column("name", str, nullable=False),
            Column("email", str, nullable=False, unique=True),
        ],
        primary_key="id",
        auto_increment=True,
    ))

    # 2. Unified ACID transaction across graph and tables
    with db.transaction() as tx:
        user_id = tx.insert("users", {"name": "Alice", "email": "alice@example.com"})
        n1 = tx.create_node("Person", {"name": "Alice", "user_id": user_id})
        n2 = tx.create_node("Person", {"name": "Bob"})
        tx.create_edge(n1, "KNOWS", n2, {"since": 2026})

    # 3. Query using Pythonic builder or SQL
    rows = db.select("users", col("email").startswith("alice"))
    print("Found user:", rows)

    sql_res = db.sql("SELECT name, email FROM users WHERE id = ?", [1])
    print("SQL results:", sql_res.dicts())

    # 4. Graph traversal
    for hit in db.traverse(n1, max_depth=2):
        node = db.get_node(hit.node_id)
        print(f"Hop {hit.depth}: {node.label} -> {node.properties}")
```

---

## Core Feature Walkthrough

### 1. Unified Hybrid ACID Transactions

BknDb provides strict serializable writes with concurrent snapshot reads (`ReadTx`). Within `db.write_tx`, all modifications across tables, graph nodes, edges, and raw key-value pairs are staged together:

```rust
db.write_tx(|b| {
    let mut g = b.graph();
    let mut rel = b.relational();
    let mut kv = b.kv();

    let node = g.create_node("Document", Properties::from([("title".to_string(), "Doc 1".into())]))?;
    rel.table_named("audit_log")?.insert(Properties::from([
        ("event".to_string(), "node_created".into()),
        ("ref_id".to_string(), PropValue::Int(node.0 as i64)),
    ]))?;
    kv.set(b"settings", b"last_updated", b"2026-10-01")?;

    Ok(())
})?;
```

If an error occurs or the closure returns `Err`, all changes are rolled back automatically.

---

### 2. Graph Algorithms & Analysis

BknDb contains built-in graph algorithms executed directly over in-memory indexes without materializing the whole graph:

```rust
let g = db.graph();

// BFS Shortest Path (hop distance)
if let Some(path) = g.find_shortest_path(start, target, Direction::Out, Some("CALLS"))? {
    println!("Shortest path: {} hops", path.distance);
}

// Dijkstra Weighted Shortest Path
if let Some(route) = g.find_weighted_path(start, target, "latency_ms", Direction::Out, None)? {
    println!("Total cost: {}", route.cost);
}

// Impact Analysis (Blast Radius traversal up to 4 hops)
let callers = g.traversal().start(target).incoming("CALLS").max_depth(4).run()?;
println!("Impacted entities: {}", callers.len());

// Top Degree Hubs (Centrality Analysis)
let hubs = g.top_hubs(5)?;
for hub in hubs {
    println!("Hub Node {:?} with {} connections", hub.node_id, hub.degree);
}

// Recursive Cascading Node Deletion
let stats = g.cascade_delete(root_node)?;
println!("Cleaned up {} nodes and {} edges", stats.nodes_deleted, stats.edges_deleted);
```

---

### 3. Full-Text (BM25) and Vector Search (HNSW)

BknDb includes native search capabilities built right into the relational and storage layers:

```rust
use bkndb::relational::{pack_vector, VectorIndexOptions, VectorMetric};

let mut rel = db.relational();

// 1. Full-Text Search (BM25)
rel.create_fulltext_index("articles", "content")?;
let text_hits = rel.search_text("articles", "content", "hybrid database rust", 5, true, None)?;
for hit in text_hits {
    println!("Score: {:.2}, Snippet: {:?}", hit.score, hit.snippet);
}

// 2. Vector Search (HNSW Approximate Nearest Neighbor)
rel.create_vector_index("articles", "embedding", VectorIndexOptions {
    metric: VectorMetric::Cosine,
    m: 16,
    ef_construction: 100,
    ef_search: 50,
})?;

let query_embedding = vec![0.021, -0.412, 0.884, /* ... */];
let vector_hits = rel.search_vector("articles", "embedding", &query_embedding, 5, VectorMetric::Cosine, None)?;
for hit in vector_hits {
    println!("Row PK: {:?}, Cosine Distance: {:.4}", hit.row.pk, hit.distance);
}
```

---

### 4. Zero-Cost Hybrid Joins

Join graph traversal results directly with relational table metadata in a single read transaction:

```rust
// Traverse graph to find friends
let friend_nodes = db.graph().neighbors(user_node, Direction::Out, Some("FRIENDS"))?;

// Join node IDs directly with the 'profiles' table where PK == NodeId
let profiles = db.join_nodes_with_table(&friend_nodes, &profiles_schema)?;

for item in profiles {
    println!("Node {:?} -> Profile: {:?}", item.node_id, item.row);
}
```

---

### 5. High-Volume Bulk Synchronization (`SyncBatch`)

Ingest hundreds of thousands of records in a single transactional batch without per-record atomic counter overhead:

```rust
use bkndb::hybrid::{SyncBatch, SyncNode, SyncEdge, NewNode};

let mut batch = SyncBatch::new();

// Add nodes (NewNode index represents index within this batch)
let n0 = batch.add_node("Module", [("name".to_string(), "engine".into())]);
let n1 = batch.add_node("Function", [("name".to_string(), "start".into())]);

// Add edge referencing newly added nodes
batch.add_edge(NewNode(n0), "DECLARES", NewNode(n1), []);

// Add relational rows
batch.add_row("files", [
    ("path".to_string(), "src/engine.rs".into()),
    ("lines".to_string(), 350.into()),
]);

let result = db.sync_batch(batch)?;
println!("Batch committed: {} nodes, {} edges, {} rows",
    result.nodes_created.len(), result.edges_created.len(), result.rows_inserted);
```

---

### 6. Storage Maintenance: Compact, Backup & Verify

The `.bkndb` storage engine provides operational primitives for continuous maintenance:

```rust
// Reclaim dead space from deleted/overwritten SSTables
db.compact()?;

// Online hot backup while database stays open for reads/writes
db.backup_to("backups/app_backup.bkndb")?;

// Verify checksum integrity across all blocks
let report = db.verify_integrity()?;
assert!(report.is_ok());

// Inspect disk usage
let stats = db.storage_stats()?;
println!("File size: {} bytes, Reclaimable: {} bytes", stats.file_size, stats.reclaimable_bytes);
```

---

## Workspace Crate Hierarchy

| Crate | Purpose |
| :--- | :--- |
| [`crates/bkndb`](crates/bkndb) | Primary top-level public facade crate (re-exports `bkndb-core`). |
| [`crates/bkndb-core`](crates/bkndb-core) | Core graph, relational, SQL/MATCH parsers, HNSW ANN, and storage abstractions. |
| [`crates/bkndb-storage-lsm`](crates/bkndb-storage-lsm) | Native single-file `.bkndb` LSM storage engine (mmap zero-copy, WAL, SSTables). |
| [`crates/bkndb-storage-mem`](crates/bkndb-storage-mem) | In-memory key-value backend for ephemeral databases and testing. |
| [`crates/bkndb-storage-redb`](crates/bkndb-storage-redb) | Alternative persistent storage engine backed by `redb`. |
| [`crates/bkndb-ffi`](crates/bkndb-ffi) | UniFFI C/Kotlin/Swift bridge with 16KB page size compliance. |
| [`bindings/python`](bindings/python) | Python package with high-level Pythonic wrapper, Pandas & NetworkX integration. |

---

## Detailed Documentation & Guides

- **Rust Full Guide & API Reference:** [docs/wikis/wiki-rust.md](docs/wikis/wiki-rust.md)
- **Python Full Guide & API Reference:** [docs/wikis/wiki-py.md](docs/wikis/wiki-py.md)

---

## License

This project is licensed under the Apache License, Version 2.0 (see [LICENSE](LICENSE) or <http://www.apache.org/licenses/LICENSE-2.0>).
