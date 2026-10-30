# bkndb-go

This guide targets the **0.3 API** in this checkout. Persistent databases use
Redb. Old LSM files are not automatically migrated, even if they also end in
`.bkndb`. The Go module requires **Go 1.22+** and a native library from the same
checkout. `go get` installs the Go wrapper; it does not build that library.

From the repository root:

```bash
cargo build --release -p bkndb-ffi
```

For a local source dependency in another Go project, use a module replacement:

```bash
go mod edit -replace github.com/BknOrg/bkndb/bindings/go/bkndb=/absolute/path/to/bkn-db/bindings/go/bkndb
go get github.com/BknOrg/bkndb/bindings/go/bkndb
```

On Windows, put `target/release/bkndb_ffi.dll` next to your executable or in a
directory searched by the loader. No C compiler is required for the Go wrapper.
On Linux/macOS, cgo and a C compiler are required; use the repo's C header and
ensure the linker can find `libbkndb_ffi.so` / `libbkndb_ffi.dylib`. Set
`LD_LIBRARY_PATH` (Linux) or `DYLD_LIBRARY_PATH` (macOS) for runtime loading.
See [the Go CI workflow](../../../.github/workflows/go.yml) for test setup.

SQL parameters are variadic (`db.SQL(query, value1, value2)`) or a single
`map[string]any` for named parameters. Search hits expose `PK`, `Values`, and
`Score`; cosine/dot scores sort highest first and Euclidean distances lowest
first. Vector dimensions are inferred from stored vectors and must match.


Official Go bindings for **BknDb** — Embedded hybrid graph-relational database engine with ACID transactions, BM25 full-text search, and HNSW vector similarity search.

The Go wrapper uses the standard library only and requires the matching BknDb native library at runtime.

---

## Features

- **Relational Tables & SQL**: Native SQL execution with parameters, auto-increment keys, and ACID transactions.
- **Full-Text Search (BM25)**: Fast text search over session memories, documents, and messages.
- **Vector Search (HNSW)**: Cosine, Euclidean, and Dot-Product similarity search for semantic memory & embeddings.
- **Knowledge Graph**: Nodes, directed edges, graph properties, traversal (`Neighbors`), and `MATCH ... RETURN` graph queries.
- **Explicit ACID Transactions**: Atomic `Begin()`, `Commit()`, and `Rollback()`.
- **Zero-Dependency on Windows**: Uses Windows native dynamic loading (`syscall.NewLazyDLL`), allowing direct `go run` / `go build` **without requiring GCC or MinGW**.
- **cgo Support**: Uses standard C-ABI (`bkndb.h`) on Linux, macOS, and POSIX systems.

---

## Installation & Setup

In your Go project (e.g. your Lightweight Hermes Agent):

```bash
# Add module replace or install
go get github.com/BknOrg/bkndb/bindings/go/bkndb
```

Ensure `bkndb_ffi.dll` (on Windows) or `libbkndb_ffi.so` (on Linux) is present in your executable directory, working directory, or system PATH.

---

## Quickstart

```go
package main

import (
	"fmt"
	"log"

	"github.com/BknOrg/bkndb/bindings/go/bkndb"
)

func main() {
	// Open or create persistent database
	db, err := bkndb.Open("hermes.bkndb")
	if err != nil {
		log.Fatal(err)
	}
	defer db.Close()

	// 1. Relational SQL & BM25 Full-Text Search
	_, _ = db.Exec("CREATE TABLE memory (id INT PRIMARY KEY AUTOINCREMENT, role TEXT, content TEXT)")
	_, _ = db.Exec("INSERT INTO memory (role, content) VALUES ('user', 'Lightweight Hermes Agent in Go')")
	_ = db.CreateFulltextIndex("memory", "content")

	hits, _ := db.SearchText("memory", "content", "Hermes", 5)
	for _, h := range hits {
		fmt.Printf("Hit: %s (score: %.2f)\n", h.Values["content"], h.Score)
	}

	// 2. Knowledge Graph for Agent Skills & Tools
	agentID, _ := db.CreateNode("Agent", map[string]any{"name": "Hermes"})
	toolID, _ := db.CreateNode("Tool", map[string]any{"name": "web_search"})
	_, _ = db.CreateEdge(agentID, "USES", toolID, map[string]any{"since": 2026})

	neighbors, _ := db.Neighbors(agentID, bkndb.DirectionOut, "USES")
	fmt.Printf("Agent connected to %d tool(s)\n", len(neighbors))

	// 3. Vector Search (HNSW)
	_, _ = db.Exec("CREATE TABLE embeddings (id INT PRIMARY KEY, text TEXT, vec LIST)")
	_ = db.CreateVectorIndex("embeddings", "vec", bkndb.MetricCosine, 16, 200)
	_, _ = db.Exec("INSERT INTO embeddings (id, text, vec) VALUES (1, 'Agent Fact', [0.1, 0.9])")

	vecHits, _ := db.SearchVector("embeddings", "vec", []float32{0.1, 0.85}, 1, bkndb.MetricCosine)
	if len(vecHits) > 0 {
		fmt.Printf("Nearest fact: %s\n", vecHits[0].Values["text"])
	}
}
```

---

## Testing

Run tests inside `bindings/go/bkndb`:

```bash
go test -v ./...
```

---

## Documentation

For a comprehensive guide, architecture deep-dive, transactions, and full API reference, see [wiki-go.md](../../../docs/wikis/wiki-go.md).
