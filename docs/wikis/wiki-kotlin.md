# bkndb for Kotlin & Android — Complete Guide & API Reference

This document is the comprehensive guide and complete API reference for using `bkndb` in Kotlin (Android, JVM desktop, and server-side Kotlin via JNA and UniFFI).

---

## Table of Contents

1. [What is bkndb and When to Use It](#1-what-is-bkndb-and-when-to-use-it)
2. [Installation & Gradle Setup](#2-installation--gradle-setup)
3. [Core Concepts](#3-core-concepts)
4. [Quick Start](#4-quick-start)
5. [Opening and Closing a Database](#5-opening-and-closing-a-database)
6. [Values & Types (`FfiPropValue`)](#6-values--types-ffipropvalue)
7. [Relational Tables & SQL Engine](#7-relational-tables--sql-engine)
8. [Graph: Nodes, Edges, and Traversal](#8-graph-nodes-edges-and-traversal)
9. [Graph Query Language (`MATCH`)](#9-graph-query-language-match)
10. [Full-Text Search (BM25)](#10-full-text-search-bm25)
11. [Vector Similarity Search (HNSW Index)](#11-vector-similarity-search-hnsw-index)
12. [Transactions (`BknDbTransaction`)](#12-transactions-bkndbtransaction)
13. [Operations: Stats, Backup, Compaction, and Integrity](#13-operations-stats-backup-compaction-and-integrity)
14. [Error Handling (`FfiBknErrorException`)](#14-error-handling-ffibknerrorexception)
15. [Real-World Recipe: Android Local Agent & Knowledge Graph](#15-real-world-recipe-android-local-agent--knowledge-graph)
16. [API Reference Summary](#16-api-reference-summary)
17. [Limitations and Best Practices](#17-limitations-and-best-practices)

---

## 1. What is bkndb and When to Use It

`bkndb` is an **embedded hybrid graph + relational + vector database** written in pure Rust. Through UniFFI Kotlin bindings and JNA, it runs in-process on Android devices and desktop JVM environments. All data is persisted into a **single `.bkndb` container file** backed by the pure Rust Redb engine.

| Model | Purpose | Primary Kotlin APIs |
| :--- | :--- | :--- |
| **Relational / SQL** | Structured tables with schemas, constraints, and standard SQL | `engine.sql()`, `engine.createTable()`, `engine.insert()` |
| **Property Graph** | Labeled nodes, directed edges, properties, BFS shortest path, Dijkstra | `engine.createNode()`, `engine.createEdge()`, `engine.neighbors()`, `engine.findShortestPath()` |
| **Full-Text Search** | Integrated BM25 inverted index for fast keyword retrieval | `engine.createFulltextIndex()`, `engine.searchText()` |
| **Vector Search** | HNSW approximate nearest neighbor search (Cosine, Euclidean, Dot) | `engine.createVectorIndex()`, `engine.searchVector()` |
| **Transactions** | Unified ACID commits across relational rows and graph topology | `val tx = engine.beginTransaction()` |

### Ideal For:
- **Offline-First Android Apps**: Local storage for complex interconnected data without needing a separate SQLite + Room + Neo4j stack.
- **On-Device Mobile AI & RAG**: Local conversation history, document chunk search (BM25), and vector similarity search without network calls.
- **Desktop JVM & Kotlin Multiplatform**: High-performance local engine packaged directly into a single JAR or AAR.

---

## 2. Installation & Gradle Setup

### Android (`build.gradle.kts`)
Include JNA and the compiled native `.so` libraries in `jniLibs`:

```kotlin
dependencies {
    implementation("net.java.dev.jna:jna:5.14.0@aar")
}

android {
    sourceSets {
        getByName("main") {
            jniLibs.srcDirs("src/main/jniLibs") // Contains arm64-v8a/libbkndb_ffi.so, x86_64, etc.
        }
    }
}
```

Include the generated `bkndb_ffi.kt` file inside your project package (e.g. `uniffi.bkndb_ffi`).

### Desktop JVM / Gradle
```kotlin
dependencies {
    implementation("net.java.dev.jna:jna:5.14.0")
}
```
Ensure `bkndb_ffi.dll` (Windows), `libbkndb_ffi.so` (Linux), or `libbkndb_ffi.dylib` (macOS) is on `jna.library.path` or system PATH.

---

## 3. Core Concepts

1. **Package**: The UniFFI binding is namespaced under `uniffi.bkndb_ffi`.
2. **Types**: IDs use Kotlin `ULong` (unsigned 64-bit integer).
3. **Auto-Closeable**: `BknDbEngine` and `BknDbTransaction` manage native memory and should be closed via `engine.close()` or `.use { ... }`.

---

## 4. Quick Start

```kotlin
package com.example.app

import uniffi.bkndb_ffi.*
import java.io.File

fun main() {
    val dbFile = File("quickstart.bkndb").absolutePath
    val engine = BknDbEngine.open(dbFile)

    try {
        // 1. Relational SQL
        engine.sql("CREATE TABLE users (id INT PRIMARY KEY AUTOINCREMENT, name TEXT, email TEXT)", emptyList())
        engine.sql(
            "INSERT INTO users (name, email) VALUES ($1, $2)",
            listOf(FfiPropValue.Text("Alice"), FfiPropValue.Text("alice@example.com"))
        )

        val res = engine.sql("SELECT id, name, email FROM users", emptyList())
        for (row in res.rows) {
            println("User: ${row["name"]} <${row["email"]}>")
        }

        // 2. Knowledge Graph
        val aliceId = engine.createNode(FfiNodeInput(
            label = "User",
            properties = mapOf("name" to FfiPropValue.Text("Alice"))
        ))
        val toolId = engine.createNode(FfiNodeInput(
            label = "Tool",
            properties = mapOf("name" to FfiPropValue.Text("Calculator"))
        ))
        engine.createEdge(FfiEdgeInput(
            source = aliceId,
            target = toolId,
            edgeType = "USES",
            properties = mapOf("frequency" to FfiPropValue.Integer(10))
        ))

        val neighbors = engine.neighbors(aliceId, FfiDirection.OUTGOING, "USES")
        println("Alice uses ${neighbors.size} tools.")
    } finally {
        engine.close()
    }
}
```

---

## 5. Opening and Closing a Database

### On-Disk Database
```kotlin
// In Android Activity or ViewModel:
val path = File(context.filesDir, "data.bkndb").absolutePath
val engine = BknDbEngine.open(path)

// Close when finished
engine.close()
```

### In-Memory Database (Ephemeral)
```kotlin
val memDb = BknDbEngine.inMemory()
try {
    // Tests and temporary memory
} finally {
    memDb.close()
}
```

### Checking Close Status
```kotlin
if (engine.isClosed()) {
    println("Engine is closed")
}
```

---

## 6. Values & Types (`FfiPropValue`)

Kotlin uses the `FfiPropValue` sealed class:

```kotlin
val textVal   = FfiPropValue.Text("Hermes")
val intVal    = FfiPropValue.Integer(100L)
val realVal   = FfiPropValue.Real(98.6)
val boolVal   = FfiPropValue.Boolean(true)
val blobVal   = FfiPropValue.Blob(byteArrayOf(0x01, 0x02))
val nullVal   = FfiPropValue.Null
val listVal   = FfiPropValue.List(listOf(FfiPropValue.Text("item1")))
val mapVal    = FfiPropValue.Map(mapOf("key" to FfiPropValue.Text("value")))
```

---

## 7. Relational Tables & SQL Engine

### Executing SQL with Parameters
```kotlin
// Create table
engine.sql("CREATE TABLE chat (id INT PRIMARY KEY AUTOINCREMENT, role TEXT, message TEXT)", emptyList())

// Insert with parameter binding ($1, $2)
engine.sql(
    "INSERT INTO chat (role, message) VALUES ($1, $2)",
    listOf(FfiPropValue.Text("user"), FfiPropValue.Text("Explain quantum computing"))
)

// Query rows
val result = engine.sql("SELECT id, role, message FROM chat WHERE role = $1", listOf(FfiPropValue.Text("user")))
for (row in result.rows) {
    val msg = (row["message"] as? FfiPropValue.Text)?.v
    println("Message: $msg")
}
```

---

## 8. Graph: Nodes, Edges, and Traversal

### Creating Nodes
```kotlin
val agentNode = engine.createNode(FfiNodeInput(
    label = "Agent",
    properties = mapOf(
        "name" to FfiPropValue.Text("Hermes-Android"),
        "version" to FfiPropValue.Real(1.5)
    )
))

val skillNode = engine.createNode(FfiNodeInput(
    label = "Skill",
    properties = mapOf("name" to FfiPropValue.Text("CameraVision"))
))
```

### Updating & Deleting Nodes
```kotlin
// Get node
val record = engine.getNode(agentNode)
println("Node Label: ${record.label}, Properties: ${record.properties}")

// Update properties
engine.updateNodeProperties(
    nodeId = agentNode,
    setProps = mapOf("status" to FfiPropValue.Text("active")),
    unsetProps = listOf("deprecated_field")
)

// Delete node (cascades to attached edges)
engine.deleteNode(agentNode)
```

### Creating & Managing Edges
```kotlin
val edgeId = engine.createEdge(FfiEdgeInput(
    source = agentNode,
    target = skillNode,
    edgeType = "CAPABLE_OF",
    properties = mapOf("enabled" to FfiPropValue.Boolean(true))
))

// Delete edge
engine.deleteEdge(edgeId)
```

### Directional Traversal
```kotlin
// FfiDirection: OUTGOING, INCOMING, BOTH
val skills = engine.neighbors(agentNode, FfiDirection.OUTGOING, "CAPABLE_OF")
for (neighbor in skills) {
    println("Connected to Node: ${neighbor.nodeId} via Edge: ${neighbor.edgeId}")
}
```

### Graph Algorithms
```kotlin
// Shortest path (BFS)
val path = engine.findShortestPath(startNodeId, targetNodeId, "CONNECTED_TO")
path?.let {
    println("Hops: ${it.distance}, Path: ${it.nodeIds}")
}

// Weighted path (Dijkstra)
val weighted = engine.findWeightedPath(cityA, cityB, weightProp = "distance", edgeType = "ROAD")
weighted?.let {
    println("Total cost: ${it.cost}")
}

// Degree Centrality (Top Hubs)
val hubs = engine.topHubs(5u)
for (hub in hubs) {
    println("Hub ${hub.nodeId} has degree ${hub.degree}")
}
```

---

## 9. Graph Query Language (`MATCH`)

Execute pattern queries across the graph:

```kotlin
val res = engine.graphQuery("MATCH (a:Agent)-[r:CAPABLE_OF]->(s:Skill) RETURN a.name, s.name")
for (row in res.rows) {
    println("Agent: ${row["a.name"]} -> Skill: ${row["s.name"]}")
}
```

---

## 10. Full-Text Search (BM25)

```kotlin
// 1. Create BM25 index on table and column
engine.createFulltextIndex("articles", "body")

// 2. Search
val hits = engine.searchText("articles", "body", "deep learning kotlin", 10u)
for (hit in hits) {
    println("Row ${hit.rowId} with BM25 score ${hit.score}: ${hit.row}")
}
```

---

## 11. Vector Similarity Search (HNSW Index)

Approximate nearest neighbor search on embeddings:

```kotlin
// 1. Create HNSW Index
// Dimensions: 384, Metric: COSINE, M: 16, efConstruction: 64
engine.createVectorIndex(
    table = "embeddings",
    column = "vec",
    dimensions = 384u,
    metric = FfiVectorMetric.COSINE,
    m = 16u,
    efConstruction = 64u
)

// 2. Search nearest vectors
val queryEmbedding = floatArrayOf(0.12f, 0.45f, -0.23f /* ... */)
val results = engine.searchVector(
    table = "embeddings",
    column = "vec",
    query = queryEmbedding.toList(),
    k = 5u,
    efSearch = 32u
)

for (r in results) {
    println("Score: ${r.score}, Data: ${r.row}")
}
```

---

## 12. Transactions (`BknDbTransaction`)

Explicit ACID transactions across graph and relational tables:

```kotlin
val tx = engine.beginTransaction()
try {
    tx.sql("INSERT INTO logs (entry) VALUES ('Tx Step 1')", emptyList())
    tx.createNode(FfiNodeInput(label = "Audit", properties = emptyMap()))

    // Commit atomically
    tx.commit()
} catch (e: Exception) {
    tx.rollback()
    throw e
}
```

---

## 13. Operations: Stats, Backup, Compaction, and Integrity

```kotlin
// Statistics
val stats = engine.stats()
println("Nodes: ${stats.nodeCount}, Edges: ${stats.edgeCount}, Tables: ${stats.tableCount}")

// Hot Backup
engine.backup("/path/to/backup.bkndb")

// Compaction
engine.compact("/path/to/compacted.bkndb")

// Integrity Verification
val report = engine.verifyIntegrity()
if (report.isClean) {
    println("Database integrity is 100% clean")
} else {
    println("Issues found: ${report.issues}")
}
```

---

## 14. Error Handling (`FfiBknErrorException`)

UniFFI maps Rust errors to Kotlin exceptions inheriting from `FfiBknErrorException`:

```kotlin
try {
    val engine = BknDbEngine.open("/invalid/path/db.bkndb")
} catch (e: FfiBknErrorException.DatabaseLocked) {
    println("Database is locked by another process.")
} catch (e: FfiBknErrorException.TableNotFound) {
    println("Table does not exist: ${e.name}")
} catch (e: FfiBknErrorException) {
    println("General BknDb error: ${e.message}")
}
```

---

## 15. Real-World Recipe: Android Local Agent & Knowledge Graph

```kotlin
package com.example.agent

import uniffi.bkndb_ffi.*
import java.io.File

class AgentDatabase(dbDir: File) {
    private val engine: BknDbEngine

    init {
        val path = File(dbDir, "agent_memory.bkndb").absolutePath
        engine = BknDbEngine.open(path)
        setup()
    }

    private fun setup() {
        engine.sql(
            """CREATE TABLE IF NOT EXISTS memories (
                id INT PRIMARY KEY AUTOINCREMENT,
                session TEXT,
                content TEXT
            )""",
            emptyList()
        )
        try {
            engine.createFulltextIndex("memories", "content")
        } catch (_: Exception) {
            // Index already exists
        }
    }

    fun store(session: String, content: String) {
        engine.sql(
            "INSERT INTO memories (session, content) VALUES ($1, $2)",
            listOf(FfiPropValue.Text(session), FfiPropValue.Text(content))
        )
    }

    fun recall(query: String, limit: UInt = 5u): List<String> {
        val hits = engine.searchText("memories", "content", query, limit)
        return hits.mapNotNull { hit ->
            (hit.row["content"] as? FfiPropValue.Text)?.v
        }
    }

    fun close() {
        engine.close()
    }
}
```

---

## 16. API Reference Summary

### `BknDbEngine`
- `companion object.open(path: String): BknDbEngine`
- `companion object.inMemory(): BknDbEngine`
- `close()`
- `isClosed(): Boolean`
- `sql(query: String, params: List<FfiPropValue>): FfiQueryResult`
- `createNode(input: FfiNodeInput): ULong`
- `createNodesBulk(inputs: List<FfiNodeInput>): List<ULong>`
- `getNode(id: ULong): FfiNodeRecord`
- `deleteNode(id: ULong)`
- `updateNodeProperties(nodeId: ULong, setProps: Map<String, FfiPropValue>, unsetProps: List<String>)`
- `createEdge(input: FfiEdgeInput): ULong`
- `createEdgesBulk(inputs: List<FfiEdgeInput>): List<ULong>`
- `getEdge(id: ULong): FfiEdgeRecord`
- `deleteEdge(id: ULong)`
- `neighbors(nodeId: ULong, direction: FfiDirection, edgeType: String?): List<FfiNeighbor>`
- `findShortestPath(from: ULong, to: ULong, edgeType: String?): FfiPathResult?`
- `findWeightedPath(from: ULong, to: ULong, weightProp: String, edgeType: String?): FfiWeightedPath?`
- `topHubs(k: UInt): List<FfiHubRecord>`
- `cascadeDelete(nodeId: ULong): FfiCascadeStats`
- `graphQuery(query: String): FfiQueryResult`
- `createFulltextIndex(table: String, column: String)`
- `searchText(table: String, column: String, query: String, limit: UInt): List<FfiScoredRow>`
- `createVectorIndex(table: String, column: String, dimensions: UInt, metric: FfiVectorMetric, m: UInt, efConstruction: UInt)`
- `searchVector(table: String, column: String, query: List<Float>, k: UInt, efSearch: UInt): List<FfiScoredRow>`
- `beginTransaction(): BknDbTransaction`
- `stats(): FfiDbStats`
- `backup(destPath: String)`
- `compact(destPath: String)`
- `verifyIntegrity(): FfiIntegrityReport`

---

## 17. Limitations and Best Practices

1. **Android Main Thread**: Perform database operations on a background thread (`Dispatchers.IO` in Kotlin Coroutines) to prevent ANR.
2. **App Storage Directories**: Use `context.filesDir` or `context.noBackupFilesDir` for SQLite/BknDb storage.
3. **Closing Handles**: Always invoke `engine.close()` when your application process shuts down or background worker finishes.
