# bkndb for Rust — Complete Guide & API Reference

This document is the comprehensive guide and API reference for the `bkndb` Rust crate: its purpose, usage patterns, public types, methods, runnable examples, and limitations.

---

## Table of Contents

1. [What is bkndb and When to Use It](#1-what-is-bkndb-and-when-to-use-it)
2. [Installation and Feature Flags](#2-installation-and-feature-flags)
3. [Architecture and Core Types](#3-architecture-and-core-types)
4. [Quick Start](#4-quick-start)
5. [Opening a Database](#5-opening-a-database)
6. [Values: `PropValue` and `Properties`](#6-values-propvalue-and-properties)
7. [Graph: Nodes, Edges, and Topology](#7-graph-nodes-edges-and-topology)
8. [Traversal and Path Algorithms](#8-traversal-and-path-algorithms)
9. [Relational: Schemas and Constraints](#9-relational-schemas-and-constraints)
10. [Relational: Rows and Query Builder](#10-relational-rows-and-query-builder)
11. [Filter Expressions and Aggregations](#11-filter-expressions-and-aggregations)
12. [Transactions: Hybrid ACID Writes and Snapshot Reads](#12-transactions-hybrid-acid-writes-and-snapshot-reads)
13. [Hybrid: `SyncBatch` and Joins](#13-hybrid-syncbatch-and-joins)
14. [Raw Key-Value (`Kv`)](#14-raw-key-value-kv)
15. [SQL Engine](#15-sql-engine)
16. [Graph Query Language (`MATCH`)](#16-graph-query-language-match)
17. [Full-Text and Vector Search (BM25 & HNSW)](#17-full-text-and-vector-search-bm25--hnsw)
18. [Operations: Backup, Stats, Integrity, Compaction](#18-operations-backup-stats-integrity-compaction)
19. [Error Handling (`BknError`)](#19-error-handling-bknerror)
20. [Storage Backends and Custom Storage Guide](#20-storage-backends-and-custom-storage-guide)
21. [Quick API Reference](#21-quick-api-reference)
22. [Real-World Recipes](#22-real-world-recipes)
23. [Limitations and Performance Guidelines](#23-limitations-and-performance-guidelines)

---

## 1. What is bkndb and When to Use It

`bkndb` is an **embedded hybrid graph + relational + vector database** written in pure Rust. It runs completely inside your application process (zero network overhead, zero background daemon), persisting all data into a **single `.bkndb` container file** via an append-only LSM tree engine.

| Model | Purpose | Primary APIs |
| :--- | :--- | :--- |
| **Graph** | Labeled nodes and typed directed edges with dynamic properties | `GraphDb`, traversal builder, BFS shortest path, Dijkstra weighted path, `query("MATCH …")` |
| **Relational** | Typed tables with schemas, constraints, secondary indexes, and query builder | `RelationalDb`, `RelTable`, `Query`, `Expr`, `Agg`, `sql("SELECT …")` |
| **Key-Value** | Raw binary byte storage partitioned per table | `Kv` |
| **Search** | Full-text BM25 inverted index & HNSW vector nearest neighbor search | `search_text`, `search_vector`, `create_vector_index` |

All data models share **one atomic ACID transaction**: `db.write_tx(|b| { … })` writes nodes, edges, relational rows, search indexes, and raw KV pairs simultaneously.

### Best Used For:
- Embedded Rust applications (CLIs, desktop/Tauri, local services) requiring rich relational data and graph topologies without external database servers.
- Knowledge Graph / GraphRAG, code intelligence (AST → function calls → file dependencies), and local agentic memory.
- Mobile and embedded systems via UniFFI bindings (Android Kotlin AAR & iOS Swift XCFramework) sharing the same storage core.

### Not Intended For:
- Multiple concurrent processes writing to the same file (the database file is locked exclusively by one process).
- Distributed clustering or multi-node replication.
- Vector search over hundreds of millions of embeddings (HNSW is optimized up to millions of vectors per file).
- Complex analytical SQL queries requiring distributed hash joins or recursive subqueries.

---

## 2. Installation and Feature Flags

Requires **Rust 1.88+** (edition 2024).

Add `bkndb` to your `Cargo.toml`:

```toml
[dependencies]
bkndb = "0.2"
```

### Feature Flags

| Feature | Default | Description |
| :--- | :---: | :--- |
| `lsm-backend` | **Yes** | Native single-file `.bkndb` container (`LsmStorageBackend`) + `BknDb` handle. |
| `mem-backend` | **Yes** | In-memory storage backend (`MemoryStorageBackend`, `BknDb::in_memory()`). |
| `graph` | **Yes** | Property graph layer (`bkndb::graph`) and `MATCH` parser. |
| `relational-layer` | **Yes** | Relational tables (`bkndb::relational`) and SQL parser. |
| `search` | **Yes** | BM25 full-text and HNSW vector search (requires `relational-layer`). |
| `redb-backend` | No | Alternative storage backend powered by `redb` (`BknDb::open_redb`). |

All core modules are re-exported at the crate root (`pub use bkndb_core::*`), including `bkndb::graph`, `bkndb::relational`, `bkndb::value`, `bkndb::lang`, `bkndb::kv`, `bkndb::hybrid`, and `bkndb::db`.

---

## 3. Architecture and Core Types

```
BknDb  ──Deref──▶  Db<LsmStorageBackend>
                     ├── .graph()       -> GraphDb<B>        (node, edge, traversal, MATCH)
                     ├── .relational()  -> RelationalDb<B>   (catalog, tables, SQL, search)
                     ├── .kv()          -> Kv<B>             (raw bytes)
                     ├── .write_tx(|b| …) / .begin_write()   (cross-model ACID writes)
                     ├── .read_tx(|r| …)                     (cross-model snapshot reads)
                     ├── .sync_batch(SyncBatch)              (high-volume bulk ingestion)
                     └── .stats()                            (storage metrics)
```

- **`BknDb`**: The primary entry point for persistent on-disk databases. It wraps `Db<LsmStorageBackend>` (via `Deref`) and adds file-level maintenance methods: `compact`, `backup_to`, `storage_stats`, `verify_integrity`.
- **`Db<B>`**: Generic over any storage backend `B: StorageEngine`. Cheap to clone (internally backed by `Arc`). `BknDb::in_memory()` returns `Db<MemoryStorageBackend>`.
- **`GraphDb`, `RelationalDb`, `Kv`**: Model-specific facades sharing the same backend. Non-transactional calls automatically open and commit their own transactions.

### Concurrency Guarantees
- **Single Writer**: Only one write transaction executes at any time. `begin_write()` waits for active writers to finish.
- **Concurrent Readers**: Reads execute on consistent snapshots without being blocked by writers.
- **Durable Commits**: In LSM mode, commits are `fsync`'d to the Write-Ahead Log before returning `Ok`.
- **Rollback**: Dropping a write transaction without calling commit automatically rolls back all changes.

---

## 4. Quick Start

```rust
use bkndb::graph::{Direction, Properties};
use bkndb::relational::{col, Agg, ColumnKind, ColumnSchema, TableSchema};
use bkndb::value::PropValue;
use bkndb::BknDb;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db = BknDb::open("quickstart.bkndb")?;

    // --- Graph ---
    let graph = db.graph();
    let mut p = Properties::new();
    p.insert("title".into(), "Attention Is All You Need".into());
    let doc = graph.create_node("Document", p)?;
    let topic = graph.create_node("Concept", Properties::from([("name".to_string(), "Transformer".into())]))?;
    graph.create_edge(doc, "DISCUSSES", topic, Properties::new())?;

    // --- Relational Schema ---
    let rel = db.relational();
    let chunks = TableSchema::builder("chunks")
        .column(ColumnSchema::new("id", ColumnKind::Int))
        .column(ColumnSchema::new("doc", ColumnKind::Int).not_null())
        .column(ColumnSchema::new("text", ColumnKind::Str))
        .column(ColumnSchema::new("tokens", ColumnKind::Int).default_value(0))
        .primary_key("id")
        .auto_increment()
        .index("doc")
        .build()?;
    rel.ensure_table(&chunks)?;

    // --- Unified ACID Transaction ---
    db.write_tx(|b| {
        let mut row = Properties::new();
        row.insert("doc".into(), PropValue::Int(doc.0 as i64));
        row.insert("text".into(), "Self-attention mechanism...".into());
        row.insert("tokens".into(), 512.into());
        b.relational().table_named("chunks")?.insert(row)?;
        b.graph().update_node_properties(doc, |props| {
            props.insert("chunked".into(), true.into());
        })?;
        Ok(())
    })?;

    // --- Querying ---
    let t = rel.table_named("chunks")?;
    let big = t.select().filter(col("doc").eq(doc.0 as i64).and(col("tokens").gt(100))).order_by_desc("tokens").run()?;
    let stats = t.select().aggregate(&[Agg::count(), Agg::avg("tokens")])?;
    println!("{} chunks, stats: {:?}", big.len(), stats);

    // --- SQL & MATCH Queries ---
    let sql_res = rel.sql("SELECT COUNT(*) FROM chunks WHERE tokens > ?", vec![PropValue::Int(100)])?;
    println!("SQL count: {:?}", sql_res.rows);

    let match_res = graph.query("MATCH (d:Document)-[:DISCUSSES]->(c) RETURN c.name", ())?;
    println!("Graph concepts: {:?}", match_res.rows);

    // --- Traversal ---
    for hit in graph.traversal().start(doc).direction(Direction::Out).max_depth(2).run()? {
        println!("depth {} -> NodeId {:?}", hit.depth, hit.node);
    }
    Ok(())
}
```

---

## 5. Opening a Database

```rust
use bkndb::{BknDb, BknError, LsmOptions};

// 1. Default persistent single-file (.bkndb)
let db = BknDb::open("data/app.bkndb")?;

// Opening the same file path while open returns an error
assert!(matches!(BknDb::open("data/app.bkndb"), Err(BknError::DatabaseLocked(_))));
drop(db); // Releases file lock

// 2. Open with custom LSM options
let tuned = BknDb::open_with_options("data/tuned.bkndb", LsmOptions {
    memtable_flush_bytes: 32 * 1024 * 1024, // 32MB MemTable buffer
    compaction_trigger_files: 8,
    block_size: 4096,
    ..Default::default()
})?;

// 3. Ephemeral in-memory database
let mem = BknDb::in_memory();
```

---

## 6. Values: `PropValue` and `Properties`

BknDb represents dynamic values using `PropValue`:

| `PropValue` Variant | Rust Representation | Storage & Key Encoding |
| :--- | :--- | :--- |
| `Null` | `()` | Nullable |
| `Bool(bool)` | `bool` | 1 byte |
| `Int(i64)` | `i64` | Sortable big-endian 8 bytes |
| `Float(f64)` | `f64` | IEEE 754 float |
| `Str(String)` | `String` | UTF-8 sortable string |
| `Bytes(Vec<u8>)` | `Vec<u8>` | Raw byte blob |
| `Timestamp(i64)` | `i64` | Microseconds since UTC epoch |
| `Uuid([u8; 16])` | `[u8; 16]` | 128-bit UUID |
| `List(Vec<PropValue>)` | `Vec<PropValue>` | JSON-like array |
| `Map(BTreeMap<String, PropValue>)` | `BTreeMap<...>` | Key-value dictionary |

`Properties` is an alias for `BTreeMap<String, PropValue>`:
```rust
use bkndb::value::PropValue;
use bkndb::graph::Properties;

let mut props = Properties::new();
props.insert("name".into(), "Alice".into());
props.insert("age".into(), 30.into());
props.insert("score".into(), 98.5.into());
props.insert("tags".into(), PropValue::List(vec!["rust".into(), "db".into()]));
```

---

## 7. Graph: Nodes, Edges, and Topology

```rust
let g = db.graph();

// 1. Create Nodes
let n1 = g.create_node("User", Properties::from([("name".into(), "Alice".into())]))?;
let n2 = g.create_node("User", Properties::from([("name".into(), "Bob".into())]))?;

// 2. Create Directed Edges
let e1 = g.create_edge(n1, "FOLLOWS", n2, Properties::from([("since".into(), 2026.into())]))?;

// 3. Inspect Properties & Topology
let alice = g.get_node(n1)?.expect("exists");
println!("Label: {}, Name: {:?}", alice.label, alice.properties.get("name"));

let neighbors = g.neighbors(n1, Direction::Out, Some("FOLLOWS"))?;
assert_eq!(neighbors, vec![n2]);

// 4. Update Properties
g.update_node_properties(n1, |props| {
    props.insert("verified".into(), true.into());
})?;

// 5. Indexing for Fast Lookups
g.create_property_index("User", "name")?;
let found = g.find_nodes("User", "name", &"Alice".into())?;
assert_eq!(found, vec![n1]);
```

---

## 8. Traversal and Path Algorithms

```rust
let g = db.graph();

// Traversal Builder
let hits = g.traversal()
    .start(start_node)
    .direction(Direction::Out)
    .edge_types(&["CALLS", "REFERENCES"])
    .max_depth(4)
    .run()?;

// BFS Shortest Path (minimum hop count)
if let Some(path) = g.find_shortest_path(start, target, Direction::Out, Some("CALLS"))? {
    println!("Distance: {} hops", path.distance);
    for step in path.steps {
        println!("  {:?} --[{}]--> {:?}", step.from, step.edge_type, step.to);
    }
}

// Dijkstra Weighted Path (minimum cost)
if let Some(route) = g.find_weighted_path(start, target, "latency_ms", Direction::Out, None)? {
    println!("Minimum latency: {}", route.cost);
}

// Top Hubs (in-degree / degree centrality)
let hubs = g.top_hubs(5)?;
for hub in hubs {
    println!("Hub Node: {:?}, Degree: {}", hub.node_id, hub.degree);
}

// Cascading Node Deletion (cleans up node and attached edges recursively)
let stats = g.cascade_delete(root_node)?;
println!("Cleaned up: {} nodes, {} edges", stats.nodes_deleted, stats.edges_deleted);
```

---

## 9. Relational: Schemas and Constraints

```rust
use bkndb::relational::{TableSchema, ColumnSchema, ColumnKind};

let schema = TableSchema::builder("orders")
    .column(ColumnSchema::new("id", ColumnKind::Int))
    .column(ColumnSchema::new("customer_id", ColumnKind::Int).not_null())
    .column(ColumnSchema::new("total", ColumnKind::Float).default_value(0.0))
    .column(ColumnSchema::new("status", ColumnKind::Str).default_value("pending"))
    .primary_key("id")
    .auto_increment()
    .index("customer_id")
    .index("status")
    .build()?;

// ensure_table applies schema idempotently and handles schema migrations safely
db.relational().ensure_table(&schema)?;
```

---

## 10. Relational: Rows and Query Builder

```rust
let rel = db.relational();
let mut orders = rel.table_named("orders")?;

// Insert
let mut row = Properties::new();
row.insert("customer_id".into(), 42.into());
row.insert("total".into(), 129.99.into());
let pk = orders.insert(row)?;

// Query Builder
use bkndb::relational::col;

let items = orders.select()
    .filter(col("customer_id").eq(42).and(col("status").eq("pending")))
    .order_by_desc("total")
    .limit(10)
    .run()?;

for item in items {
    println!("Order PK: {:?}, Total: {:?}", item.pk, item.values.get("total"));
}

// Update
orders.update(col("status").eq("pending"), |row| {
    row.insert("status".into(), "processed".into());
})?;

// Delete
orders.delete(col("status").eq("cancelled"))?;
```

---

## 11. Filter Expressions and Aggregations

```rust
use bkndb::relational::{col, Agg};

let mut t = db.relational().table_named("products")?;

// Filter operators: eq, ne, lt, lte, gt, gte, is_in, between, starts_with, contains, like
let cond = col("price").between(10.0, 50.0)
    .and(col("category").is_in(vec!["books".into(), "stationery".into()]))
    .and(col("name").starts_with("Rust"));

let filtered = t.select().filter(cond).run()?;

// Aggregations: count, sum, avg, min, max
let agg_results = t.select().aggregate(&[
    Agg::count(),
    Agg::avg("price"),
    Agg::max("price"),
])?;
println!("Product Stats: {:?}", agg_results);
```

---

## 12. Transactions: Hybrid ACID Writes and Snapshot Reads

```rust
// Atomic write transaction
db.write_tx(|b| {
    let mut g = b.graph();
    let mut rel = b.relational();

    let node = g.create_node("Account", Properties::from([("balance".into(), 1000.into())]))?;
    rel.table_named("audit_log")?.insert(Properties::from([
        ("event".into(), "account_opened".into()),
        ("account_id".into(), PropValue::Int(node.0 as i64)),
    ]))?;

    Ok(())
})?;

// Snapshot read transaction
let (node_count, row_count) = db.read_tx(|tx| {
    let nodes = tx.graph().node_count()?;
    let rows = tx.relational().table_named("audit_log")?.count()?;
    Ok((nodes, rows))
})?;
```

---

## 13. Hybrid: `SyncBatch` and Joins

### Bulk Synchronization
```rust
use bkndb::hybrid::{SyncBatch, NewNode};

let mut batch = SyncBatch::new();

let n0 = batch.add_node("Package", [("name".to_string(), "tokio".into())]);
let n1 = batch.add_node("Package", [("name".to_string(), "mio".into())]);
batch.add_edge(NewNode(n0), "DEPENDS_ON", NewNode(n1), []);

batch.add_row("metadata", [
    ("pkg".to_string(), "tokio".into()),
    ("downloads".to_string(), 50000000.into()),
]);

let result = db.sync_batch(batch)?;
println!("Synchronized {} nodes and {} rows", result.nodes_created.len(), result.rows_inserted);
```

### Joining Graph Nodes with Relational Tables
```rust
let nodes = vec![NodeId(1), NodeId(2)];
let joined = db.join_nodes_with_table(&nodes, &schema)?;
for item in joined {
    println!("Node {:?} => Metadata Row: {:?}", item.node_id, item.row);
}
```

---

## 14. Raw Key-Value (`Kv`)

For binary protocols, caches, and custom serialization:

```rust
let kv = db.kv();
kv.set(b"session_store", b"sess_12345", b"user_data_payload")?;

if let Some(val) = kv.get(b"session_store", b"sess_12345")? {
    println!("Found session: {:?}", val);
}
```

---

## 15. SQL Engine

```rust
let rel = db.relational();

// DDL
rel.sql("CREATE TABLE items (id INT PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, qty INT DEFAULT 0)", ())?;

// DML
rel.sql("INSERT INTO items (name, qty) VALUES (?, ?)", vec!["Keyboard".into(), 15.into()])?;

// Query
let res = rel.sql("SELECT name, qty FROM items WHERE qty > :min ORDER BY qty DESC", 
    bkndb::lang::Params::named([("min", 10.into())])
)?;
for row in res.rows {
    println!("Item: {:?}, Qty: {:?}", row[0], row[1]);
}
```

---

## 16. Graph Query Language (`MATCH`)

```rust
let g = db.graph();

let query = "
    MATCH (user:User {name: $name})-[:FOLLOWS*1..2]->(friend:User)
    RETURN friend.name AS friend_name, count(*) AS paths
    ORDER BY friend_name
";

let res = g.query(query, bkndb::lang::Params::named([("name", "Alice".into())]))?;
for row in res.rows {
    println!("Friend: {:?}, Common paths: {:?}", row[0], row[1]);
}
```

---

## 17. Full-Text and Vector Search (BM25 & HNSW)

```rust
use bkndb::relational::{pack_vector, VectorIndexOptions, VectorMetric};

let mut rel = db.relational();

// 1. Full-Text Search (BM25)
rel.create_fulltext_index("articles", "body")?;
let text_results = rel.search_text("articles", "body", "memory mapped storage", 5, true, None)?;
for hit in text_results {
    println!("Score: {:.2}, Snippet: {:?}", hit.score, hit.snippet);
}

// 2. HNSW Vector Indexing
rel.create_vector_index("articles", "embedding", VectorIndexOptions {
    metric: VectorMetric::Cosine,
    m: 16,
    ef_construction: 100,
    ef_search: 50,
})?;

let query_vec = vec![0.12, -0.45, 0.78, /* ... */];
let hits = rel.search_vector("articles", "embedding", &query_vec, 5, VectorMetric::Cosine, None)?;
for hit in hits {
    println!("Row PK: {:?}, Distance: {:.4}", hit.row.pk, hit.distance);
}
```

---

## 18. Operations: Backup, Stats, Integrity, Compaction

```rust
// 1. Force SSTable Compaction to reclaim dead space
db.compact()?;

// 2. Online Hot Backup
db.backup_to("backups/app_backup.bkndb")?;

// 3. Storage Block CRC32 Checksum Verification
let report = db.verify_integrity()?;
assert!(report.is_ok());

// 4. File-Level Stats
let stats = db.storage_stats()?;
println!("File size: {} bytes, Reclaimable: {} bytes", stats.file_size, stats.reclaimable_bytes);
```

---

## 19. Error Handling (`BknError`)

Common error variants:
- `BknError::NotFound`: Requested node, edge, or row not found.
- `BknError::DuplicateKey`: Primary key or unique constraint violation.
- `BknError::DatabaseLocked`: Database file is already opened by another handle/process.
- `BknError::SchemaViolation`: Missing required columns or incompatible data types.
- `BknError::Corruption`: CRC32 checksum mismatch in WAL or SSTable blocks.

---

## 20. Storage Backends and Custom Storage Guide

To implement a new backend, implement `StorageEngine`, `StorageReadTx`, and `StorageWriteTx`:

```rust
use bkndb_core::{StorageEngine, StorageReadTx, StorageWriteTx, TableSpec, BknError};

pub struct CustomEngine { /* ... */ }

impl StorageEngine for CustomEngine {
    type ReadTx<'a> = CustomReadTx<'a>;
    type WriteTx<'a> = CustomWriteTx<'a>;

    fn begin_read(&self) -> Result<Self::ReadTx<'_>, BknError> { todo!() }
    fn begin_write(&self) -> Result<Self::WriteTx<'_>, BknError> { todo!() }
}
```

---

## 21. Quick API Reference

```rust
// Opening
BknDb::open(path)?;
BknDb::open_with_options(path, options)?;
BknDb::in_memory();

// Graph
db.graph().create_node(label, props)?;
db.graph().create_edge(from, type, to, props)?;
db.graph().neighbors(node, direction, edge_type)?;
db.graph().find_shortest_path(from, to, direction, edge_type)?;
db.graph().find_weighted_path(from, to, weight_prop, direction, edge_type)?;
db.graph().top_hubs(k)?;
db.graph().cascade_delete(node)?;
db.graph().query("MATCH ...", params)?;

// Relational
db.relational().ensure_table(&schema)?;
db.relational().table_named("name")?.insert(row)?;
db.relational().table_named("name")?.select().filter(col("x").gt(5)).run()?;
db.relational().sql("SELECT ...", params)?;
db.relational().create_fulltext_index(table, col)?;
db.relational().search_text(table, col, query, limit, highlight, filter)?;
db.relational().create_vector_index(table, col, options)?;
db.relational().search_vector(table, col, query_vec, limit, metric, filter)?;

// Transactions
db.write_tx(|b| { ... })?;
db.read_tx(|tx| { ... })?;
```

---

## 22. Real-World Recipes

### Knowledge Graph + RAG Pipeline
```rust
// Store document nodes, entity concepts, and text chunks with vector embeddings
db.write_tx(|b| {
    let doc = b.graph().create_node("Document", Properties::from([("title".to_string(), "AI Whitepaper".into())]))?;
    let concept = b.graph().create_node("Concept", Properties::from([("name".to_string(), "LLM".into())]))?;
    b.graph().create_edge(doc, "COVERS", concept, Properties::new())?;

    b.relational().table_named("chunks")?.insert(Properties::from([
        ("doc_id".to_string(), PropValue::Int(doc.0 as i64)),
        ("text".to_string(), "Large Language Models operate by...".into()),
        ("embedding".to_string(), pack_vector(&embedding_vec)),
    ]))?;
    Ok(())
})?;
```

---

## 23. Limitations and Performance Guidelines

1. **Process Exclusivity:** A single `.bkndb` file is locked exclusively by one process. Inside that process, multiple threads can read concurrently while writes are serialized.
2. **Batching Writes:** Always wrap multiple inserts/updates in `write_tx`, `*_bulk`, or `SyncBatch` to execute a single disk `fsync` per transaction.
3. **Indexing:** Add `.index()` on frequently filtered columns and create property indexes on initial graph lookup keys.
4. **Vector Search:** Linear exact search is suitable for collections under 50,000 vectors. For larger sets, build an HNSW index with `create_vector_index`.
5. **Disk Reclaiming:** Space from deleted records is reclaimed after calling `db.compact()`.

---

## License

Licensed under the Apache License, Version 2.0 (see [LICENSE](../../LICENSE)).
