# BknDb for Swift / iOS — 0.3 usage guide

This guide follows the generated [`bkndb_ffi.swift`](../../bindings/bkndb_ffi.swift)
and the current `bkndb-ffi` crate. Generate Swift bindings and the native
library from the same checkout. Disk databases use Redb; legacy LSM options
accepted by `openWithOptions` are ignored. Existing LSM `.bkndb` files are
not automatically converted to Redb.

## Build and integrate

This checkout does not include a `Package.swift` or a ready-to-add SPM product.
Integrate the generated source and native library in your own Xcode target or
Swift package. On macOS with Xcode and Rust stable, run from the repo root:

```bash
cargo build --release -p bkndb-ffi
cargo run --release -p bkndb-ffi --bin uniffi-bindgen -- generate --library target/release/libbkndb_ffi.dylib --language swift --out-dir bindings
mkdir -p bindings/swift
cp bindings/bkndb_ffiFFI.h bindings/swift/
cp bindings/bkndb_ffiFFI.modulemap bindings/swift/module.modulemap
bash scripts/build_ios.sh release bindings/ios
```

The script builds device arm64 and simulator arm64/x86_64 libraries, combines
the simulator architectures, and writes `bindings/ios/BknDb.xcframework`.
It expects the C header/module map in `bindings/swift`, prepared above.
Add that XCFramework and `bindings/bkndb_ffi.swift` to your Xcode target.
Ensure the generated `bkndb_ffiFFI` C module is visible to the Swift compiler.
When the generated Swift file is compiled directly into your app, its public
types are in your app module; `import BknDb` is only appropriate if you create
a separate Swift wrapper module named `BknDb`.

For macOS, link the host native library instead of the iOS XCFramework.
Other Apple targets require their own builds; the supplied script covers iOS.

## Quick start: SQL, graph transactions, and BM25

Run database work away from the UI thread. Pass a path inside an existing
writable app directory, for example Application Support.

```swift
import Foundation

func runExample(path: String) throws {
    let engine = try BknDbEngine.open(path: path)
    defer { try? engine.close() }

    _ = try engine.sql(
        query: "CREATE TABLE IF NOT EXISTS memories (id INT PRIMARY KEY AUTOINCREMENT, content TEXT)",
        positional: [], named: nil
    )
    let tx = try engine.beginTransaction()
    do {
        _ = try tx.sql(
            query: "INSERT INTO memories (content) VALUES (?)",
            positional: [.str("Local Swift agent memory")], named: nil
        )
        let agent = try tx.createNode(label: "Agent", properties: ["name": .str("Assistant")])
        let skill = try tx.createNode(label: "Skill", properties: ["name": .str("Search")])
        _ = try tx.createEdge(from: agent, edgeType: "USES", to: skill, properties: [:])
        try tx.commit()
    } catch {
        if tx.isActive() { try? tx.rollback() }
        throw error
    }

    _ = try engine.createFulltextIndex(table: "memories", column: "content")
    let hits = try engine.searchText(
        table: "memories", column: "content", query: "Swift agent", limit: 5,
        matchAll: false, filter: []
    )
    for hit in hits {
        if case let .str(content)? = hit.row.values["content"] {
            print("BM25 score=\(hit.score): \(content)")
        }
    }
    let result = try engine.sql(
        query: "SELECT id, content FROM memories WHERE id >= ?",
        positional: [.int(1)], named: nil
    )
    print(result.columns)
    result.rows.forEach { print($0) } // positional rows, aligned with columns
}
```

`createFulltextIndex` returns `false` when the index exists. Do not suppress
unrelated errors to handle that case. `BknDbEngine.inMemory()` creates an
ephemeral database and is also a throwing constructor.

## Values and results

| Swift value | Meaning |
| --- | --- |
| `.null` | Null |
| `.str("text")` | String |
| `.int(42)` | Int64 |
| `.float(0.5)` | Double |
| `.bool(true)` | Boolean |
| `.bytes(Data([1, 2]))` | Binary data |
| `.timestamp(0)` | UTC microseconds since Unix epoch |
| `.uuid(hi: 0, lo: 1)` | UUID halves |
| `.list([.int(1)])` | Dynamic list |
| `.map(["key": .str("value")])` | Dynamic map |

`FfiQueryResult` has `columns`, positional `rows`, and `affected`.
`FfiScoredRow` has `row` (`pk`, `values`) and `score`.
`getNode(id:)` and `getEdge(id:)` return optional records. Check them before
reading fields. `FfiNodeInput` and `FfiEdgeInput` are for bulk creation;
single-entity methods accept separate arguments as shown above.

## Graph operations

The fragments below assume an open `engine` and existing node IDs (`UInt64`).

```swift
try engine.updateNodeProperties(id: alice, set: ["active": .bool(true)], unset: [])
let neighbors = try engine.neighbors(node: alice, direction: .out, edgeType: "KNOWS")
if let path = try engine.findShortestPath(
    start: alice, target: bob, direction: .out, edgeTypes: ["KNOWS"]
) {
    print("Hops: \(path.edgeIds.count), nodes: \(path.nodeIds)")
}
let route = try engine.findWeightedPath(
    start: alice, target: bob, direction: .out, edgeTypes: ["ROAD"],
    weightProperty: "km", defaultWeight: 1.0
)
let hubs = try engine.topHubs(k: 5, direction: .in, edgeType: nil)
let result = try engine.graphQuery(
    query: "MATCH (a:Person {name: $name})-[:KNOWS]->(b) RETURN b.name",
    positional: [], named: ["name": .str("Alice")]
)
// Follows outgoing CONTAINS edges and returns deleted node IDs.
let deletedIds = try engine.cascadeDelete(root: root, containmentEdge: "CONTAINS")
```

Directions are `.out`, `.in`, and `.both`. SQL accepts `?` with `positional`
or `:name` with `named`. Graph queries accept `$name` with `named`. The current
FFI `topHubs` argument is named `edgeType`; the implementation passes it to
the core's optional node-label filter. Pass `nil` for unfiltered hubs.

## Vector search

Generate embeddings in your application. Store a numeric `LIST` or packed
little-endian float32 `BYTES` column. Index dimensions are inferred from
stored vectors and must remain consistent; there is no `dimensions` argument.

```swift
_ = try engine.sql(
    query: "CREATE TABLE IF NOT EXISTS embeddings (id INT PRIMARY KEY, vec LIST)",
    positional: [], named: nil
)
_ = try engine.sql(
    query: "UPSERT INTO embeddings (id, vec) VALUES (?, ?)",
    positional: [.int(1), .list([.float(0.1), .float(0.8), .float(0.3)])], named: nil
)
_ = try engine.createVectorIndex(
    table: "embeddings", column: "vec", metric: .cosine, m: 16, efConstruction: 200
)
let hits = try engine.searchVector(
    table: "embeddings", column: "vec", vector: [0.1, 0.9, 0.2], limit: 5,
    metric: .cosine, filter: [], exact: false, efSearch: 64
)
for hit in hits { print("PK: \(hit.row.pk), score: \(hit.score)") }
```

Set `exact: true` to bypass HNSW. Cosine/dot scores sort highest first;
Euclidean scores represent distances and sort lowest first.

## Operations and errors

```swift
let stats = try engine.stats()
print("Nodes: \(stats.nodes), edges: \(stats.edges), tables: \(stats.tables.count)")
if let storage = stats.storage { print("File bytes: \(storage.fileBytes)") }
try engine.backup(dest: "/existing/writable/directory/new_backup.bkndb")
try engine.compact() // in place; no destination argument
let report = try engine.verifyIntegrity() // throws if corruption is found
print("Verified entries: \(report.entries)")
```

Backup destinations must not already exist. Storage metrics are optional for
in-memory databases. Integrity reports contain counters, not `isClean` or
`issues`. Catch `FfiBknError` or propagate the error to the caller; see its
actual cases in the generated Swift source. Avoid writes through the parent
engine while an explicit transaction is open; commit or roll back before
closing it. Release handles when the owning component finishes, rather than
relying on an app-termination callback.
