# BknDb for Kotlin / Android — 0.3 usage guide

The Kotlin API is generated from `bkndb-ffi` using UniFFI. This guide follows
[`bkndb_ffi.kt`](../../bindings/uniffi/bkndb_ffi/bkndb_ffi.kt) and the current
Rust FFI implementation. Kotlin and the native library must come from the same
build. Disk databases use Redb; `openWithOptions` accepts legacy LSM options
for compatibility but ignores them. Redb does not automatically convert old
LSM files with the same `.bkndb` extension.

## Build and integrate

Run from the repository root with Rust stable, Android SDK/NDK, and the
appropriate NDK linker configured for the selected Cargo target:

```powershell
cargo build --release -p bkndb-ffi
cargo run --release -p bkndb-ffi --bin uniffi-bindgen -- generate --library target/release/bkndb_ffi.dll --language kotlin --out-dir bindings
.\scripts\build_android.ps1 -Target aarch64-linux-android
```

The host-library command above is for Windows. Use the host `.so` or `.dylib`
when generating bindings on Linux or macOS. The Android script sets linker
alignment to 16KB and copies `libbkndb_ffi.so` into
`bindings/android/jniLibs/arm64-v8a/`. Repeat for each ABI your app supports.
The script produces native libraries; it does **not** assemble an AAR.

Copy the generated `bindings/uniffi/bkndb_ffi/bkndb_ffi.kt` into your app's
Kotlin sources, preserving its `uniffi.bkndb_ffi` package. Put each `.so` in
`app/src/main/jniLibs/<abi>/`. Include JNA's Android AAR in your app dependencies
and resolve the JNA imports used by the generated source. Test the resulting
APK/AAB on your supported Android versions; linker alignment alone does not
validate final packaging. This repo does not provide a Gradle project or a
published Maven coordinate to add directly.

## Quick start: SQL, atomic graph writes, and BM25

Call this function from a background worker or `Dispatchers.IO` in your app.
The path must be in an existing writable directory such as `context.filesDir`.

```kotlin
import uniffi.bkndb_ffi.*

fun runExample(path: String) {
    val engine = BknDbEngine.open(path)
    try {
        engine.sql("CREATE TABLE IF NOT EXISTS memories (id INT PRIMARY KEY AUTOINCREMENT, content TEXT)")
        val tx = engine.beginTransaction()
        try {
            tx.sql(
                "INSERT INTO memories (content) VALUES (?)",
                positional = listOf(FfiPropValue.Str("Local Kotlin agent memory"))
            )
            val agent = tx.createNode("Agent", mapOf("name" to FfiPropValue.Str("Assistant")))
            val skill = tx.createNode("Skill", mapOf("name" to FfiPropValue.Str("Search")))
            tx.createEdge(agent, "USES", skill, emptyMap())
            tx.commit()
        } catch (error: Exception) {
            if (tx.isActive()) tx.rollback()
            throw error
        }

        engine.createFulltextIndex("memories", "content")
        for (hit in engine.searchText("memories", "content", "Kotlin agent", 5u)) {
            val content = (hit.row.values["content"] as? FfiPropValue.Str)?.v1
            println("BM25 score=${hit.score}: $content")
        }
        val result = engine.sql("SELECT id, content FROM memories WHERE id >= ?", listOf(FfiPropValue.Int(1L)))
        // Rows are positional lists, aligned with result.columns.
        println(result.columns)
        result.rows.forEach { println(it) }
    } finally {
        engine.close()
    }
}
```

`createFulltextIndex` returns `false` when the index already exists. Do not
catch and suppress every exception to handle an existing index.

## Values and result types

| Kotlin value | Meaning |
| --- | --- |
| `FfiPropValue.Null` | Null |
| `FfiPropValue.Str("text")` | String; read `.v1` after a type check |
| `FfiPropValue.Int(42L)` | Signed 64-bit integer |
| `FfiPropValue.Float(0.5)` | Double |
| `FfiPropValue.Bool(true)` | Boolean |
| `FfiPropValue.Bytes(byteArrayOf(1, 2))` | Binary data |
| `FfiPropValue.Timestamp(0L)` | UTC microseconds since Unix epoch |
| `FfiPropValue.Uuid(hi = 0uL, lo = 1uL)` | UUID halves |
| `FfiPropValue.List(listOf(FfiPropValue.Int(1L)))` | Dynamic list |
| `FfiPropValue.Map(mapOf("key" to FfiPropValue.Str("value")))` | Dynamic map |

`FfiQueryResult` has `columns`, positional `rows`, and `affected`.
`FfiScoredRow` has `row` (`pk`, `values`) and `score`.
`getNode` and `getEdge` return nullable records. Check them before reading fields.

## Graph operations

These fragments assume an open `engine` and existing `alice`, `bob`, and `root`
node IDs (`ULong`). A single-node write uses separate arguments; input records
such as `FfiNodeInput` and `FfiEdgeInput` are for bulk methods.

```kotlin
engine.updateNodeProperties(alice, mapOf("active" to FfiPropValue.Bool(true)), emptyList())
val neighbors = engine.neighbors(alice, FfiDirection.OUT, "KNOWS")
val path = engine.findShortestPath(alice, bob, FfiDirection.OUT, listOf("KNOWS"))
println("Hops: ${path?.edgeIds?.size}")
val route = engine.findWeightedPath(alice, bob, FfiDirection.OUT, listOf("ROAD"), "km", 1.0)
val hubs = engine.topHubs(5u, FfiDirection.IN, null)
val result = engine.graphQuery(
    "MATCH (a:Person {name: \$name})-[:KNOWS]->(b) RETURN b.name",
    named = mapOf("name" to FfiPropValue.Str("Alice"))
)
// Recursively follows outgoing CONTAINS edges; returns deleted node IDs.
val deletedIds = engine.cascadeDelete(root, "CONTAINS")
```

The Kotlin string escapes the graph parameter as `\$name`.
SQL examples can use `?` with `positional`, or `:name` with `named`.
Directions are `OUT`, `IN`, and `BOTH`. The FFI `topHubs` argument
is named `edgeType`, but currently passes through to the core node-label filter;
use `null` for unfiltered hubs.

## Vector search

Your application supplies embeddings. Store a numeric `LIST` or packed
little-endian float32 `BYTES` column. Dimensions are inferred from the first
indexed vector and must match subsequent vectors.

```kotlin
engine.sql("CREATE TABLE IF NOT EXISTS embeddings (id INT PRIMARY KEY, vec LIST)")
engine.sql(
    "UPSERT INTO embeddings (id, vec) VALUES (?, ?)",
    listOf(FfiPropValue.Int(1L), FfiPropValue.List(listOf(
        FfiPropValue.Float(0.1), FfiPropValue.Float(0.8), FfiPropValue.Float(0.3)
    )))
)
engine.createVectorIndex("embeddings", "vec", FfiVectorMetric.COSINE, m = 16u, efConstruction = 200u)
val hits = engine.searchVector(
    "embeddings", "vec", listOf(0.1f, 0.9f, 0.2f), 5u,
    FfiVectorMetric.COSINE, exact = false, efSearch = 64u
)
hits.forEach { println("PK=${it.row.pk}, score=${it.score}") }
```

Use `exact = true` to bypass HNSW. Cosine/dot scores sort highest first;
Euclidean scores represent distance and sort lowest first. Search methods
accept `filter: List<FfiExprNode>` when filtering is required.

## Operations and errors

```kotlin
val stats = engine.stats()
println("Nodes=${stats.nodes}, edges=${stats.edges}, tables=${stats.tables.size}")
println("File bytes=${stats.storage?.fileBytes}") // null for in-memory databases
engine.backup("/existing/writable/directory/new_backup.bkndb") // destination must not exist
engine.compact() // in place, no destination argument
val report = engine.verifyIntegrity() // throws on corruption
println("Verified entries=${report.entries}")
```

Methods throw `FfiBknException` (the Kotlin name generated from Rust's
`FfiBknError`). A report has counters such as `entries` and `blocksVerified`;
it has no `isClean` flag. Avoid parent-engine writes while an explicit
transaction is open; use the transaction handle, then commit or roll back
before closing the engine. Release handles when the owning component is
finished; do not rely on an Android process-shutdown callback.

For all signatures, consult the generated Kotlin source linked above and
[`crates/bkndb-ffi/src`](../../crates/bkndb-ffi/src). For the public Python API,
see [the Python guide](wiki-py.md); its wrapper defaults differ from raw UniFFI.
