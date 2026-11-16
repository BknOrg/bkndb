# bkndb-go

This guide targets **BknDb 0.3.1**. Persistent databases use Redb; LSM files
are a different format and are not automatically migrated.

The Go release module includes a C header and precompiled static BknDb libraries.
Building an application requires **Go 1.22+, CGO, and a C compiler**. Rust is only
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

Official Go bindings for **BknDb** — Embedded hybrid graph-relational database engine with ACID transactions, BM25 full-text search, and HNSW vector similarity search.

The Go wrapper uses the standard library and CGO to link the bundled static BknDb library.

---

## Features

- **Relational Tables & SQL**: Native SQL execution with parameters, auto-increment keys, and ACID transactions.
- **Full-Text Search (BM25)**: Fast text search over session memories, documents, and messages.
- **Vector Search (HNSW)**: Cosine, Euclidean, and Dot-Product similarity search for semantic memory & embeddings.
- **Knowledge Graph**: Nodes, directed edges, graph properties, traversal (`Neighbors`), and `MATCH ... RETURN` graph queries.
- **Explicit ACID Transactions**: Atomic `Begin()`, `Commit()`, and `Rollback()`.
- **Static CGO linking**: Uses the bundled C header and static archive on all five supported platform targets.

---

## Installation & Setup

In your Go project (e.g. your Lightweight Hermes Agent):

```bash
go get github.com/BknOrg/bkndb/bindings/go/bkndb@v0.3.1
CGO_ENABLED=1 go build .
```

The tagged module includes its matching static library. No BknDb runtime library path configuration is needed.

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
