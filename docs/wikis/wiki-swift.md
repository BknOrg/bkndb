# bkndb for Swift — Complete Guide & API Reference

This document is the comprehensive guide and complete API reference for using `bkndb` in Swift (iOS, macOS, visionOS, and server-side Swift via SPM or XCFramework).

---

## Table of Contents

1. [What is bkndb and When to Use It](#1-what-is-bkndb-and-when-to-use-it)
2. [Installation & Setup](#2-installation--setup)
3. [Core Architecture](#3-core-architecture)
4. [Quick Start](#4-quick-start)
5. [Opening and Closing a Database](#5-opening-and-closing-a-database)
6. [Values & Types (`FfiPropValue`)](#6-values--types-ffipropvalue)
7. [Graph: Nodes, Edges, and Traversal](#7-graph-nodes-edges-and-traversal)
8. [Graph Query Language (`MATCH`)](#8-graph-query-language-match)
9. [Relational Tables & SQL](#9-relational-tables--sql)
10. [Full-Text Search (BM25)](#10-full-text-search-bm25)
11. [Vector Search (HNSW Index)](#11-vector-search-hnsw-index)
12. [Transactions (`BknDbTransaction`)](#12-transactions-bkndbtransaction)
13. [Database Operations: Stats, Backup, Compaction](#13-database-operations-stats-backup-compaction)
14. [Error Handling (`FfiBknError`)](#14-error-handling-ffibknerror)
15. [Real-World Recipe: Offline iOS AI Memory & Graph](#15-real-world-recipe-offline-ios-ai-memory--graph)
16. [API Reference Summary](#16-api-reference-summary)
17. [Limitations and Best Practices](#17-limitations-and-best-practices)

---

## 1. What is bkndb and When to Use It

`bkndb` is an **embedded hybrid graph + relational + vector database** written in pure Rust. Through UniFFI Swift bindings, it runs completely in-process on Apple platforms (iOS, macOS, visionOS, watchOS, and Linux server-side Swift). All data is persisted into a **single `.bkndb` container file** backed by the pure Rust Redb engine.

| Model | Purpose | Primary Swift APIs |
| :--- | :--- | :--- |
| **Relational / SQL** | Structured tabular records with schemas, constraints, and SQL | `engine.createTable()`, `engine.insert()`, `engine.sql()` |
| **Property Graph** | Labeled nodes, directed edges, properties, BFS shortest path, Dijkstra | `engine.createNode()`, `engine.createEdge()`, `engine.neighbors()`, `engine.findShortestPath()` |
| **Full-Text Search** | BM25 lexical search over local text and documents | `engine.createFulltextIndex()`, `engine.searchText()` |
| **Vector Search** | HNSW approximate nearest neighbor search (Cosine, Euclidean, Dot) | `engine.createVectorIndex()`, `engine.searchVector()` |
| **Transactions** | Unified ACID commits across graph and relational tables | `try engine.beginTransaction()` |

### Ideal For:
- **Offline-First iOS Apps**: Store user models, relational data, and local embeddings without a cloud database.
- **On-Device LLM & AI Agents**: Local conversational memory, BM25 text retrieval, and knowledge graph recall directly on iPhone/Mac.
- **Desktop macOS Utilities**: High-performance local document indexer and relationship analyzer.

---

## 2. Installation & Setup

### Option A: Swift Package Manager (Recommended)
Add the package or include the generated XCFramework and `bkndb_ffi.swift` in your `Package.swift`:

```swift
// Package.swift
dependencies: [
    .package(url: "https://github.com/BknOrg/bkndb.git", from: "0.2.0")
],
targets: [
    .target(
        name: "MyApp",
        dependencies: [
            .product(name: "BknDb", package: "bkndb")
        ]
    )
]
```

### Option B: Direct XCFramework / Source Integration
1. Build the Rust static library/framework:
   ```bash
   cargo build --release -p bkndb-ffi
   ```
2. Add `bkndb_ffi.swift` and `bkndb_ffiFFI.h` to your Xcode project target.
3. Link the compiled static library (`libbkndb_ffi.a`) or XCFramework.

---

## 3. Core Architecture

The Swift interface is powered by Mozilla's UniFFI. It provides:
- Strongly typed Swift structs and enums (`FfiNodeRecord`, `FfiEdgeRecord`, `FfiPropValue`).
- Automatic memory management (ARC) managing underlying Rust heap references.
- Swift `throws` for native error handling using `FfiBknError`.

---

## 4. Quick Start

```swift
import Foundation
import BknDb

do {
    // 1. Open or create local database file
    let docsUrl = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
    let dbPath = docsUrl.appendingPathComponent("agent.bkndb").path
    let engine = try BknDbEngine.open(path: dbPath)
    defer { try? engine.close() }

    // 2. Relational SQL Table
    try engine.sql(query: "CREATE TABLE memories (id INT PRIMARY KEY AUTOINCREMENT, content TEXT)", params: [])
    try engine.sql(query: "INSERT INTO memories (content) VALUES ('Local Swift Agent using BknDb')", params: [])

    // 3. Full-Text Search
    try engine.createFulltextIndex(table: "memories", column: "content")
    let hits = try engine.searchText(table: "memories", column: "content", query: "Swift Agent", limit: 5)
    for hit in hits {
        print("Hit [Score: \(hit.score)]: \(hit.row)")
    }

    // 4. Knowledge Graph
    let agentNodeId = try engine.createNode(input: FfiNodeInput(
        label: "Agent",
        properties: ["name": .text("Hermes-Swift"), "version": .real(1.0)]
    ))
    let skillNodeId = try engine.createNode(input: FfiNodeInput(
        label: "Skill",
        properties: ["name": .text("PhotoAnalyzer")]
    ))
    _ = try engine.createEdge(input: FfiEdgeInput(
        source: agentNodeId,
        target: skillNodeId,
        edgeType: "HAS_SKILL",
        properties: ["priority": .integer(1)]
    ))

    let neighbors = try engine.neighbors(nodeId: agentNodeId, direction: .outgoing, edgeType: "HAS_SKILL")
    print("Agent is connected to \(neighbors.count) skills.")
} catch {
    print("BknDb Error: \(error)")
}
```

---

## 5. Opening and Closing a Database

### Persistent Database
```swift
let engine = try BknDbEngine.open(path: "/path/to/my_data.bkndb")
defer {
    try? engine.close()
}
```

### In-Memory Database (Ephemeral)
Ideal for unit testing and temporary caching:
```swift
let memEngine = try BknDbEngine.inMemory()
defer {
    try? memEngine.close()
}
```

### Inspecting State
```swift
if engine.isClosed() {
    print("Database is closed")
}
```

---

## 6. Values & Types (`FfiPropValue`)

Property graph values and relational column values use the `FfiPropValue` enum:

```swift
let propString: FfiPropValue = .text("Hermes")
let propInt: FfiPropValue = .integer(42)
let propDouble: FfiPropValue = .real(3.14159)
let propBool: FfiPropValue = .boolean(true)
let propBytes: FfiPropValue = .blob(Data([0x01, 0x02, 0x03]))
let propNull: FfiPropValue = .null
let propList: FfiPropValue = .list([.text("swift"), .text("rust")])
let propMap: FfiPropValue = .map(["author": .text("BknOrg")])
```

---

## 7. Graph: Nodes, Edges, and Traversal

### Creating and Querying Nodes
```swift
// Create single node
let nodeId = try engine.createNode(input: FfiNodeInput(
    label: "Person",
    properties: ["name": .text("Alice"), "age": .integer(29)]
))

// Inspect node
let node = try engine.getNode(id: nodeId)
print("Node ID: \(node.id), Label: \(node.label)")

// Update properties
try engine.updateNodeProperties(
    nodeId: nodeId,
    setProps: ["verified": .boolean(true)],
    unsetProps: ["temporary_field"]
)

// Delete node (and its edges)
try engine.deleteNode(id: nodeId)
```

### Creating Edges
```swift
let edgeId = try engine.createEdge(input: FfiEdgeInput(
    source: aliceId,
    target: bobId,
    edgeType: "FRIENDS_WITH",
    properties: ["since": .integer(2026)]
))

// Delete edge
try engine.deleteEdge(id: edgeId)
```

### Directional Traversal
```swift
// FfiDirection: .outgoing, .incoming, .both
let friends = try engine.neighbors(
    nodeId: aliceId,
    direction: .outgoing,
    edgeType: "FRIENDS_WITH"
)
for n in friends {
    print("Friend Node ID: \(n.nodeId) via Edge ID: \(n.edgeId)")
}
```

### Graph Algorithms
```swift
// BFS Shortest Path (hop count)
if let path = try engine.findShortestPath(from: aliceId, to: davidId, edgeType: "FRIENDS_WITH") {
    print("Shortest path: \(path.nodeIds) with distance: \(path.distance)")
}

// Dijkstra Weighted Shortest Path
if let weighted = try engine.findWeightedPath(from: cityA, to: cityB, weightProp: "km", edgeType: "ROAD") {
    print("Total distance: \(weighted.cost) km")
}

// Degree Centrality (Top Hubs)
let hubs = try engine.topHubs(k: 5)
for hub in hubs {
    print("Hub Node \(hub.nodeId) has degree \(hub.degree)")
}
```

---

## 8. Graph Query Language (`MATCH`)

Execute Cypher pattern queries:

```swift
let queryResult = try engine.graphQuery(query: "MATCH (a:Agent)-[r:HAS_SKILL]->(s:Skill) RETURN a.name, s.name")
for row in queryResult.rows {
    print("Agent: \(row["a.name"] ?? .null) -> Skill: \(row["s.name"] ?? .null)")
}
```

---

## 9. Relational Tables & SQL

### Direct SQL Execution
```swift
try engine.sql(
    query: "CREATE TABLE items (id INT PRIMARY KEY AUTOINCREMENT, name TEXT, price REAL)",
    params: []
)

try engine.sql(
    query: "INSERT INTO items (name, price) VALUES ($1, $2)",
    params: [.text("Widget"), .real(19.99)]
)

let result = try engine.sql(
    query: "SELECT id, name, price FROM items WHERE price < $1",
    params: [.real(50.0)]
)

for row in result.rows {
    print("Item: \(row["name"] ?? .null) Price: \(row["price"] ?? .null)")
}
```

---

## 10. Full-Text Search (BM25)

```swift
// 1. Create index
try engine.createFulltextIndex(table: "documents", column: "text")

// 2. Search
let hits = try engine.searchText(table: "documents", column: "text", query: "machine learning agent", limit: 10)
for hit in hits {
    print("Row \(hit.rowId), Score: \(hit.score)")
}
```

---

## 11. Vector Search (HNSW Index)

Approximate nearest neighbor search for embeddings on iOS/macOS:

```swift
// 1. Create HNSW Vector Index
// Dimensions: 512, Metric: .cosine, M: 16, efConstruction: 64
try engine.createVectorIndex(
    table: "embeddings",
    column: "vec",
    dimensions: 512,
    metric: .cosine,
    m: 16,
    efConstruction: 64
)

// 2. Query nearest vectors
let queryVec: [Float] = [...] // 512-dim embedding
let matches = try engine.searchVector(
    table: "embeddings",
    column: "vec",
    query: queryVec,
    k: 5,
    efSearch: 32
)

for match in matches {
    print("Cosine Match Score: \(match.score), Row: \(match.row)")
}
```

---

## 12. Transactions (`BknDbTransaction`)

Explicit ACID transactions across graph and relational tables:

```swift
let tx = try engine.beginTransaction()
do {
    try tx.sql(query: "INSERT INTO logs (msg) VALUES ('Transaction step 1')", params: [])
    _ = try tx.createNode(input: FfiNodeInput(label: "TxNode", properties: [:]))
    
    // Commit all changes atomically
    try tx.commit()
} catch {
    // Rollback on failure
    try tx.rollback()
    throw error
}
```

---

## 13. Database Operations: Stats, Backup, Compaction

```swift
// Stats
let stats = try engine.stats()
print("Nodes: \(stats.nodeCount), Edges: \(stats.edgeCount), Size: \(stats.fileSizeBytes) bytes")

// Hot Backup
try engine.backup(destPath: "/path/to/backup.bkndb")

// Online Compaction
try engine.compact(destPath: "/path/to/compacted.bkndb")

// Integrity Verification
let report = try engine.verifyIntegrity()
if !report.isClean {
    print("Integrity issues: \(report.issues)")
}
```

---

## 14. Error Handling (`FfiBknError`)

Methods throw `FfiBknError`. Catch specific failure modes:

```swift
do {
    let engine = try BknDbEngine.open(path: "/locked/file.bkndb")
} catch FfiBknError.DatabaseLocked {
    print("File is locked by another process.")
} catch FfiBknError.TableNotFound(let tableName) {
    print("Table not found: \(tableName)")
} catch FfiBknError.IndexAlreadyExists(let idx) {
    print("Index already exists: \(idx)")
} catch {
    print("Other error: \(error)")
}
```

---

## 15. Real-World Recipe: Offline iOS AI Memory & Graph

```swift
import Foundation
import BknDb

final class LocalMemoryStore {
    private let engine: BknDbEngine

    init(dbUrl: URL) throws {
        self.engine = try BknDbEngine.open(path: dbUrl.path)
        try setupSchema()
    }

    private func setupSchema() throws {
        try engine.sql(
            query: "CREATE TABLE IF NOT EXISTS conversation (id INT PRIMARY KEY AUTOINCREMENT, role TEXT, content TEXT)",
            params: []
        )
        try? engine.createFulltextIndex(table: "conversation", column: "content")
    }

    func saveMessage(role: String, content: String) throws {
        try engine.sql(
            query: "INSERT INTO conversation (role, content) VALUES ($1, $2)",
            params: [.text(role), .text(content)]
        )
    }

    func searchMessages(query: String) throws -> [String] {
        let hits = try engine.searchText(table: "conversation", column: "content", query: query, limit: 5)
        return hits.compactMap { hit in
            if case .text(let content) = hit.row["content"] {
                return content
            }
            return nil
        }
    }
}
```

---

## 16. API Reference Summary

### `BknDbEngine`
- `open(path: String) throws -> BknDbEngine`
- `inMemory() throws -> BknDbEngine`
- `close() throws`
- `isClosed() -> Bool`
- `sql(query: String, params: [FfiPropValue]) throws -> FfiQueryResult`
- `createNode(input: FfiNodeInput) throws -> UInt64`
- `getNode(id: UInt64) throws -> FfiNodeRecord`
- `deleteNode(id: UInt64) throws`
- `updateNodeProperties(nodeId: UInt64, setProps: [String: FfiPropValue], unsetProps: [String]) throws`
- `createEdge(input: FfiEdgeInput) throws -> UInt64`
- `getEdge(id: UInt64) throws -> FfiEdgeRecord`
- `deleteEdge(id: UInt64) throws`
- `neighbors(nodeId: UInt64, direction: FfiDirection, edgeType: String?) throws -> [FfiNeighbor]`
- `findShortestPath(from: UInt64, to: UInt64, edgeType: String?) throws -> FfiPathResult?`
- `findWeightedPath(from: UInt64, to: UInt64, weightProp: String, edgeType: String?) throws -> FfiWeightedPath?`
- `topHubs(k: UInt32) throws -> [FfiHubRecord]`
- `graphQuery(query: String) throws -> FfiQueryResult`
- `createFulltextIndex(table: String, column: String) throws`
- `searchText(table: String, column: String, query: String, limit: UInt32) throws -> [FfiScoredRow]`
- `createVectorIndex(table: String, column: String, dimensions: UInt32, metric: FfiVectorMetric, m: UInt32, efConstruction: UInt32) throws`
- `searchVector(table: String, column: String, query: [Float], k: UInt32, efSearch: UInt32) throws -> [FfiScoredRow]`
- `beginTransaction() throws -> BknDbTransaction`
- `stats() throws -> FfiDbStats`
- `backup(destPath: String) throws`
- `compact(destPath: String) throws`
- `verifyIntegrity() throws -> FfiIntegrityReport`

---

## 17. Limitations and Best Practices

1. **App Sandbox on iOS**: Ensure database files are placed in `Documents` or `Application Support` directories so they persist across launches.
2. **One Process per File**: Background extensions or app groups must not open the same `.bkndb` file concurrently; use a single coordinator process.
3. **Explicit Close in App Lifecycle**: Close the database handle or commit active transactions in `applicationWillTerminate` or background transitions.
