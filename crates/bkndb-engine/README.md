# bkndb-engine

Independent transactional LSM key-value engine. No SQL, graph, search, Redb or
`bkndb-core` dependency. Owned transactions, immutable snapshots, streaming scans,
full compaction, integrity checks and explicit backup/migration/repair share a
positioned I/O contract on native and capability-based hosts.

This new internal crate is unpublished and under validation. Redb remains BknDb's
default. See [engine contracts, format and usage](../../docs/LSM_ENGINE.md) and
[audit status](../../docs/LSM_AUDIT.md). Native databases use one data file plus a
stable `.lock` sidecar and temporary maintenance files. LSM v1/v2 require migration
to a new v3 file for writing; they are never silently upgraded in place.

```rust
use bkndb_engine::{Engine, StorageBackend, StorageWriteTx, TableSpec};
let db = Engine::open("app.bkndb")?;
let mut tx = db.begin_write_owned()?;
tx.put(TableSpec("items"), b"key", b"value")?;
tx.commit()?;
# Ok::<(), bkndb_engine::Error>(())
```

`CommitUnknown` requires reconciliation after reopening. A failed post-commit
maintenance operation preserves the committed outcome and requires reopen;
`last_maintenance_error()` retains its cause. The `test-io`/`fuzzing` features are
for test adapters and bounded parser harnesses, not production storage.
