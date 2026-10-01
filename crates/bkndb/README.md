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
- **High-Performance Native Storage (.bkndb):** Single-file container using an append-only LSM architecture with memory-mapped zero-copy reading (`memmap2`), WAL crash durability, and adaptive MemTable buffering.
- **Sub-Millisecond Graph Traversal:** Built-in BFS shortest path, Dijkstra weighted shortest path, multi-hop radius traversal (blast radius), in-degree centrality (top hubs), and cascading node deletion.
- **Integrated Full-Text & Vector Search:** Native BM25 inverted index and HNSW (Hierarchical Navigable Small World) vector index for fast semantic similarity search in the same database.
- **Relational Indexing & Hybrid Joins:** Secondary indexing, fast prefix search, and zero-cost joining of graph node traversals with relational metadata tables.
- **High-Volume Bulk Ingestion:** Batch synchronization API (`SyncBatch`) capable of ingesting 100,000+ nodes, edges, and rows in under a second.
- **Cross-Platform:** Pure Rust core, with UniFFI mobile bindings (Android Kotlin AAR with 16KB page size compliance & iOS Swift XCFramework) and Python bindings (`pip install bkndb`).

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
bkndb = "0.2"
```

### Cargo Feature Flags

| Feature | Default | Description |
| :--- | :---: | :--- |
| `lsm-backend` | **Yes** | Native single-file `.bkndb` container with zero-copy mmap. |
| `mem-backend` | **Yes** | High-throughput in-memory storage engine. |
| `graph` | **Yes** | Property graph layer, traversal algorithms, and Cypher `MATCH` queries. |
| `relational-layer` | **Yes** | Relational tables, primary keys, secondary indices, and prefix queries. |
| `search` | **Yes** | Full-text search (BM25) and exact / HNSW vector similarity search. |
| `redb-backend` | No | Optional storage backend powered by `redb`. |

Example for a minimal in-memory graph-only build:
```toml
bkndb = { version = "0.2", default-features = false, features = ["mem-backend", "graph"] }
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
let colleagues = g.neighbors(NodeId(1), Direction::Out, Some("COLLABORATES_WITH"))?;
println!("Alice has {} collaborator(s)", colleagues.len());

// Shortest Path (BFS)
if let Some(path) = g.find_shortest_path(NodeId(1), NodeId(2), Direction::Out, None)? {
    println!("Shortest path: {} hops", path.distance);
    for step in path.steps {
        println!("  {:?} --[{}]--> {:?}", step.from, step.edge_type, step.to);
    }
}

// Impact / Blast Radius Traversal
let impacted = g.traversal().start(NodeId(2)).incoming("COLLABORATES_WITH").max_depth(3).run()?;
println!("Total impacted nodes: {}", impacted.len());

// Centrality (Top Hubs)
let hubs = g.top_hubs(5)?;
for hub in hubs {
    println!("Hub: Node {:?}, Degree: {}", hub.node_id, hub.degree);
}

// Recursive Cascading Node Deletion
let cleanup_stats = g.cascade_delete(NodeId(2))?;
println!("Deleted {} nodes, {} edges", cleanup_stats.nodes_deleted, cleanup_stats.edges_deleted);
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
use bkndb::relational::{pack_vector, VectorIndexOptions, VectorMetric};

let mut rel = db.relational();

// Full-Text Search (BM25)
rel.create_fulltext_index("documents", "content")?;
let text_hits = rel.search_text("documents", "content", "memory-mapped zero-copy", 5, true, None)?;
for hit in text_hits {
    println!("Score: {:.2}, Snippet: {:?}", hit.score, hit.snippet);
}

// HNSW Vector Indexing & Search
rel.create_vector_index("documents", "embedding", VectorIndexOptions {
    metric: VectorMetric::Cosine,
    m: 16,
    ef_construction: 100,
    ef_search: 50,
})?;

let query_vec = vec![0.05, -0.12, 0.44, /* ... */];
let vec_hits = rel.search_vector("documents", "embedding", &query_vec, 5, VectorMetric::Cosine, None)?;
for hit in vec_hits {
    println!("Row PK: {:?}, Distance: {:.4}", hit.row.pk, hit.distance);
}
```

---

### 7. Zero-Cost Hybrid Joins

Join graph traversal results directly with relational table metadata in a single read transaction:

```rust
let nodes = vec![NodeId(1), NodeId(2)];
let joined = db.join_nodes_with_table(&nodes, &schema)?;

for item in joined {
    println!("Node {:?} -> Row: {:?}", item.node_id, item.row);
}
```

---

### 8. High-Performance Bulk Synchronization (`SyncBatch`)

Ingest hundreds of thousands of records in a single atomic transaction:

```rust
use bkndb::hybrid::{SyncBatch, NewNode};

let mut batch = SyncBatch::new();

let n0 = batch.add_node("Service", [("name".to_string(), "auth".into())]);
let n1 = batch.add_node("Service", [("name".to_string(), "billing".into())]);
batch.add_edge(NewNode(n0), "DEPENDS_ON", NewNode(n1), []);

batch.add_row("service_metrics", [
    ("service".to_string(), "auth".into()),
    ("uptime".to_string(), 99.98.into()),
]);

let result = db.sync_batch(batch)?;
println!("Committed: {} nodes, {} edges, {} rows",
    result.nodes_created.len(), result.edges_created.len(), result.rows_inserted);
```

---

### 9. Storage Maintenance & Operations

```rust
// Force SSTable compaction to reclaim dead space
db.compact()?;

// Online hot backup
db.backup_to("backup.bkndb")?;

// Verify checksums of all blocks
let report = db.verify_integrity()?;
assert!(report.is_ok());

// Inspect disk metrics
let stats = db.storage_stats()?;
println!("File size: {} bytes, Reclaimable: {} bytes", stats.file_size, stats.reclaimable_bytes);
```

---

## Detailed Documentation

For exhaustive API documentation, performance recipes, and advanced patterns, refer to:
- **[wiki-rust.md](../../docs/wikis/wiki-rust.md)**: Complete Rust user guide (1,400+ lines).
- **[wiki-py.md](../../docs/wikis/wiki-py.md)**: Complete Python user guide.

---

## License

Licensed under the Apache License, Version 2.0 (see [LICENSE](../../LICENSE)).
