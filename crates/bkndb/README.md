# bkndb

[![Crates.io](https://img.shields.io/crates/v/bkndb.svg)](https://crates.io/crates/bkndb)
[![Documentation](https://docs.rs/bkndb/badge.svg)](https://docs.rs/bkndb)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)

**An embedded, zero-daemon hybrid graph-relational-vector database engine with ACID transactions, written in Rust.**

`bkndb` unifies relational tables (with secondary indexes and prefix matching), a property graph engine (with sub-millisecond graph traversal), full-text search (BM25), and vector similarity search (HNSW ANN) into a single, unified storage layer with atomic transactions and native single-file `.bkndb` container persistence.

Designed for **AI agents**, **GraphRAG**, **code intelligence**, **local-first desktop/mobile applications**, and **embedded analytics**.

---

## Key Features

- **Embedded & Zero-Daemon:** Runs directly inside your application process (like SQLite/DuckDB). No server setup, no port configuration, zero network latency.
- **Unified Hybrid ACID Transactions:** Read and write relational rows and property graph nodes/edges in a single atomic transaction with snapshot isolation (`db.write_tx` / `db.read_tx`).
- **High-Performance Native Storage (.bkndb):** Single-file container powered by `redb` as the default copy-on-write B-tree ACID engine, alongside an optional append-only LSM architecture with memory-mapped zero-copy reading (`memmap2`) and WAL crash durability.
- **Sub-Millisecond Graph Traversal:** Built-in BFS shortest path, Dijkstra weighted shortest path, multi-hop radius traversal (blast radius), in-degree centrality (top hubs), and cascading node deletion.
- **Integrated Full-Text & Vector Search:** Native BM25 inverted index and HNSW (Hierarchical Navigable Small World) vector index for fast semantic similarity search in the same database.
- **Relational Indexing & Hybrid Joins:** Secondary indexing, fast prefix search, and zero-cost joining of graph node traversals with relational metadata tables.
- **High-Volume Bulk Ingestion:** Batch synchronization API (`SyncBatch`) capable of ingesting 100,000+ nodes, edges, and rows in under a second.
- **Cross-Platform:** Rust core with Python, Go, and UniFFI Kotlin/Swift bindings. The Android script builds native `.so` libraries with 16KB linker alignment; the iOS script packages an XCFramework.

---

## Performance Highlights

In real-world code intelligence workloads (call graph traversal & AST storage) comparing `bkndb` (native `.bkndb` LSM engine) against SQLite (WAL mode):

| Operation | `bkndb` (Native LSM) | SQLite (WAL mode) | Speedup |
| :--- | :---: | :---: | :---: |
| **BFS Shortest Path** (Call Graph) | **81.7 µs** | 2,912.0 µs | **35.6x faster** |
| **Blast Radius / Impact Traversal** | **41.8 µs** | 994.9 µs | **23.8x faster** |
| **Symbol Prefix Search** | **120.6 µs** | 180.7 µs | **1.50x faster** |
| **AST Batch Sync (10,000 items)** | **123.4 ms** | 133.2 ms | **1.08x faster** |
| **HNSW Vector Search (100k vectors, 128d)** | **~5.1 ms** | ~117.0 ms (Linear Scan) | **22.9x faster** |

---

## Installation

Add `bkndb` to your `Cargo.toml`:

```toml
[dependencies]
bkndb = "0.3"
```

### Cargo Feature Flags

| Feature | Default | Description |
| :--- | :---: | :--- |
| `redb-backend` | **Yes** | Default robust single-file ACID storage backend powered by `redb`. |
| `mem-backend` | **Yes** | High-throughput in-memory storage engine. |
| `graph` | **Yes** | Property graph layer, traversal algorithms, and Cypher `MATCH` queries. |
| `relational-layer` | **Yes** | Relational tables, primary keys, secondary indices, and prefix queries. |
| `search` | **Yes** | Full-text search (BM25) and exact / HNSW vector similarity search. |
| `lsm-backend` | No | Optional append-only single-file LSM storage engine with zero-copy mmap. |

`BknDb::open()` selects Redb with the default features. To use LSM while
keeping the defaults, enable `lsm-backend` and call `BknDb::open_lsm()`.
Redb and LSM files have different formats even when both use `.bkndb`;
opening a file with the other backend does not migrate it.

These examples target the 0.3 API in this checkout. To use this source tree
directly, depend on `bkndb = { path = "../bkn-db/crates/bkndb" }` (adjust the path).


Example for a minimal in-memory graph-only build:
```toml
bkndb = { version = "0.3", default-features = false, features = ["mem-backend", "graph"] }
```

---

## Quick Start Guide

### 1. Opening a Database

```rust
use bkndb::BknDb;

// Persistent single-file database (.bkndb)
let db = BknDb::open("app_data.bkndb")?;

// Or in-memory database for fast testing or caching
let mem_db = BknDb::in_memory();
```

---

### 2. Defining Schemas and Relational Tables

```rust
use bkndb::relational::{TableSchema, ColumnSchema, ColumnKind};

let schema = TableSchema::builder("users")
    .column(ColumnSchema::new("id", ColumnKind::Int))
    .column(ColumnSchema::new("name", ColumnKind::Str).not_null())
    .column(ColumnSchema::new("email", ColumnKind::Str).not_null().unique())
    .column(ColumnSchema::new("role", ColumnKind::Str).default_value("member"))
    .primary_key("id")
    .auto_increment()
    .index("role")
    .build()?;

db.relational().create_table(&schema)?;
```

---

### 3. Unified ACID Write Transactions

Mutate relational rows and property graph entities atomically in a single transaction:

```rust
use bkndb::graph::Properties;
use bkndb::value::PropValue;

db.write_tx(|b| {
    // 1. Relational Table Write
    let mut rel = b.relational();
    let mut user_row = Properties::new();
    user_row.insert("name".to_string(), "Alice".into());
    user_row.insert("email".to_string(), "alice@example.com".into());
    let user_pk = rel.table(&schema).insert(user_row)?;

    // 2. Graph Node & Edge Creation
    let mut g = b.graph();
    let n_alice = g.create_node("Person", Properties::from([
        ("name".to_string(), "Alice".into()),
        ("user_id".to_string(), user_pk),
    ]))?;

    let n_bob = g.create_node("Person", Properties::from([
        ("name".to_string(), "Bob".into()),
    ]))?;

    g.create_edge(n_alice, "COLLABORATES_WITH", n_bob, Properties::from([
        ("since".to_string(), 2026.into()),
    ]))?;

    Ok(())
})?;
```

---

### 4. Graph Traversals & Graph Algorithms

```rust
use bkndb::graph::{Direction, NodeId};

let g = db.graph();

// Outgoing neighbors
let colleagues = g.neighbors_out(NodeId(1), "COLLABORATES_WITH")?;
println!("Alice has {} collaborator(s)", colleagues.len());

// Shortest Path (BFS)
if let Some(path) = g.find_shortest_path(NodeId(1), NodeId(2), Direction::Out, None)? {
    println!("Shortest path: {} hops", path.edges().len());
    for step in path.steps {
        println!("  Node {:?}, via edge {:?}, type {:?}", step.node, step.via_edge, step.edge_type);
    }
}

// Impact / Blast Radius Traversal
let impacted = g.traversal().start(NodeId(2)).incoming("COLLABORATES_WITH").max_depth(3).run()?;
println!("Total impacted nodes: {}", impacted.len());

// Centrality (Top Hubs)
let hubs = g.top_hubs(5, Direction::In, None)?;
for (node_id, degree) in hubs {
    println!("Hub: Node {:?}, Degree: {}", node_id, degree);
}

// Recursive Cascading Node Deletion
let cleanup_stats = g.cascade_delete(NodeId(2), "CONTAINS")?;
println!("Deleted {} nodes and their incident edges", cleanup_stats.len());
```

---

### 5. Declarative Query Languages (SQL & MATCH)

#### SQL Execution
```rust
let res = db.relational().sql(
    "SELECT name, email FROM users WHERE role = ? ORDER BY id DESC LIMIT 10",
    vec!["member".into()],
)?;
for row in res.rows {
    println!("User: {:?}, Email: {:?}", row[0], row[1]);
}
```

#### Cypher MATCH Execution
```rust
let res = db.graph().query(
    "MATCH (a:Person)-[:COLLABORATES_WITH]->(b:Person) RETURN a.name, b.name",
    (),
)?;
for row in res.rows {
    println!("{:?} -> {:?}", row[0], row[1]);
}
```

---

### 6. Full-Text (BM25) and Vector Search (HNSW)

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

### 7. Zero-Cost Hybrid Joins

Join graph traversal results directly with relational table metadata in a single read transaction:

```rust
let nodes = vec![NodeId(1), NodeId(2)];
let joined = db.join_nodes_with_table(&nodes, &schema)?;

for item in joined {
    println!("Node {:?} -> Row: {:?}", item.id, item.row);
}
```

---

### 8. High-Performance Bulk Synchronization (`SyncBatch`)

Ingest hundreds of thousands of records in a single atomic transaction:

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

### 9. Storage Maintenance & Operations

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

## Detailed Documentation

For exhaustive API documentation, performance recipes, and advanced patterns, refer to:
- **[wiki-rust.md](../../docs/wikis/wiki-rust.md)**: Rust user guide for the 0.3 API.
- **[wiki-py.md](../../docs/wikis/wiki-py.md)**: Complete Python user guide.

---

## License

Licensed under the Apache License, Version 2.0 (see [LICENSE](../../LICENSE)).
