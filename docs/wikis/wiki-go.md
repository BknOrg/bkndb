# bkndb for Go — Complete Guide & API Reference

This guide targets **BknDb 0.3.1**. Persistent databases use Redb; LSM files
are a different format and are not automatically migrated.

The Go release module includes a C header and precompiled static BknDb libraries.
Building an application requires **Go 1.25+, CGO, and a C compiler**. Rust is only
needed by maintainers rebuilding those libraries. The resulting executable needs
no separate BknDb shared library, though OS system libraries may still be required.

```bash
go get github.com/BknOrg/bkndb/bindings/go/bkndb@v0.3.1
CGO_ENABLED=1 go build .
```

This version becomes available through `go get` after the distribution PR is
merged and the `bindings/go/bkndb/v0.3.1` tag is published.

| Platform | Supported architectures | Build requirement / minimum runtime |
| --- | --- | --- |
| Linux (glibc) | amd64, arm64 | GCC; glibc 2.28+ |
| macOS | amd64, arm64 | Xcode Command Line Tools; macOS 11.0+ |
| Windows | amd64 | MinGW-w64 GCC on PATH; CGO enabled |

For Debian/Ubuntu install `build-essential`; for macOS run `xcode-select --install`.
For Windows, install MSYS2's `mingw-w64-x86_64-gcc` package and add its
`mingw64/bin` directory to PATH. In PowerShell enable CGO with
`$env:CGO_ENABLED = "1"`, then run `go build .`. Compilers are needed on the build
machine only, not on machines running the executable.

The bundled Linux archive targets glibc 2.28. The final executable can require a
newer glibc if built with a newer system C toolchain; use a matching build
environment when deploying to older Linux systems.

Alpine/musl, other architectures, and `CGO_ENABLED=0` are unsupported. CGO cross
compilation requires a matching target C compiler; setting GOOS/GOARCH alone is
not sufficient. The public Go API and Redb storage behavior are unchanged.

For source development from the repository root (Python 3.9+ and Rust required):

```bash
# Replace linux_amd64 with your supported host platform.
rustup target add x86_64-unknown-linux-gnu
python3 scripts/go/package.py build linux_amd64
cd bindings/go/bkndb
CGO_ENABLED=1 go test ./...
```

Windows builds use Rust target `x86_64-pc-windows-gnu`; macOS uses
`x86_64-apple-darwin` or `aarch64-apple-darwin`. Use `python` instead of `python3`
where appropriate. Local source builds are not release distributions: Linux
release archives are rebuilt in manylinux 2.28 containers by CI.

SQL parameters remain variadic (`db.SQL(query, value1, value2)`) or a single
`map[string]any` for named parameters. Vector dimensions must match stored data.

This document is the comprehensive guide and complete API reference for the `bkndb` Go package (`github.com/BknOrg/bkndb/bindings/go/bkndb`): architecture, installation, public APIs, transactions, graph queries, vector & full-text search, runnable examples, and performance guidelines.

---

## Table of Contents

1. [What is bkndb and When to Use It](#1-what-is-bkndb-and-when-to-use-it)
2. [Installation & Architecture](#2-installation--architecture)
3. [Core Concepts](#3-core-concepts)
4. [Quick Start](#4-quick-start)
5. [Opening and Closing a Database](#5-opening-and-closing-a-database)
6. [Relational Tables & SQL Engine](#6-relational-tables--sql-engine)
7. [Graph: Nodes, Edges, and Traversal](#7-graph-nodes-edges-and-traversal)
8. [Graph Query Language (`MATCH`)](#8-graph-query-language-match)
9. [Full-Text Search (BM25)](#9-full-text-search-bm25)
10. [Vector Similarity Search (HNSW Index)](#10-vector-similarity-search-hnsw-index)
11. [Explicit ACID Transactions](#11-explicit-acid-transactions)
12. [Operations: Stats, Backup, and Compaction](#12-operations-stats-backup-and-compaction)
13. [Driver Architecture & Cross-Platform Details](#13-driver-architecture--cross-platform-details)
14. [Real-World Recipe: Lightweight Agent Memory](#14-real-world-recipe-lightweight-agent-memory)
15. [API Reference Summary](#15-api-reference-summary)
16. [Limitations and Best Practices](#16-limitations-and-best-practices)

---

## 1. What is bkndb and When to Use It

`bkndb` is an **embedded hybrid graph + relational + vector database** written in pure Rust with official Go bindings. It runs directly inside your Go application process (like SQLite), requiring zero background daemons, external services, or Docker containers. All data models are persisted into a **single `.bkndb` container file** backed by the pure Rust Redb engine.

| Model | Purpose | Go APIs |
| :--- | :--- | :--- |
| **Relational / SQL** | Tabular records, structured queries, filtering, indexing | `db.Exec()`, `db.SQL()`, `db.SQL()` |
| **Property Graph** | Labeled nodes, directed edges, graph properties, traversal | `db.AddNode()`, `db.AddEdge()`, `db.Neighbors()`, `db.GraphQuery()` |
| **Full-Text Search** | BM25 lexical text search across document chunks and conversations | `db.CreateFulltextIndex()`, `db.SearchText()` |
| **Vector Search** | HNSW approximate nearest neighbor search (Cosine, Euclidean, Dot) | `db.CreateVectorIndex()`, `db.SearchVector()` |
| **Transactions** | Unified ACID transactions across SQL and graph data | `db.Begin()`, `tx.Commit()`, `tx.Rollback()` |

### Ideal For:
- **Lightweight AI Agents (e.g. Hermes Agent)**: Single-binary agent deployment needing conversational history (SQL), episodic recall (BM25/vector), and tool/skill relationships (Graph).
- **Embedded Desktop & CLI Utilities**: Local storage with a single application executable; a C compiler is required at build time.
- **Local GraphRAG & Knowledge Bases**: Fast local similarity search combined with graph entity traversal.

---

## 2. Installation & Architecture

### Go Module

Use the installation commands and platform prerequisites above. Published modules
contain their own static archives; local checkout builds must first run the
packaging build script for the host platform.

---

## 3. Core Concepts

1. **Single File, Single Writer**: Opening `data.bkndb` acquires an exclusive lock. Only one process can open the file at a time.
2. **Unified Data Models**: Nodes, edges, and SQL tables reside in the same physical file and can be modified together within ACID transactions.
3. **Property Maps**: Properties in nodes and edges are represented using standard Go `map[string]any` and automatically serialized to JSON across the FFI bridge.

---

## 4. Quick Start

```go
package main

import (
	"fmt"
	"log"

	"github.com/BknOrg/bkndb/bindings/go/bkndb"
)

func main() {
	// 1. Open database (creates file if not exists)
	db, err := bkndb.Open("quickstart.bkndb")
	if err != nil {
		log.Fatal(err)
	}
	defer db.Close()

	// 2. Relational SQL
	if _, err := db.Exec("CREATE TABLE users (id INT PRIMARY KEY AUTOINCREMENT, name TEXT, email TEXT)"); err != nil {
		log.Fatal(err)
	}
	_, _ = db.Exec("INSERT INTO users (name, email) VALUES ('Alice', 'alice@example.com')")

	res, err := db.SQL("SELECT id, name, email FROM users")
	if err != nil {
		log.Fatal(err)
	}
	for _, row := range res.Rows {
		fmt.Printf("User #%v: %s <%s>\n", row[0], row[1], row[2])
	}

	// 3. Knowledge Graph
	aliceNode, _ := db.CreateNode("Person", map[string]any{"name": "Alice", "role": "admin"})
	toolNode, _ := db.CreateNode("Tool", map[string]any{"name": "terminal", "risk": "high"})
	_, _ = db.CreateEdge(aliceNode, "CAN_EXECUTE", toolNode, map[string]any{"granted": 2026})

	neighbors, _ := db.Neighbors(aliceNode, bkndb.DirectionOut, "CAN_EXECUTE")
	for _, n := range neighbors {
		fmt.Printf("Alice is connected to node #%d\n", n.NodeID)
	}
}
```

---

## 5. Opening and Closing a Database

### On-Disk Database
```go
db, err := bkndb.Open("app_data.bkndb")
if err != nil {
    // Fails if another process has the database open (locked)
    log.Fatalf("Failed to open database: %v", err)
}
defer db.Close()
```

### In-Memory Database (Ephemeral)
Useful for unit tests, temporary caches, and ephemeral agent sessions:
```go
db, err := bkndb.InMemory()
if err != nil {
    log.Fatal(err)
}
defer db.Close()
```

### Checking Close Status
```go
if db.IsClosed() {
    fmt.Println("Database handle is closed")
}
```

---

## 6. Relational Tables & SQL Engine

BknDb contains a built-in SQL parsing and execution engine supporting `CREATE TABLE`, `INSERT`, `SELECT`, `UPDATE`, and `DELETE`.

### Creating Tables and Inserting
```go
// Create table with auto-increment primary key
_, err := db.Exec("CREATE TABLE messages (id INT PRIMARY KEY AUTOINCREMENT, role TEXT, content TEXT, timestamp INT)")
if err != nil {
    log.Fatal(err)
}

// Insert rows
_, err = db.Exec("INSERT INTO messages (role, content, timestamp) VALUES ('user', 'Hello Agent!', 1792680000)")
```

### Querying Data
`db.SQL(query)` returns a `*QueryResult`:
```go
res, err := db.SQL("SELECT id, role, content FROM messages WHERE role = 'user' ORDER BY id DESC LIMIT 10")
if err != nil {
    log.Fatal(err)
}

// Columns returned
fmt.Println("Columns:", res.Columns)

// Access rows
for _, row := range res.Rows {
    fmt.Printf("[%v] %v: %v\n", row[0], row[1], row[2])
}
```

### Parameterized Queries
Safely pass arguments to prevent SQL injection:
```go
res, err := db.SQL(
    "SELECT id, content FROM messages WHERE role = $1 AND timestamp >= $2",
    "user", 1792600000,
)
```

---

## 7. Graph: Nodes, Edges, and Traversal

BknDb provides native property graph capabilities.

### Creating Nodes
```go
// CreateNode(label string, properties map[string]any) (uint64, error)
agentID, err := db.CreateNode("Agent", map[string]any{
    "name": "Hermes-7B",
    "version": 1.2,
    "active": true,
})

skillID, err := db.CreateNode("Skill", map[string]any{
    "name": "CodeInterpreter",
    "runtime": "python3",
})
```

### Inspecting and Updating Nodes
```go
// Get node
node, err := db.GetNode(agentID)
if err != nil {
    log.Fatal(err)
}
if node == nil {
    log.Fatal("node not found")
}
fmt.Printf("Node #%d [%s]: %v\n", node.ID, node.Label, node.Properties)

// Update properties (setProps map[string]any, optional unsetKeys ...string)
err = db.UpdateNode(agentID, map[string]any{
    "status": "ready",
    "last_seen": 1792681000,
})

// Delete node (also removes connected edges)
err = db.DeleteNode(agentID)
```

### Creating Edges
```go
// CreateEdge(from uint64, edgeType string, to uint64, properties map[string]any) (uint64, error)
edgeID, err := db.CreateEdge(agentID, "CAN_USE", skillID, map[string]any{
    "priority": 1,
    "timeout_sec": 30,
})

// Get edge
edge, err := db.GetEdge(edgeID)

// Delete edge
_, err = db.DeleteEdge(edgeID)
```

### Graph Traversal (`Neighbors`)
Find adjacent nodes using directional filtering:
```go
// Direction options:
// bkndb.DirectionOut  (0)
// bkndb.DirectionIn   (1)
// bkndb.DirectionBoth (2)

// Find all outgoing skills of the agent
neighbors, err := db.Neighbors(agentID, bkndb.DirectionOut, "CAN_USE")
if err != nil {
    log.Fatal(err)
}

for _, n := range neighbors {
    fmt.Printf("Adjacent Node ID: %d via Edge ID: %d\n", n.NodeID, n.EdgeID)
}

// Find all connections of any type
allNeighbors, _ := db.Neighbors(agentID, bkndb.DirectionBoth, "")
```

---

## 8. Graph Query Language (`MATCH`)

Execute Cypher-like pattern matching over your graph:

```go
res, err := db.GraphQuery("MATCH (a:Agent)-[r:CAN_USE]->(s:Skill) RETURN a.name, s.name")
if err != nil {
    log.Fatal(err)
}

for _, row := range res.Rows {
    fmt.Printf("Agent %s can use skill %s\n", row[0], row[1])
}
```

---

## 9. Full-Text Search (BM25)

BknDb includes an integrated BM25 inverted index for full-text search directly across relational table columns.

```go
// 1. Create BM25 index on table and column
err := db.CreateFulltextIndex("documents", "content")
if err != nil {
    log.Fatal(err)
}

// 2. Perform BM25 search
// SearchText(table, column, query string, limit int) ([]ScoredRow, error)
hits, err := db.SearchText("documents", "content", "hermes autonomous agent", 5)
if err != nil {
    log.Fatal(err)
}

for _, hit := range hits {
    fmt.Printf("[Score: %.4f] PK: %v Content: %v\n", hit.Score, hit.PK, hit.Values["content"])
}
```

---

## 10. Vector Similarity Search (HNSW Index)

BknDb supports Approximate Nearest Neighbor (ANN) vector search via Hierarchical Navigable Small World (HNSW) graphs.

### Metric Types
- `bkndb.MetricCosine` (`"cosine"`)
- `bkndb.MetricEuclidean` (`"euclidean"`)
- `bkndb.MetricDot` (`"dot"`)

### Creating Vector Index and Searching
```go
// 1. Create HNSW index
// Metric: Cosine, M: 16, efConstruction: 64
err := db.CreateVectorIndex("embeddings", "vec", bkndb.MetricCosine, 16, 64)
if err != nil {
    log.Fatal(err)
}

// 2. Query nearest vectors
// SearchVector(table, column string, vector []float32, limit int, metric VectorMetric) ([]ScoredRow, error)
queryEmbedding := []float32{0.1, 0.8, 0.3, 0.0}
results, err := db.SearchVector("embeddings", "vec", queryEmbedding, 5, bkndb.MetricCosine)
if err != nil {
    log.Fatal(err)
}

for _, res := range results {
    fmt.Printf("[Score: %.4f] Matched Row: %v\n", res.Score, res.Values)
}
```

---

## 11. Explicit ACID Transactions

When you need atomicity across multiple operations, use `db.Begin()`:

```go
tx, err := db.Begin()
if err != nil {
    log.Fatal(err)
}
// Ensure rollback if not committed
defer tx.Rollback()

// Execute SQL in transaction
if _, err := tx.Exec("INSERT INTO accounts (id, balance) VALUES (1, 500)"); err != nil {
    return err
}

if _, err := tx.Exec("INSERT INTO accounts (id, balance) VALUES (2, 300)"); err != nil {
    return err
}

// Commit transaction
if err := tx.Commit(); err != nil {
    log.Fatalf("Commit failed: %v", err)
}
```

---

## 12. Operations: Stats, Backup, and Compaction

### Database Statistics
```go
stats, err := db.Stats()
if err == nil {
    var fileBytes uint64
    if stats.Storage != nil { fileBytes = stats.Storage.FileBytes }
    fmt.Printf("Nodes: %d, Edges: %d, Tables: %d, File Size: %d bytes\n",
        stats.Nodes, stats.Edges, len(stats.Tables), fileBytes)
}
```

### Hot Backup
Create a consistent point-in-time copy of the database without stopping:
```go
err := db.Backup("backup_2026.bkndb")
if err != nil {
    log.Fatal(err)
}
```

### Database Compaction
Reclaim unused disk pages and optimize storage layout (in-place):
```go
err := db.Compact()
```

---

## 13. Driver Architecture & Cross-Platform Details

The public `DB` and `Tx` types delegate to a single internal CGO driver on every
supported target. `binding_cgo.go` includes the package-local `bkndb.h` and links
an explicit `${SRCDIR}/lib/<os>_<arch>/libbkndb_ffi.a` path. No dynamic loader or
runtime DLL extraction is involved. Unsupported targets and disabled CGO fail
at compile time with a diagnostic naming the supported platforms.

---

## 14. Real-World Recipe: Lightweight Agent Memory

```go
package main

import (
	"fmt"
	"log"

	"github.com/BknOrg/bkndb/bindings/go/bkndb"
)

type AgentMemory struct {
	db *bkndb.DB
}

func NewAgentMemory(path string) (*AgentMemory, error) {
	db, err := bkndb.Open(path)
	if err != nil {
		return nil, err
	}

	// Session messages table
	_, err = db.Exec(`CREATE TABLE IF NOT EXISTS messages (
		id INT PRIMARY KEY AUTOINCREMENT,
		session_id TEXT,
		role TEXT,
		content TEXT
	)`)
	if err != nil {
		db.Close()
		return nil, err
	}
	if err = db.CreateFulltextIndex("messages", "content"); err != nil {
		db.Close()
		return nil, err
	}

	return &AgentMemory{db: db}, nil
}

func (m *AgentMemory) SaveMessage(session, role, content string) error {
	_, err := m.db.Exec(
		"INSERT INTO messages (session_id, role, content) VALUES (?, ?, ?)",
		session, role, content,
	)
	return err
}

func (m *AgentMemory) RecallRelevant(query string, limit int) ([]string, error) {
	hits, err := m.db.SearchText("messages", "content", query, limit)
	if err != nil {
		return nil, err
	}
	var out []string
	for _, h := range hits {
		out = append(out, fmt.Sprintf("%v: %v", h.Values["role"], h.Values["content"]))
	}
	return out, nil
}

func main() {
	mem, err := NewAgentMemory("hermes.bkndb")
	if err != nil {
		log.Fatal(err)
	}
	defer mem.db.Close()

	mem.SaveMessage("session-1", "user", "How do I configure SearXNG?")
	mem.SaveMessage("session-1", "assistant", "You can set up SearXNG via Docker or local systemd.")

	results, _ := mem.RecallRelevant("SearXNG configuration", 5)
	for _, r := range results {
		fmt.Println("Recalled:", r)
	}
}
```

---

## 15. API Reference Summary

### Top-Level Constructors
- `bkndb.Open(path string) (*DB, error)`
- `bkndb.InMemory() (*DB, error)`

### `DB` Methods
- `db.Close() error`
- `db.IsClosed() bool`
- `db.Stats() (*DbStats, error)`
- `db.Backup(destPath string) error`
- `db.Compact() error`
- `db.Exec(sql string, params ...any) (uint64, error)`
- `db.SQL(sql string, params ...any) (*QueryResult, error)`
- `db.CreateNode(label string, props map[string]any) (uint64, error)`
- `db.GetNode(id uint64) (*Node, error)`
- `db.UpdateNode(id uint64, setProps map[string]any, unsetKeys ...string) error`
- `db.DeleteNode(id uint64) error`
- `db.CreateEdge(from uint64, edgeType string, to uint64, props map[string]any) (uint64, error)`
- `db.GetEdge(id uint64) (*Edge, error)`
- `db.DeleteEdge(id uint64) (bool, error)`
- `db.Neighbors(nodeID uint64, dir Direction, edgeType ...string) ([]Neighbor, error)`
- `db.GraphQuery(query string, params ...any) (*QueryResult, error)`
- `db.CreateFulltextIndex(table, column string) error`
- `db.SearchText(table, column, query string, limit int, matchAll ...bool) ([]ScoredRow, error)`
- `db.CreateVectorIndex(table, col string, metric VectorMetric, m, efConstr int) error`
- `db.SearchVector(table, col string, query []float32, limit int, metric VectorMetric, exact ...bool) ([]ScoredRow, error)`
- `db.Begin() (*Tx, error)`

### `Tx` Methods
- `tx.Exec(sql string, params ...any) (uint64, error)`
- `tx.SQL(sql string, params ...any) (*QueryResult, error)`
- `tx.Commit() error`
- `tx.Rollback() error`

---

## 16. Limitations and Best Practices

1. **Single Writer**: Do not attempt to open the same `.bkndb` file from multiple concurrent OS processes.
2. **Explicit Close**: Always `defer db.Close()` to ensure file locks are released and buffers flushed.
3. **Transaction Isolation**: During an active `tx := db.Begin()`, avoid running concurrent writes on the parent `db` until `tx.Commit()` or `tx.Rollback()` is called.

## Pilihan LSM dan WASM

This section describes unreleased working changes; an existing published tag may
predate this implementation. Publication requires a separate unused version tag.


`Open` tetap Redb pada driver native. Gunakan `OpenLSM` untuk format LSM.
Dengan `CGO_ENABLED=0 go build -tags=bkndb_wasm .`, seluruh SQL/graph/search berjalan
di wazero memakai I/O host Go. Driver ini memerlukan Go 1.25+, tanpa compiler C,
Rust, atau file library tambahan. Memory default 256 MiB dan dapat dikonfigurasi
melalui `OpenLSMWithWASMConfig`. File LSM memakai sidecar `.lock`; migrasi dari
Redb atau LSM lama dilakukan ke file baru.

Lihat [kontrak dan penggunaan LSM/WASM](../LSM_ENGINE.md) serta
[status audit](../LSM_AUDIT.md). Implementasi ini belum dipromosikan menjadi default.

Go 1.25 memerlukan macOS 12+; deployment target arsip Rust 11.0 tidak menurunkan
minimum OS runtime Go. [Referensi resmi](https://go.dev/doc/go1.25#darwin).
