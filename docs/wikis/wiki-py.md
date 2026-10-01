# bkndb for Python — Complete Guide & API Reference

This document is the comprehensive guide and complete API reference for the `bkndb` Python package: what it does, how to use it, every public class, method, function, runnable examples, and performance characteristics.

---

## Table of Contents

1. [What is bkndb and When to Use It](#1-what-is-bkndb-and-when-to-use-it)
2. [Installation](#2-installation)
3. [Core Concepts](#3-core-concepts)
4. [Quick Start](#4-quick-start)
5. [Opening and Closing a Database](#5-opening-and-closing-a-database)
6. [Graph: Nodes and Edges](#6-graph-nodes-and-edges)
7. [Graph: Traversal, Paths, and Graph Analysis](#7-graph-traversal-paths-and-graph-analysis)
8. [Graph: Label and Property Indexes](#8-graph-label-and-property-indexes)
9. [Relational: Table Schemas](#9-relational-table-schemas)
10. [Relational: Writing Rows](#10-relational-writing-rows)
11. [Relational: Reading and Filtering](#11-relational-reading-and-filtering)
12. [Relational: Aggregations](#12-relational-aggregations)
13. [Transactions](#13-transactions)
14. [Bulk Sync: Graph + Tables](#14-bulk-sync-graph--tables)
15. [SQL Engine](#15-sql-engine)
16. [Graph Query Language (`MATCH`)](#16-graph-query-language-match)
17. [Full-Text Search (BM25)](#17-full-text-search-bm25)
18. [Vector Search (Exact & HNSW Index)](#18-vector-search-exact--hnsw-index)
19. [Operations: Backup, Stats, Integrity, Compaction](#19-operations-backup-stats-integrity-compaction)
20. [Import and Export: CSV / JSONL](#20-import-and-export-csv--jsonl)
21. [Pandas and NetworkX Integration](#21-pandas-and-networkx-integration)
22. [Error Handling](#22-error-handling)
23. [API Reference](#23-api-reference)
24. [Real-World Recipes](#24-real-world-recipes)
25. [Limitations and Performance Guidelines](#25-limitations-and-performance-guidelines)

---

## 1. What is bkndb and When to Use It

`bkndb` is an **embedded hybrid graph + relational + vector database** engine. It runs entirely inside your Python application process (like SQLite), requires zero external daemon or server setup, and persists all data into a **single `.bkndb` file**. The underlying core engine is written in pure Rust for high speed and memory safety.

In a single database instance, you get:

| Model | Purpose | Primary Access Methods |
| :--- | :--- | :--- |
| **Property Graph** | Interconnected entities: labeled nodes + typed directed edges with dynamic properties | `create_node`, `neighbors`, `traverse`, `find_shortest_path`, `graph_query("MATCH …")` |
| **Relational Tables** | Structured tabular data with typed schemas, constraints, and secondary indexes | `create_table`, `insert`, `select`, `aggregate`, `sql("SELECT …")` |
| **Search Engine** | BM25 full-text search and vector similarity search (exact & HNSW ANN) | `search_text`, `search_vector`, `create_vector_index` |

All models share the **exact same ACID transaction**: a single `with db.transaction() as tx:` block can create graph nodes, edges, relational rows, and update vector embeddings simultaneously—either all mutations commit, or all roll back automatically.

### Ideal For:
- **Knowledge Bases & GraphRAG:** Documents and concepts modeled as a property graph, text chunks stored in tables with BM25 text and HNSW vector search.
- **Code Intelligence:** Repositories, files, ASTs, and function call paths modeled as graphs; metadata stored in relational tables.
- **Desktop, CLI, and Offline-First Apps:** Local embedded storage without requiring Docker, PostgreSQL, or Neo4j.
- **Network Analysis:** Social graphs, dependency trees, recommendation engines with sub-millisecond traversal.

### Not Intended For:
- Multiple concurrent processes writing to the same file simultaneously (file locking is exclusive per process).
- Distributed multi-node clustering or remote network database serving.
- Extreme vector scale (e.g. hundreds of millions of embeddings). HNSW in BknDb is tuned for millions of vectors per file.

---

## 2. Installation

Requires **Python 3.10+**.

```bash
pip install bkndb                     # Core wheel (Linux, Windows, macOS)
pip install "bkndb[pandas,networkx]"  # With Pandas and NetworkX support
```

One pre-built wheel per OS/architecture serves every Python 3.x version via UniFFI's native ctypes interface.

---

## 3. Core Concepts

1. **Single File, Single Owner:** `bkndb.open("data.bkndb")` creates or opens one file. While open, the file is locked exclusively by the operating system. Close it via `db.close()` or by using the `with` context manager.
2. **Unified Transctions:** Graph nodes and relational rows share atomic transactions.
3. **Native Dynamic Typing:** Property values map directly to standard Python types: `int`, `float`, `str`, `bool`, `bytes`, `None`, `datetime.datetime` (UTC), `uuid.UUID`, `list`, and `dict`.

---

## 4. Quick Start

```python
import bkndb
from bkndb import TableSchema, Column, col, Agg

with bkndb.open("quickstart.bkndb") as db: # or bkndb.in_memory()
    # 1. Create a relational schema
    db.create_table(TableSchema(
        "chunks",
        [
            Column("id", int),
            Column("doc_id", int, nullable=False),
            Column("text", str),
            Column("tokens", int, default=0),
        ],
        primary_key="id",
        auto_increment=True,
        indexes=["doc_id"],
    ))

    # 2. Unified ACID transaction across graph & tables
    with db.transaction() as tx:
        doc = tx.create_node("Document", {"title": "Attention Is All You Need"})
        concept = tx.create_node("Concept", {"name": "Transformer"})
        tx.create_edge(doc, "DISCUSSES", concept, {"relevance": 0.95})
        tx.insert("chunks", {"doc_id": doc, "text": "Self-attention...", "tokens": 512})

    # 3. Querying
    rows = db.select("chunks", (col("doc_id") == doc) & (col("tokens") > 100))
    print("Found chunks:", rows)

    # 4. Traversal
    for hit in db.traverse(doc, max_depth=2):
        node = db.get_node(hit.node_id)
        print(f"Depth {hit.depth}: {node.label} -> {node.properties}")
```

---

## 5. Opening and Closing a Database

```python
import bkndb

# 1. Persistent single-file database
db = bkndb.open("my_data.bkndb")

# Close and release file locks
db.close()
assert db.closed

# 2. Context manager (recommended)
with bkndb.open("my_data.bkndb") as db:
    # Operations
    pass

# 3. Tuning options
with bkndb.open(
    "tuned.bkndb",
    memtable_flush_bytes=32 * 1024 * 1024, # 32MB MemTable buffer
    compaction_trigger_files=8,
) as db:
    pass

# 4. Ephemeral in-memory database
with bkndb.in_memory() as mem_db:
    pass
```

---

## 6. Graph: Nodes and Edges

```python
# Create nodes
alice = db.create_node("Person", {"name": "Alice", "age": 30})
bob = db.create_node("Person", {"name": "Bob", "age": 28})

# Bulk creation
[charlie, david] = db.create_nodes_bulk([
    ("Person", {"name": "Charlie"}),
    ("Person", {"name": "David"}),
])

# Create directed edges
e1 = db.create_edge(alice, "KNOWS", bob, {"since": 2026})

# Inspect nodes and edges
node_obj = db.get_node(alice)
print(node_obj.id, node_obj.label, node_obj.properties)

# Update node properties
db.update_node(alice, set={"verified": True, "age": 31}, unset=["temporary_flag"])

# Delete node and its edges
db.delete_node(david)
```

---

## 7. Graph: Traversal, Paths, and Graph Analysis

```python
from bkndb import Direction

# Outgoing neighbors
friends = db.neighbors(alice, Direction.OUTGOING, edge_type="KNOWS")

# Multi-hop radius traversal
for hit in db.traverse(alice, direction=Direction.BOTH, max_depth=3):
    print(f"Hop {hit.depth}: Node {hit.node_id}")

# BFS Shortest Path (minimum hop count)
path = db.find_shortest_path(alice, charlie, edge_type="KNOWS")
if path:
    print(f"Shortest path length: {path.distance} hops")
    print(f"Steps: {path.node_ids}")

# Dijkstra Weighted Path (minimum cost)
route = db.find_weighted_path(office, airport, weight="distance_km")
if route:
    print(f"Total distance: {route.cost} km, path: {route.path.node_ids}")

# Degree Centrality (Top Hubs)
top_nodes = db.top_hubs(k=5)
for hub in top_nodes:
    print(f"Node {hub.node_id} has {hub.degree} connections")

# Cascading Delete
stats = db.cascade_delete(alice)
print(f"Deleted {stats.nodes_deleted} nodes and {stats.edges_deleted} edges")
```

---

## 8. Graph: Label and Property Indexes

```python
# Label indexing (built-in)
all_people = db.nodes_by_label("Person")
total_people = db.count_nodes("Person")

# Property Indexing (speeds up find_nodes to O(1) index lookup)
db.create_node_index("Person", "email")

# Lookup
[alice_node] = db.find_nodes("Person", "email", "alice@example.com")
```

---

## 9. Relational: Table Schemas

```python
from bkndb import TableSchema, Column

schema = TableSchema(
    "users",
    [
        Column("id", int),
        Column("email", str, nullable=False, unique=True),
        Column("username", str, nullable=False),
        Column("age", int, default=18),
        Column("created_at", "timestamp"),
        Column("metadata", "map"),
    ],
    primary_key="id",
    auto_increment=True,
    indexes=["username"],
)

# Idempotent table creation with automatic migration support
db.ensure_table(schema)
```

---

## 10. Relational: Writing Rows

```python
users = db.table("users")

# Insert single row (returns primary key)
pk = users.insert({"email": "alice@example.com", "username": "alice"})

# Bulk insert
pks = users.insert_many([
    {"email": "bob@example.com", "username": "bob"},
    {"email": "carol@example.com", "username": "carol"},
])

# Upsert (insert or overwrite existing primary key)
users.upsert({"id": pk, "email": "alice_updated@example.com", "username": "alice"})

# Update rows matching condition
users.update_rows(set={"age": 21}, where=col("username") == "bob")

# Delete rows
users.delete_rows(where=col("age") < 18)
```

---

## 11. Relational: Reading and Filtering

```python
from bkndb import col

# Query builder filters
cond = (
    (col("age") >= 18) &
    (col("email").endswith("@company.com")) &
    col("metadata.role").is_in(["admin", "editor"])
)

# Select with ordering and pagination
results = db.select(
    "users",
    where=cond,
    order_by="-created_at", # descending
    limit=10,
    offset=0,
)
for row in results:
    print(row.pk, row["email"], row["metadata"])

# Lazy iteration for massive tables (chunked batches)
for row in db.iter_rows("users", where=col("age") > 20, batch_size=5000):
    process_row(row)
```

---

## 12. Relational: Aggregations

```python
from bkndb import Agg

# Total aggregations
[stats] = db.aggregate("users", [
    Agg.count(),
    Agg.avg("age"),
    Agg.min("age"),
    Agg.max("age"),
])
print(stats["count"], stats["avg_age"])

# Grouped aggregations
by_role = db.aggregate(
    "users",
    aggregates=[Agg.count(), Agg.avg("age")],
    group_by=["metadata.role"],
)
for item in by_role:
    print(item["metadata.role"], item["count"])
```

---

## 13. Transactions

```python
with db.transaction() as tx:
    node_id = tx.create_node("Account", {"balance": 1000})
    tx.insert("audit_log", {"action": "account_created", "ref_id": node_id})
    # Commits automatically at the end of the block
    # If any error is raised inside the block, rolls back automatically!
```

---

## 14. Bulk Sync: Graph + Tables

```python
from bkndb import NewNode

db.sync_batch(
    nodes=[
        ("Module", {"name": "engine"}),
        ("Function", {"name": "start"}),
    ],
    edges=[
        (NewNode(0), "EXPOSES", NewNode(1), {}),
    ],
    rows={
        "code_files": [
            {"path": "engine.rs", "bytes": 4096},
        ],
    },
)
```

---

## 15. SQL Engine

```python
# Execute queries with parameters
res = db.sql(
    "SELECT id, email, age FROM users WHERE age >= ? ORDER BY age DESC LIMIT 5",
    [21],
)

# Access results
print("Columns:", res.columns)
print("Rows as dicts:", res.dicts())
print("First scalar:", res.scalar())
```

---

## 16. Graph Query Language (`MATCH`)

```python
res = db.graph_query(
    "MATCH (a:Person {name: $name})-[:KNOWS*1..2]->(b:Person) "
    "WHERE b.age > 20 "
    "RETURN b.name AS friend, count(*) AS path_count "
    "ORDER BY friend",
    {"name": "Alice"},
)
print("Cypher results:", res.dicts())
```

---

## 17. Full-Text Search (BM25)

```python
# Create BM25 index on text column
db.create_fulltext_index("documents", "content")

# Search with prefix and BM25 ranking
hits = db.search_text("documents", "content", "embedded graph*", limit=5)
for h in hits:
    print(f"Row {h.row.pk}, Score: {h.score:.2f}")
```

---

## 18. Vector Search (Exact & HNSW Index)

```python
import bkndb

# 1. Insert vector embedding (using compact pack_vector)
embedding = [0.12, -0.45, 0.78, ...]
db.insert("documents", {
    "title": "LLM Guide",
    "emb": bkndb.pack_vector(embedding),
})

# 2. Build HNSW index for sub-millisecond retrieval on large collections
db.create_vector_index("documents", "emb", metric="cosine", m=16, ef_construction=200)

# 3. Perform approximate nearest neighbor search
results = db.search_vector("documents", "emb", embedding, limit=5, metric="cosine")
for r in results:
    print(f"Document PK: {r.row.pk}, Distance: {r.distance:.4f}")
```

---

## 19. Operations: Backup, Stats, Integrity, Compaction

```python
with bkndb.open("production.bkndb") as db:
    # 1. Reclaim deleted space
    db.compact()

    # 2. Hot Online Backup
    db.backup("backups/snapshot_daily.bkndb")

    # 3. Integrity Check
    db.verify_integrity()

    # 4. Storage Metrics
    stats = db.stats()
    print("Storage Stats:", stats.storage.file_size, stats.storage.reclaimable_bytes)
```

---

## 20. Import and Export: CSV / JSONL

```python
# Export
db.export_jsonl("users", "users_backup.jsonl")
db.export_csv("users", "users_backup.csv")

# Import (atomic transaction)
db.import_jsonl("users", "users_backup.jsonl", mode="upsert")
db.import_csv("users", "users_backup.csv", mode="insert")
```

---

## 21. Pandas and NetworkX Integration

```python
# Export relational table directly to Pandas DataFrame
df = db.select_df("users", col("age") >= 21)
print(df.head())

# Convert graph traversal to NetworkX MultiDiGraph
import networkx as nx
G = db.to_networkx(root_node, max_depth=3)
print(f"NetworkX graph has {G.number_of_nodes()} nodes and {G.number_of_edges()} edges")
```

---

## 22. Error Handling

All BknDb exceptions inherit from `bkndb.BknDbError`:

```python
import bkndb

try:
    db.insert("users", {"id": 1, "email": "test@test.com"})
    db.insert("users", {"id": 1, "email": "test@test.com"})
except bkndb.DuplicateKeyError as e:
    print("Primary key collision:", e)
except bkndb.ConstraintViolationError as e:
    print("NOT NULL or UNIQUE violated:", e)
except bkndb.DatabaseLockedError:
    print("Another process has locked this file!")
```

---

## 23. API Reference

### Top-Level Module
- `bkndb.open(path, ...)`
- `bkndb.in_memory()`
- `bkndb.pack_vector(list_of_floats)`
- `bkndb.col(name)`
- `bkndb.Agg`
- `bkndb.TableSchema`, `bkndb.Column`

---

## 24. Real-World Recipes

### GraphRAG Memory Pipeline
```python
with bkndb.open("graph_rag.bkndb") as db:
    with db.transaction() as tx:
        # Create Concept & Document Nodes
        doc = tx.create_node("Document", {"url": "https://arxiv.org/abs/2301"})
        chunk = tx.create_node("Chunk", {"text": "Transformers rely on attention."})
        tx.create_edge(doc, "CONTAINS", chunk)

        # Store in table with vector embeddings
        tx.insert("text_embeddings", {
            "chunk_node": chunk,
            "vector": bkndb.pack_vector(embed("Transformers rely on attention.")),
        })
```

---

## 25. Limitations and Performance Guidelines

1. **Process Exclusivity:** A single `.bkndb` file cannot be opened by multiple processes concurrently.
2. **Batch Operations:** Always use `sync_batch` or `db.transaction()` for batch writes to avoid repeated `fsync` disk flushes.
3. **Index Creation:** Create HNSW vector indexes *after* initial bulk data loading for up to 2.5x faster ingestion.
4. **Compaction:** Run `db.compact()` periodically to reclaim storage space after high-volume delete or update cycles.

---

## License

Licensed under the Apache License, Version 2.0 (see [LICENSE](../../LICENSE)).
