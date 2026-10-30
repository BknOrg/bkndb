# bkndb-ffi

[![Crates.io](https://img.shields.io/crates/v/bkndb-ffi.svg)](https://crates.io/crates/bkndb-ffi)
[![Documentation](https://docs.rs/bkndb-ffi/badge.svg)](https://docs.rs/bkndb-ffi)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)

**Multi-platform Foreign Function Interface (FFI) layer for BknDb, powered by [UniFFI](https://mozilla.github.io/uniffi-rs/).**

`bkndb-ffi` exposes the high-performance BknDb engine to Android (Kotlin), iOS (Swift), and Python with zero network overhead, safe type mappings, and automatic serialization.

---

## Supported Targets

- **Android (Kotlin):** Generates Kotlin source and native `.so` libraries compiled with the NDK. The Android script sets 16KB linker alignment (`-C link-arg=-Wl,-z,max-page-size=16384`); packaging an AAR and validating the final APK/AAB are application build steps.
- **iOS (Swift):** Generates Swift bindings and universal XCFrameworks for iOS physical devices (`arm64`) and simulators (`x86_64` / `arm64-sim`).
- **Python:** Provides the native dynamic library loaded by `bindings/python/bkndb/` via UniFFI ctypes loader.
- **Go:** Provides zero-dependency native Go bindings in `bindings/go/bkndb/` via C-ABI (`bkndb.h`) and Windows syscall DLL loader (no GCC required on Windows).

---

## Artifacts & Generated Files

```
bindings/
├── bkndb.h                   # Public C-ABI header (Go, C, C++, Zig)
├── bkndb_ffi.swift           # Swift native interface
├── bkndb_ffiFFI.h            # UniFFI C header
├── bkndb_ffiFFI.modulemap    # Clang modulemap
├── go/                       # Go package (bkndb)
│   └── bkndb/
├── uniffi/
│   └── bkndb_ffi/
│       └── bkndb_ffi.kt      # Kotlin native interface
└── python/                   # Python package
```

---

## Building Native Binaries

### Android (PowerShell / Linux)

Run from the repository root with the Android NDK installed and Cargo's linker configured for the target. Setting `ANDROID_NDK_HOME` alone does not configure that linker:

```powershell
./scripts/build_android.ps1
```

Supported ABIs:
- `arm64-v8a` (with 16KB page size alignment)
- `armeabi-v7a`
- `x86_64`
- `x86`

### iOS (Bash / macOS)

On macOS with Xcode, generate the Swift bindings and prepare the headers expected by the packaging script, then build from the repository root:

```bash
cargo build --release -p bkndb-ffi
cargo run --release -p bkndb-ffi --bin uniffi-bindgen -- generate --library target/release/libbkndb_ffi.dylib --language swift --out-dir bindings
mkdir -p bindings/swift
cp bindings/bkndb_ffiFFI.h bindings/swift/
cp bindings/bkndb_ffiFFI.modulemap bindings/swift/module.modulemap
bash scripts/build_ios.sh release bindings/ios
```

This writes `bindings/ios/BknDb.xcframework`. Add it together with the generated Swift source to your Xcode target, or create your own Swift package wrapper. This checkout does not provide a `Package.swift` or an SPM product.

### Generating Bindings via `uniffi-bindgen`

```bash
cargo build --release -p bkndb-ffi
cargo run --release -p bkndb-ffi --bin uniffi-bindgen -- generate \
    --library target/release/libbkndb_ffi.so \
    --language kotlin \
    --out-dir bindings
```

---

## Kotlin Example (Android)

The examples use the 0.3 FFI API. Generate bindings and native libraries from the same checkout. Disk databases use Redb; legacy LSM options passed to `openWithOptions` are ignored. Redb does not automatically migrate LSM `.bkndb` files.

```kotlin
import uniffi.bkndb_ffi.*

// Open database
val db = BknDbEngine.open("/data/data/com.example.app/files/app.bkndb")
try {

// Create graph node
val props = mapOf("name" to FfiPropValue.Str("Alice"))
val aliceId = db.createNode("Person", props)

// Query neighbors
val friends = db.neighbors(aliceId, FfiDirection.OUT, "KNOWS")
println("Found ${friends.size} friends")
} finally {
    db.close()
}
```

---

## Swift Example (iOS)

```swift
import Foundation

// Compile the generated Swift file in your app target, or import your own wrapper module.

// Open database
let db = try BknDbEngine.open(path: documentsUrl.appendingPathComponent("app.bkndb").path)
defer { try? db.close() }

// Create node
let props: [String: FfiPropValue] = ["name": .str("Alice")]
let aliceId = try db.createNode(label: "Person", properties: props)

// Traverse
let neighbors = try db.neighbors(node: aliceId, direction: .out, edgeType: "KNOWS")
print("Found \(neighbors.count) connections")
```

## Documentation & Language Guides

Comprehensive guides and complete API references are available in `docs/wikis/`:
- **Go**: [wiki-go.md](../../docs/wikis/wiki-go.md)
- **Swift (iOS/macOS)**: [wiki-swift.md](../../docs/wikis/wiki-swift.md)
- **Kotlin (Android/JVM)**: [wiki-kotlin.md](../../docs/wikis/wiki-kotlin.md)
- **Python**: [wiki-py.md](../../docs/wikis/wiki-py.md)
- **Rust**: [wiki-rust.md](../../docs/wikis/wiki-rust.md)

---

## License

Licensed under the Apache License, Version 2.0 (see [LICENSE](../../LICENSE)).
