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
                      │  Rust • Python • Go • Kotlin/Swift  │
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
                      ┌────────────────────────────────────────┐
                      │            Storage Backends            │
                      │  ► Redb Engine (default ACID backend)  │
                      │  ► In-Memory Backend (testing/cache)   │
                      │  ► Native .bkndb LSM (optional engine) │
                      └────────────────────────────────────────┘
```

---

## Why BknDb?

1. **Embedded & Zero-Daemon:** Runs directly inside your application process (like SQLite/DuckDB). No server setup, no port configuration, zero network latency.
2. **Unified ACID Transactions:** Write relational records, graph nodes, directed edges, and search vectors in a single atomic transaction. Either everything commits, or nothing does.
3. **Single-File Native Persistence (`.bkndb`):** Powered by `redb` as the default battle-tested copy-on-write B-tree ACID storage layer for guaranteed durability, alongside an optional high-throughput append-only LSM architecture with memory-mapped zero-copy reading (`memmap2`) and WAL durability.
4. **Sub-Millisecond Graph Traversal:** Built-in BFS shortest path, Dijkstra weighted path, blast radius traversal (impact analysis), in-degree centrality (top hubs), and cascading node deletion.
5. **Built-in Full-Text & Vector Search:** Native BM25 inverted index and HNSW (Hierarchical Navigable Small World) vector index for fast semantic similarity search in the same database.
6. **Zero-Cost Hybrid Joins:** Effortlessly join graph traversal results with relational metadata tables in one read transaction.
7. **High-Volume Bulk Ingestion:** `SyncBatch` API uses block counter reservation to reduce per-record overhead when ingesting nodes, edges, and rows.
8. **Multi-Language Support:** Rust, Python (with Pandas & NetworkX support), Go via C ABI, and UniFFI Kotlin/Swift bindings. Android builds produce native libraries for app packaging; the iOS script packages an XCFramework.

---

## Performance Benchmark

These recorded results are workload-specific LSM measurements, not measurements
of the default Redb backend or performance guarantees for the current release.

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
bkndb = "0.3"
```

#### Cargo Feature Flags

| Feature | Default | Description |
| :--- | :---: | :--- |
| `redb-backend` | **Yes** | Default robust single-file ACID storage backend backed by `redb`. |
| `mem-backend` | **Yes** | Ephemeral high-throughput in-memory storage engine. |
| `graph` | **Yes** | Property graph layer, traversal algorithms, and Cypher `MATCH` queries. |
| `relational-layer` | **Yes** | Relational tables, schema constraints, query builder, and SQL engine. |
| `search` | **Yes** | Full-text search (BM25) and exact / HNSW vector similarity search. |
| `lsm-backend` | No | Optional append-only single-file LSM storage engine with zero-copy mmap. |

`BknDb::open()` selects Redb with the default features. To use LSM while
keeping the defaults, enable `lsm-backend` and call `BknDb::open_lsm()`.
Redb and LSM files have different formats even when both use `.bkndb`;
opening a file with the other backend does not migrate it.

These examples target the 0.3 API in this checkout. To use this source tree
directly, depend on `bkndb = { path = "../bkn-db/crates/bkndb" }` (adjust the path).


### Python

```bash
pip install bkndb                     # Core wheel (Windows, Linux, macOS)
pip install "bkndb[pandas,networkx]"  # With Pandas and NetworkX support
```

### Mobile (Android & iOS)

- **Android (Kotlin):** Build the native `.so` with 16KB linker alignment and package it with the generated Kotlin binding; the script does not assemble an AAR. See [scripts/build_android.ps1](scripts/build_android.ps1).
- **iOS (Swift):** Packaged XCFramework for iOS devices and simulators. See [scripts/build_ios.sh](scripts/build_ios.sh) and [bindings/bkndb_ffi.swift](bindings/bkndb_ffi.swift).

---

## Quick Start (Rust)

Use a new database file for this example; running it again would insert the
same unique email. Create parent directories before opening nested paths.

```rust
use bkndb::BknDb;
use bkndb::relational::{TableSchema, ColumnSchema, ColumnKind};
use bkndb::graph::Properties;
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
    let (alice_node, _bob_node) = db.write_tx(|b| {
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

        Ok((alice_node, bob_node))
    })?;

    // 4. Graph traversal
    let g = db.graph();
    let neighbors = g.neighbors_out(alice_node, "KNOWS")?;
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
use bkndb::TableSpec;

db.relational().sql(
    "CREATE TABLE IF NOT EXISTS audit_log (id INT PRIMARY KEY AUTOINCREMENT, event TEXT, ref_id INT)", (),
)?;
db.write_tx(|b| {
    let node = b.graph().create_node("Document", Properties::from([
        ("title".into(), "Doc 1".into()),
    ]))?;
    b.relational().table_named("audit_log")?.insert(Properties::from([
        ("event".into(), "node_created".into()),
        ("ref_id".into(), PropValue::Int(node.0 as i64)),
    ]))?;
    b.kv().put(TableSpec("settings"), b"last_updated", b"2026-10-01")?;
    Ok(())
})?;
```

If an error occurs or the closure returns `Err`, all changes are rolled back automatically.

---

### 2. Graph Algorithms & Analysis

BknDb traverses graph adjacency indexes through the selected backend.
The following fragments assume existing node IDs and an open `db`:

```rust
use bkndb::graph::Direction;

let g = db.graph();

// BFS Shortest Path (hop distance)
if let Some(path) = g.find_shortest_path(start, target, Direction::Out, Some(&["CALLS"]))? {
    println!("Shortest path: {} hops", path.edges().len());
}

// Dijkstra Weighted Shortest Path
if let Some(route) = g.find_weighted_path(start, target, Direction::Out, None, "latency_ms", 1.0)? {
    println!("Total cost: {}", route.cost);
}

// Impact Analysis (Blast Radius traversal up to 4 hops)
let callers = g.traversal().start(target).incoming("CALLS").max_depth(4).run()?;
println!("Impacted entities: {}", callers.len());

// Top Degree Hubs (Centrality Analysis)
let hubs = g.top_hubs(5, Direction::In, None)?;
for (node_id, degree) in hubs {
    println!("Hub Node {:?} with {} connections", node_id, degree);
}

// Recursive Cascading Node Deletion
let stats = g.cascade_delete(root_node, "CONTAINS")?;
println!("Deleted {} nodes and their incident edges", stats.len());
```

---

### 3. Full-Text (BM25) and Vector Search (HNSW)

BknDb includes native search capabilities built right into the relational and storage layers:

```rust
use bkndb::graph::Properties;
use bkndb::relational::{
    pack_vector, ColumnKind, ColumnSchema, TableSchema,
    VectorIndexOptions, VectorMetric, VectorSearchOptions,
};

let rel = db.relational();
let articles = TableSchema::builder("articles")
    .column(ColumnSchema::new("id", ColumnKind::Int))
    .column(ColumnSchema::new("content", ColumnKind::Str))
    .column(ColumnSchema::new("embedding", ColumnKind::Bytes))
    .primary_key("id").auto_increment().build()?;
rel.create_table(&articles)?;
rel.table(&articles).insert(Properties::from([
    ("content".into(), "Hybrid database in Rust".into()),
    ("embedding".into(), pack_vector(&[0.1, 0.8, 0.3])),
]))?;

rel.create_fulltext_index("articles", "content")?;
// true requires every query term; it does not enable highlighting.
for hit in rel.search_text("articles", "content", "hybrid rust", 5, true, None)? {
    println!("BM25: {:.2}, Row: {:?}", hit.score, hit.row);
}

rel.create_vector_index("articles", "embedding", VectorIndexOptions {
    metric: VectorMetric::Cosine, m: 16, ef_construction: 100,
})?;
let query = [0.1, 0.9, 0.2];
for hit in rel.search_vector_with(
    "articles", "embedding", &query, 5, VectorMetric::Cosine, None,
    VectorSearchOptions { exact: false, ef_search: Some(50) },
)? {
    println!("PK: {:?}, Cosine similarity: {:.4}", hit.row.pk, hit.score);
}
```

---

### 4. Zero-Cost Hybrid Joins

Join node IDs with relational metadata in one read transaction. To include
the traversal in the same snapshot, perform both inside `db.read_tx`:

```rust
let profiles = db.read_tx(|tx| {
    let friend_nodes: Vec<_> = tx.graph().neighbors_out(user_node, "FRIENDS")?
        .into_iter().map(|(node, _edge)| node).collect();
    // The profiles table's integer primary key must equal the graph node ID.
    tx.join_nodes_with_table(&friend_nodes, &profiles_schema)
})?;
for item in profiles {
    println!("Node {:?} -> Profile: {:?}", item.id, item.row);
}
```

---

### 5. High-Volume Bulk Synchronization (`SyncBatch`)

Ingest graph and relational data in one transaction. Rows are upserted
by primary key; graph entities are newly created. Node references below use
zero-based positions in the batch:

```rust
use bkndb::{NodeRef, SyncBatch};
use bkndb::graph::Properties;
use bkndb::relational::{ColumnKind, ColumnSchema, TableSchema};

let files = TableSchema::builder("files")
    .column(ColumnSchema::new("path", ColumnKind::Str))
    .column(ColumnSchema::new("lines", ColumnKind::Int))
    .primary_key("path")
    .build()?;
db.relational().create_table(&files)?;

let mut batch = SyncBatch::new();
// New node references are zero-based positions in batch.nodes.
batch.add_node("Module", Properties::from([("name".into(), "engine".into())]));
batch.add_node("Function", Properties::from([("name".into(), "start".into())]));
batch.add_linked_edge(NodeRef::New(0), "DECLARES", NodeRef::New(1), Properties::new());
batch.add_row(&files, Properties::from([
    ("path".into(), "src/engine.rs".into()),
    ("lines".into(), 350.into()),
]));

let result = db.sync_batch(batch)?;
let rows: usize = result.relational_pks.iter().map(Vec::len).sum();
println!("Committed: {} nodes, {} edges, {} rows",
    result.node_ids.len(), result.edge_ids.len(), rows);
```

---

### 6. Storage Maintenance: Compact, Backup & Verify

The `.bkndb` storage engine provides operational primitives for continuous maintenance:

```rust
// Redb-backed BknDb::open handle; compact in place.
db.compact()?;

// Destination must not already exist.
db.backup_to("app_backup.bkndb")?;

// Failure is returned as Err(BknError::Corruption(...)), not a report flag.
let report = db.verify_integrity()?;
println!("Verified {} entries", report.entries);

let stats = db.storage_stats()?; // RedbStats with the default backend
println!("File: {} bytes, Tables: {}, Stored: {} bytes, Reclaimable: {} bytes",
    stats.file_bytes, stats.table_count, stats.stored_bytes, stats.reclaimable_bytes);
```

---

## Workspace Crate Hierarchy

| Crate | Purpose |
| :--- | :--- |
| [`crates/bkndb`](crates/bkndb) | Primary top-level public facade crate (re-exports `bkndb-core`). |
| [`crates/bkndb-core`](crates/bkndb-core) | Core graph, relational, SQL/MATCH parsers, HNSW ANN, and storage abstractions. |
| [`crates/bkndb-storage-redb`](crates/bkndb-storage-redb) | Default robust single-file ACID storage engine backed by `redb`. |
| [`crates/bkndb-storage-mem`](crates/bkndb-storage-mem) | In-memory key-value backend for ephemeral databases and testing. |
| [`crates/bkndb-storage-lsm`](crates/bkndb-storage-lsm) | Optional append-only LSM storage engine (mmap zero-copy, WAL, SSTables). |
| [`crates/bkndb-ffi`](crates/bkndb-ffi) | UniFFI C/Kotlin/Swift bridge with 16KB page size compliance. |
| [`bindings/python`](bindings/python) | Python package with high-level Pythonic wrapper, Pandas & NetworkX integration. |

---

## Detailed Documentation & Guides

- **Rust Full Guide & API Reference:** [docs/wikis/wiki-rust.md](docs/wikis/wiki-rust.md)
- **Python Full Guide & API Reference:** [docs/wikis/wiki-py.md](docs/wikis/wiki-py.md)
- **Go Guide:** [docs/wikis/wiki-go.md](docs/wikis/wiki-go.md)
- **Android/Kotlin Guide:** [docs/wikis/wiki-kotlin.md](docs/wikis/wiki-kotlin.md)
- **Swift/iOS Guide:** [docs/wikis/wiki-swift.md](docs/wikis/wiki-swift.md)

---

## License

This project is licensed under the Apache License, Version 2.0 (see [LICENSE](LICENSE) or <http://www.apache.org/licenses/LICENSE-2.0>).
