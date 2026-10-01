# bkndb-ffi

[![Crates.io](https://img.shields.io/crates/v/bkndb-ffi.svg)](https://crates.io/crates/bkndb-ffi)
[![Documentation](https://docs.rs/bkndb-ffi/badge.svg)](https://docs.rs/bkndb-ffi)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)

**Multi-platform Foreign Function Interface (FFI) layer for BknDb, powered by [UniFFI](https://mozilla.github.io/uniffi-rs/).**

`bkndb-ffi` exposes the high-performance BknDb engine to Android (Kotlin), iOS (Swift), and Python with zero network overhead, safe type mappings, and automatic serialization.

---

## Supported Targets

- **Android (Kotlin):** Generates Android AAR libraries compiled with NDK. Fully compliant with **Android 15+ 16KB page size** alignment (`-C link-arg=-Wl,-z,max-page-size=16384`).
- **iOS (Swift):** Generates Swift bindings and universal XCFrameworks for iOS physical devices (`arm64`) and simulators (`x86_64` / `arm64-sim`).
- **Python:** Provides the native dynamic library loaded by `bindings/python/bkndb/` via UniFFI ctypes loader.

---

## Artifacts & Generated Files

```
bindings/
├── bkndb_ffi.swift           # Swift native interface
├── bkndb_ffiFFI.h            # C header
├── bkndb_ffiFFI.modulemap    # Clang modulemap
├── uniffi/
│   └── bkndb_ffi/
│       └── bkndb_ffi.kt      # Kotlin native interface
└── python/                   # Python package
```

---

## Building Native Binaries

### Android (PowerShell / Linux)

Run the automated Android build script (requires `ANDROID_NDK_HOME`):

```powershell
./scripts/build_android.ps1
```

Supported ABIs:
- `arm64-v8a` (with 16KB page size alignment)
- `armeabi-v7a`
- `x86_64`
- `x86`

### iOS (Bash / macOS)

Run the automated iOS build script (requires Xcode):

```bash
./scripts/build_ios.sh
```

Generates `BknDb.xcframework` ready for drag-and-drop into Xcode projects or Swift Package Manager (SPM).

### Generating Bindings via `uniffi-bindgen`

```bash
cargo run --bin uniffi-bindgen generate \
    --library target/release/libbkndb_ffi.so \
    --language kotlin \
    --out-dir bindings/uniffi/
```

---

## Kotlin Example (Android)

```kotlin
import uniffi.bkndb_ffi.*

// Open database
val db = BknDbEngine.open("/data/data/com.example.app/files/app.bkndb")

// Create graph node
val props = mapOf("name" to FfiPropValue.Str("Alice"))
val aliceId = db.createNode("Person", props)

// Query neighbors
val friends = db.neighbors(aliceId, FfiDirection.OUT, "KNOWS")
println("Found ${friends.size} friends")
```

---

## Swift Example (iOS)

```swift
import BknDb

// Open database
let db = try BknDbEngine.open(path: documentsUrl.appendingPathComponent("app.bkndb").path)

// Create node
let props: [String: FfiPropValue] = ["name": .str("Alice")]
let aliceId = try db.createNode(label: "Person", properties: props)

// Traverse
let neighbors = try db.neighbors(nodeId: aliceId, direction: .out, edgeType: "KNOWS")
print("Found \(neighbors.count) connections")
```

---

## License

Licensed under the Apache License, Version 2.0 (see [LICENSE](../../LICENSE)).
