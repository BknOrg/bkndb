# bkndb-core

[![Crates.io](https://img.shields.io/crates/v/bkndb-core.svg)](https://crates.io/crates/bkndb-core)
[![Documentation](https://docs.rs/bkndb-core/badge.svg)](https://docs.rs/bkndb-core)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)

Core primitives, hybrid transactional interface, graph engine abstractions, and relational schema layer for the `bkndb` database engine.

This crate provides the core foundational types (`StorageEngine`, `HybridTx`, `GraphEngine`, `RelSchema`, `PropValue`, `NodeId`, `EdgeId`) used by `bkndb` and its storage backends (`bkndb-storage-lsm`, `bkndb-storage-mem`, `bkndb-storage-redb`).

Most users should depend directly on [`bkndb`](https://crates.io/crates/bkndb) instead of this crate.

## License

Licensed under the Apache License, Version 2.0.
