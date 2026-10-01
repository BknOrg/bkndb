# bkndb — Python Bindings

[![PyPI Version](https://img.shields.io/pypi/v/bkndb.svg)](https://pypi.org/project/bkndb/)
[![Python Version](https://img.shields.io/pypi/pyversions/bkndb.svg)](https://pypi.org/project/bkndb/)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)

**Embedded hybrid graph + relational + vector database engine for Python.**

`bkndb` runs completely in-process (like SQLite), persists to a single native `.bkndb` container file, and provides unified ACID transactions across property graphs, relational tables, BM25 full-text search, and HNSW vector similarity search.

---

## Installation

```bash
pip install bkndb                     # Pre-built wheels for Windows, Linux, and macOS
pip install "bkndb[pandas,networkx]"  # With Pandas DataFrame & NetworkX integration
```

Requires **Python 3.10+**. One single wheel per OS/architecture serves all Python 3.x versions.

---

## Quick Start

```python
import bkndb
from bkndb import TableSchema, Column, col, Agg

# Open or create a persistent database file
with bkndb.open("knowledge.bkndb") as db: # or bkndb.in_memory()
    # 1. Define and create a relational table
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
        # Create graph entities
        doc = tx.create_node("Document", {"title": "Attention Is All You Need"})
        concept = tx.create_node("Concept", {"name": "Transformer"})
        tx.create_edge(doc, "DISCUSSES", concept, {"relevance": 0.95})

        # Insert relational row referencing the graph node
        tx.insert("chunks", {"doc_id": doc, "text": "Self-attention mechanism...", "tokens": 512})

    # 3. Query relational tables
    rows = db.select("chunks", col("tokens") >= 500, order_by="-tokens", limit=5)
    print("Found chunks:", rows)

    # 4. Traversal & Graph Analysis
    for hit in db.traverse(doc, max_depth=2):
        target = db.get_node(hit.node_id)
        print(f"Hop {hit.depth}: ({target.label}) -> {target.properties}")

    # 5. Execute raw SQL or Cypher MATCH queries
    sql_res = db.sql("SELECT doc_id, COUNT(*) AS count FROM chunks GROUP BY doc_id").dicts()
    print("SQL Stats:", sql_res)

    match_res = db.graph_query(
        "MATCH (d:Document)-[:DISCUSSES]->(c:Concept) RETURN d.title, c.name"
    ).dicts()
    print("Graph Relations:", match_res)
```

---

## Feature Overview

### 1. Database Handles & Context Management
- `bkndb.open(path, ...)`: Opens or creates a `.bkndb` file.
- `bkndb.in_memory()`: Creates an ephemeral, in-memory database instance.
- `db.close()`: Explicitly closes the database and releases file locks.
- `db.compact()`: Rewrites the database file, reclaiming space from deleted records.
- `db.backup(dest_path)`: Performs an online hot backup without stopping reads or writes.
- `db.verify_integrity()`: Validates CRC32 checksums of all storage blocks.
- `db.stats()`: Returns entity counts and low-level storage metrics (`reclaimable_bytes`, file size).

---

### 2. Property Graph & Algorithms
- **CRUD:** `create_node`, `create_nodes_bulk`, `get_node`, `update_node`, `delete_node`, `create_edge`, `create_edges_bulk`, `get_edge`, `update_edge`, `delete_edge`.
- **Topologies:** `neighbors(id, direction, edge_type)`, `degree(id, direction)`.
- **Shortest Path (BFS):**
  ```python
  path = db.find_shortest_path(start_node, target_node, edge_type="CALLS")
  # Returns: distance, list of hops
  ```
- **Dijkstra Weighted Path:**
  ```python
  route = db.find_weighted_path(home_node, office_node, weight="minutes")
  # Returns: route.cost, route.path
  ```
- **Blast Radius / Impact Traversal:**
  ```python
  hits = db.traverse(root_node, direction=bkndb.Direction.INCOMING, max_depth=3)
  ```
- **Top Hubs & Centrality:**
  ```python
  top_entities = db.top_hubs(k=10)
  ```
- **Cascading Deletion:**
  ```python
  stats = db.cascade_delete(node_id)
  ```

---

### 3. Relational Tables & Query Builder
- **Schema Definition:** `TableSchema(name, columns, primary_key, auto_increment, indexes)`.
- **Constraints:** `nullable=False` (NOT NULL), `unique=True` (UNIQUE), `default=value`.
- **Mutations:** `insert`, `insert_many`, `upsert`, `upsert_many`, `update_rows`, `delete_rows`.
- **Query Builder (`col`):**
  ```python
  from bkndb import col

  # Comparisons & Boolean logic
  cond = (col("age") >= 18) & (col("status") == "active")

  # String & List operators
  cond = col("email").endswith("@company.com") | col("tags").contains("vip")

  # Nested JSON / Map access
  cond = col("metadata.author.id") == 42

  rows = db.select("users", where=cond, order_by="-created_at", limit=20)
  ```
- **Aggregations:**
  ```python
  [stats] = db.aggregate("users", [Agg.count(), Agg.avg("age"), Agg.max("salary")])
  grouped = db.aggregate("orders", [Agg.sum("amount")], group_by=["customer_id"])
  ```

---

### 4. Text Query Languages: SQL & Cypher MATCH

#### SQL
```python
db.sql(
    "SELECT category, COUNT(*) AS count, AVG(price) AS avg_price "
    "FROM products WHERE in_stock = ? GROUP BY category ORDER BY count DESC",
    [True],
).dicts()
```

#### Cypher MATCH
```python
db.graph_query(
    "MATCH (u:User {name: $user})-[:FRIEND*1..2]->(f:User) "
    "WHERE f.age > 21 "
    "RETURN f.name AS friend, count(*) AS paths ORDER BY f.name",
    {"user": "Alice"},
).dicts()
```

---

### 5. Full-Text Search (BM25) & Vector Search (HNSW)

```python
# 1. Full-Text BM25 Search
db.create_fulltext_index("articles", "content")
hits = db.search_text("articles", "content", "graph machine learning", limit=5)
for h in hits:
    print(f"Row {h.row.pk}, Score: {h.score:.2f}")

# 2. Vector Search (Exact or HNSW ANN)
# Embedding can be float list or compact binary pack_vector()
embedding = model.encode("semantic query")
db.insert("articles", {"title": "Paper", "emb": bkndb.pack_vector(embedding)})

# Build HNSW index for high-scale fast search
db.create_vector_index("articles", "emb", metric="cosine", m=16, ef_construction=200)

results = db.search_vector("articles", "emb", embedding, limit=5, metric="cosine")
for res in results:
    print(f"PK: {res.row.pk}, Distance: {res.distance:.4f}")
```

---

### 6. High-Performance Bulk Synchronization (`sync_batch`)

Atomic ingestion of thousands of graph nodes, edges, and relational rows without transaction overhead:

```python
from bkndb import NewNode

db.sync_batch(
    nodes=[
        ("File", {"path": "src/main.rs"}),
        ("Function", {"name": "main"}),
    ],
    edges=[
        (repo_node_id, "CONTAINS", NewNode(0), {}),
        (NewNode(0), "DEFINES", NewNode(1), {}),
    ],
    rows={
        "ast_metadata": [
            {"file_path": "src/main.rs", "lines": 120},
        ],
    },
)
```

---

### 7. Pandas & NetworkX Integration

```python
# Export relational table directly to pandas DataFrame (indexed by primary key)
df = db.select_df("users", col("status") == "active")

# Convert graph traversal to NetworkX MultiDiGraph
G = db.to_networkx(start_node_id, max_depth=3)
```

---

### 8. Import & Export (CSV and JSONLines)

```python
# Export
db.export_jsonl("users", "users_backup.jsonl")
db.export_csv("users", "users_backup.csv")

# Import (executed inside a single atomic transaction)
db.import_jsonl("users", "users_backup.jsonl", mode="upsert")
db.import_csv("users", "new_users.csv", mode="insert")
```

---

## Development & Building from Source

```bash
cd bindings/python
python -m venv .venv
# Activate virtualenv (Linux/macOS: source .venv/bin/activate, Windows: .venv\Scripts\activate)
pip install maturin pytest
maturin develop --release
pytest
```

---

## License

Licensed under the Apache License, Version 2.0 (see [LICENSE](../../LICENSE)).
