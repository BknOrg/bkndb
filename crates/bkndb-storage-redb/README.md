# bkndb-storage-redb

[![Crates.io](https://img.shields.io/crates/v/bkndb-storage-redb.svg)](https://crates.io/crates/bkndb-storage-redb)
[![Documentation](https://docs.rs/bkndb-storage-redb/badge.svg)](https://docs.rs/bkndb-storage-redb)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)

**Optional persistent storage engine backend for BknDb, backed by [`redb`](https://crates.io/crates/redb).**

`bkndb-storage-redb` implements the `StorageEngine` trait using Redb, an embedded key-value database written in pure Rust that uses copy-on-write B-trees with strict ACID transaction guarantees.

While the native `.bkndb` LSM engine (`bkndb-storage-lsm`) is the recommended default for most workloads, `redb` is available as a battle-tested alternative backend.

---

## Installation & Feature Flag

Enable the `redb-backend` feature flag in `bkndb`:

```toml
[dependencies]
bkndb = { version = "0.2", features = ["redb-backend"] }
```

---

## Usage

### Via Top-Level Facade (`BknDb::open_redb`)

```rust
use bkndb::BknDb;

// Opens or creates a persistent Redb-backed database file
let db = BknDb::open_redb("redb_store.db")?;

// Perform full graph and relational transactions
db.write_tx(|b| {
    let mut g = b.graph();
    g.create_node("Service", Default::default())?;
    Ok(())
})?;
```

### Standalone Usage

```rust
use bkndb_storage_redb::RedbStorageBackend;
use bkndb_core::Db;

let backend = RedbStorageBackend::open("redb_store.db")?;
let db = Db::new(backend);
```

---

## License

Licensed under the Apache License, Version 2.0 (see [LICENSE](../../LICENSE)).
