# bkndb-storage-lsm

Adapter from the independent `bkndb-engine` crate to `bkndb-core::StorageBackend`.
Existing `LsmStorageBackend`, `LsmOptions`, transaction and maintenance entry points
remain available. Redb remains the default BknDb backend.

See [LSM format, durability, migration and Go/WASM usage](../../docs/LSM_ENGINE.md)
and [audit/acceptance status](../../docs/LSM_AUDIT.md). A database uses one data file,
a stable `.lock` sidecar and temporary maintenance files. Legacy formats are
read-only migration sources; no in-place format upgrade is performed.
