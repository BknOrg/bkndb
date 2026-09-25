# bkndb-storage-redb

[![Crates.io](https://img.shields.io/crates/v/bkndb-storage-redb.svg)](https://crates.io/crates/bkndb-storage-redb)
[![Documentation](https://docs.rs/bkndb-storage-redb/badge.svg)](https://docs.rs/bkndb-storage-redb)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)

Optional storage engine backend for the `bkndb` embedded database, backed by [`redb`](https://crates.io/crates/redb).

This crate implements the `StorageEngine` trait defined in `bkndb-core` using Redb as an underlying key-value store with ACID transactions.

Most users should use the top-level [`bkndb`](https://crates.io/crates/bkndb) crate with the optional `redb-backend` feature:

```toml
[dependencies]
bkndb = { version = "0.1", features = ["redb-backend"] }
```

## License

Licensed under the Apache License, Version 2.0.
