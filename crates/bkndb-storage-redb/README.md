# bkndb-storage-redb

[![Crates.io](https://img.shields.io/crates/v/bkndb-storage-redb.svg)](https://crates.io/crates/bkndb-storage-redb)
[![Documentation](https://docs.rs/bkndb-storage-redb/badge.svg)](https://docs.rs/bkndb-storage-redb)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)

**Default persistent storage engine backend for BknDb, backed by [`redb`](https://crates.io/crates/redb).**

`bkndb-storage-redb` implements the storage traits for BknDb using Redb, an embedded key-value database written in pure Rust that uses copy-on-write B-trees with strict ACID transaction guarantees.

It serves as the default on-disk storage engine for `bkndb`, providing robust single-file persistence (`.bkndb`), table scans, MVCC read transactions, and reliable crash recovery.

---

## Installation & Feature Flag

`redb-backend` is enabled by default in `bkndb`:

```toml
[dependencies]
bkndb = "0.2"
```

---

## Usage

### Via Top-Level Facade (`BknDb::open`)

```rust
use bkndb::BknDb;

// Opens or creates a persistent Redb-backed database file (default backend)
let db = BknDb::open("redb_store.bkndb")?;

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
