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

`bkndb` is an **embedded hybrid graph + relational + vector database** written in pure Rust. It runs completely inside your application process (zero network overhead, zero background daemon), persisting all data into a **single `.bkndb` container file** via battle-tested Redb storage engine (with an optional LSM-tree engine).

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
- Mobile applications via UniFFI Kotlin/Swift bindings sharing the same storage core. Package the Android native libraries into your app; use the iOS script to assemble an XCFramework.

### Not Intended For:
- Multiple concurrent processes writing to the same file (the database file is locked exclusively by one process).
- Distributed clustering or multi-node replication.
- Vector search over hundreds of millions of embeddings (HNSW is optimized up to millions of vectors per file).
- Complex analytical SQL queries requiring distributed hash joins or recursive subqueries.

---

## 2. Installation and Feature Flags

Use a current **Rust stable** toolchain with edition 2024 support. The workspace does not declare a `rust-version` minimum.

Add `bkndb` to your `Cargo.toml`:

```toml
[dependencies]
bkndb = "0.3"
```

### Feature Flags

| Feature | Default | Description |
| :--- | :---: | :--- |
| `redb-backend` | **Yes** | Default robust single-file storage backend (`RedbStorageBackend`) + `BknDb` handle. |
| `mem-backend` | **Yes** | In-memory storage backend (`MemoryStorageBackend`, `BknDb::in_memory()`). |
| `graph` | **Yes** | Property graph layer (`bkndb::graph`) and `MATCH` parser. |
| `relational-layer` | **Yes** | Relational tables (`bkndb::relational`) and SQL parser. |
| `search` | **Yes** | BM25 full-text and HNSW vector search (requires `relational-layer`). |
| `lsm-backend` | No | Optional LSM-tree storage backend (`LsmStorageBackend`, `BknDb::open_lsm`). |

`BknDb::open()` selects Redb with the default features. To use LSM while
keeping the defaults, enable `lsm-backend` and call `BknDb::open_lsm()`.
Redb and LSM files have different formats even when both use `.bkndb`;
opening a file with the other backend does not migrate it.

These examples target the 0.3 API in this checkout. To use this source tree
directly, depend on `bkndb = { path = "../bkn-db/crates/bkndb" }` (adjust the path).


All core modules are re-exported at the crate root (`pub use bkndb_core::*`), including `bkndb::graph`, `bkndb::relational`, `bkndb::value`, `bkndb::lang`, `bkndb::kv`, `bkndb::hybrid`, and `bkndb::db`.

---

## 3. Architecture and Core Types

```
BknDb  ──Deref──▶  Db<RedbStorageBackend>
                     ├── .graph()       -> GraphDb<B>        (node, edge, traversal, MATCH)
                     ├── .relational()  -> RelationalDb<B>   (catalog, tables, SQL, search)
                     ├── .kv()          -> Kv<B>             (raw bytes)
                     ├── .write_tx(|b| …) / .begin_write()   (cross-model ACID writes)
                     ├── .read_tx(|r| …)                     (cross-model snapshot reads)
                     ├── .sync_batch(SyncBatch)              (high-volume bulk ingestion)
                     └── .stats()                            (logical entity/row counts)
```

- **`BknDb`**: The primary entry point for persistent on-disk databases. It wraps `Db<RedbStorageBackend>` (via `Deref`) and adds file-level maintenance methods: `compact`, `backup_to`, `storage_stats`, `verify_integrity`.
- **`Db<B>`**: Generic over any storage backend `B: StorageBackend`. Cheap to clone (internally backed by `Arc`). `BknDb::in_memory()` returns `Db<MemoryStorageBackend>`.
- **`GraphDb`, `RelationalDb`, `Kv`**: Model-specific facades sharing the same backend. Non-transactional calls automatically open and commit their own transactions.

### Concurrency Guarantees
- **Single Writer**: Only one write transaction executes at any time. `begin_write()` waits for active writers to finish.
- **Concurrent Readers**: Reads execute on consistent snapshots without being blocked by writers.
- **Durable Commits**: Commits are safely synced to disk before returning `Ok`.
- **Rollback**: Dropping a write transaction without calling commit automatically rolls back all changes.

---

## 4. Quick Start

Run this example with a new database file (or use `BknDb::in_memory()`). Later
fragments assume an open `db`, the stated schemas/nodes, and the imports used
in the quick start. Schema names in independent sections are examples rather
than one continuous program.

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
use bkndb::{BknDb, BknError};

// 1. Default persistent single-file database (backed by Redb)
let db = BknDb::open("data/app.bkndb")?;

// open_redb is an explicit alias
let db_alias = BknDb::open_redb("data/app_redb.bkndb")?;

// Opening the same file path while open returns an error
assert!(matches!(BknDb::open("data/app.bkndb"), Err(BknError::DatabaseLocked(_))));
drop(db); // Releases file lock

// 2. Open optional LSM backend (when feature "lsm-backend" is enabled)
#[cfg(feature = "lsm-backend")]
{
    use bkndb::LsmOptions;
    let lsm_db = BknDb::open_lsm_with_options("data/lsm.bkndb", LsmOptions {
        memtable_flush_bytes: 32 * 1024 * 1024, // 32MB MemTable buffer
        compaction_trigger_files: 8,
        block_size_bytes: 4096,
        ..Default::default()
    })?;
    drop(lsm_db);
}

// 3. Ephemeral in-memory database
let mem = BknDb::in_memory();
```

Create parent directories before opening nested paths. Enable the dependency feature
`lsm-backend` to use LSM methods; `#[cfg(feature = "lsm-backend")]` in an
application refers to that application's own feature, not automatically to a dependency feature.

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

let neighbors = g.neighbors_out(n1, "FOLLOWS")?;
assert_eq!(neighbors.iter().map(|(node, _)| *node).collect::<Vec<_>>(), vec![n2]);

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
    .filter_edge_types(&["CALLS", "REFERENCES"])
    .max_depth(4)
    .run()?;

// BFS Shortest Path (minimum hop count)
if let Some(path) = g.find_shortest_path(start, target, Direction::Out, Some(&["CALLS"]))? {
    println!("Distance: {} hops", path.edges().len());
    for step in path.steps {
        println!("  Node {:?}, via edge {:?}, type {:?}", step.node, step.via_edge, step.edge_type);
    }
}

// Dijkstra Weighted Path (minimum cost)
if let Some(route) = g.find_weighted_path(start, target, Direction::Out, None, "latency_ms", 1.0)? {
    println!("Minimum latency: {}", route.cost);
}

// Top Hubs (in-degree / degree centrality)
let hubs = g.top_hubs(5, Direction::In, None)?;
for (node_id, degree) in hubs {
    println!("Hub Node: {:?}, Degree: {}", node_id, degree);
}

// Cascading Node Deletion (cleans up node and attached edges recursively)
let stats = g.cascade_delete(root_node, "CONTAINS")?;
println!("Deleted {} nodes and their incident edges", stats.len());
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
orders.update().filter(col("status").eq("pending")).set("status", "processed").run()?;

// Delete
orders.delete().filter(col("status").eq("cancelled")).run()?;
```

---

## 11. Filter Expressions and Aggregations

```rust
use bkndb::relational::{col, Agg};

let rel = db.relational();
let t = rel.table_named("products")?;

// Filter operators: eq, ne, lt, le, gt, ge, is_in, between, starts_with, contains, like
let cond = col("price").between(10.0, 50.0)
    .and(col("category").is_in(["books", "stationery"]))
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
    let node = b.graph().create_node("Account", Properties::from([("balance".into(), 1000.into())]))?;
    b.relational().table_named("audit_log")?.insert(Properties::from([
        ("event".into(), "account_opened".into()),
        ("account_id".into(), PropValue::Int(node.0 as i64)),
    ]))?;

    Ok(())
})?;

// Snapshot read transaction
let (node_count, row_count) = db.read_tx(|tx| {
    let nodes = tx.graph().nodes_by_label("Account")?.len();
    let rows = tx.relational().table_named("audit_log")?.count(&bkndb::relational::Query::new())?;
    Ok((nodes, rows))
})?;
```

---

## 13. Hybrid: `SyncBatch` and Joins

### Bulk Synchronization

Relational rows are upserted by primary key. Graph nodes/edges are created;
repeating a batch does not deduplicate graph entities. `add_node` returns
`&mut Self`; use zero-based `NodeRef::New` positions for linked endpoints.
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

### Joining Graph Nodes with Relational Tables
```rust
let nodes = vec![NodeId(1), NodeId(2)];
let joined = db.join_nodes_with_table(&nodes, &schema)?;
for item in joined {
    println!("Node {:?} => Metadata Row: {:?}", item.id, item.row);
}
```

---

## 14. Raw Key-Value (`Kv`)

For binary protocols, caches, and custom serialization:

```rust
let kv = db.kv();
kv.put(bkndb::TableSpec("session_store"), b"sess_12345", b"user_data_payload")?;

if let Some(val) = kv.get(bkndb::TableSpec("session_store"), b"sess_12345")? {
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
    bkndb::lang::Params::named([("min", 10)])
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

let res = g.query(query, bkndb::lang::Params::named([("name", "Alice")]))?;
for row in res.rows {
    println!("Friend: {:?}, Common paths: {:?}", row[0], row[1]);
}
```

---

## 17. Full-Text and Vector Search (BM25 & HNSW)

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

## 18. Operations: Backup, Stats, Integrity, Compaction

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

## 19. Error Handling (`BknError`)

Common error variants:
- `BknError::NotFound`: Requested node, edge, or row not found.
- `BknError::DuplicateKey`: Primary key or unique constraint violation.
- `BknError::DatabaseLocked`: Database file is already opened by another handle/process.
- `BknError::SchemaMismatch` / `BknError::ConstraintViolation`: Missing required columns or incompatible data types.
- `BknError::Corruption`: Checksum mismatch or corrupted storage blocks.

---

## 20. Storage Backends and Custom Storage Guide

`bkndb` ships with multiple built-in engines implementing the `StorageBackend` trait:
- **Redb (`RedbStorageBackend`)**: Default on-disk engine, battle-tested ACID B-tree in a single file with fast point lookups and MVCC reads.
- **Memory (`MemoryStorageBackend`)**: In-memory ephemeral engine, zero disk I/O, ideal for tests and ephemeral caches.
- **LSM (`LsmStorageBackend`)**: Optional engine (`feature = "lsm-backend"`), single-file WAL + memtable + compressed SSTables.

To implement a new backend, implement `StorageBackend` (`Send + Sync`),
`StorageReadTx`, and `StorageWriteTx`. This skeleton assumes custom transaction
types that implement those traits:

```text
use bkndb_core::{StorageBackend, StorageReadTx, StorageWriteTx, TableSpec, BknError};

pub struct CustomEngine { /* ... */ }

impl StorageBackend for CustomEngine {
    type ReadTx<'a> = CustomReadTx<'a> where Self: 'a;
    type WriteTx<'a> = CustomWriteTx<'a> where Self: 'a;

    fn begin_read(&self) -> Result<Self::ReadTx<'_>, BknError> { todo!() }
    fn begin_write(&self) -> Result<Self::WriteTx<'_>, BknError> { todo!() }
}
```

---

## 21. Quick API Reference

```text
// Opening (schematic calls)
BknDb::open(path)?;                     // Default (Redb)
BknDb::open_redb(path)?;                // Explicit alias
BknDb::open_lsm(path)?;                 // feature "lsm-backend"
BknDb::open_lsm_with_options(path, opt);// feature "lsm-backend"
BknDb::in_memory();                     // In-memory
```

```text
// Graph
db.graph().create_node(label, props)?;
db.graph().create_edge(from, edge_type, to, props)?;
db.graph().neighbors_out(node, "EDGE_TYPE")?; // (NodeId, EdgeId) pairs
db.graph().find_shortest_path(from, to, direction, edge_type)?;
db.graph().find_weighted_path(from, to, direction, edge_types, weight_prop, 1.0)?;
db.graph().top_hubs(k, direction, label)?;
db.graph().cascade_delete(node, "CONTAINS")?;
db.graph().query("MATCH ...", params)?;

// Relational
db.relational().ensure_table(&schema)?;
db.relational().table_named("name")?.insert(row)?;
db.relational().table_named("name")?.select().filter(col("x").gt(5)).run()?;
db.relational().sql("SELECT ...", params)?;
db.relational().create_fulltext_index(table, col)?;
db.relational().search_text(table, col, query, limit, match_all, filter)?;
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
