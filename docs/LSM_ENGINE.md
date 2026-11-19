# LSM engine and explicit Go/WASM driver

This implementation is under validation. Redb remains the default. Neither a green
unit-test run nor the new WASM driver is a claim that the engine is bug-free or
ready to replace Redb. See [audit and acceptance status](LSM_AUDIT.md).

## Architecture and standalone use

`bkndb-engine` contains only transactional key-value storage: memtables, WAL,
SSTables, snapshot scans, full compaction, backup, integrity checks and explicit
repair/migration. It does not depend on `bkndb-core`, SQL, graph, search or Redb.
`bkndb-storage-lsm` translates its transactions/errors into the existing
`StorageBackend` contract. `bkndb-wasm` links that adapter with the same
`bkndb-core` SQL/graph/search implementation used by native BknDb.

The new engine crate is internal and unpublished. A separate registry release
is required before documenting a crates.io dependency. It can already be used
independently through a path dependency:

```toml
[dependencies]
bkndb-engine = { path = "../bkndb/crates/bkndb-engine" }
```

```rust
use bkndb_engine::{Engine, StorageBackend, StorageReadTx, StorageWriteTx, TableSpec};
let db = Engine::open("app.bkndb")?;
let table_name = String::from("documents");
let mut tx = db.begin_write_owned()?;
tx.put(TableSpec(&table_name), b"key", b"value")?;
tx.commit()?;
let snapshot = db.begin_read_owned()?;
assert_eq!(snapshot.get(TableSpec(&table_name), b"key")?, Some(b"value".to_vec()));
# Ok::<(), bkndb_engine::Error>(())
```

Owned transactions retain their writer permit and engine reference safely.
Snapshots retain immutable memory/file handles; compaction does not change what
an older snapshot reads. Native writers are serialized; a WASM instance rejects
an additional writer instead of blocking its single execution thread.

## Files, locking and I/O

One database has one data file and a persistent `<database>.lock` sidecar. Do not
unlink, replace or copy the lock file while a database is open. Compaction uses
`<database>.compact.tmp`. Backup/migration use destination-side temporary files;
no automatic operation overwrites an existing backup or migration destination.
Operational files are not independent database formats.

The lock remains attached to the sidecar during data-file replacement. Native
paths resolve symlinks before locking; hard-linked data files are rejected.
The contract assumes cooperating processes on a local filesystem implementing
exclusive locking, durable file sync, atomic same-directory replacement and
appropriate directory persistence. Network filesystems, adversarial path changes
and devices that lie about sync are outside the verified contract.

`io::StorageIo` and `StorageFile` define positioned reads/writes, short-write
retries, length, truncation, sync, exclusive locks, no-clobber publication and
replacement/directory barriers. Required capabilities are checked at open.
Native reads use bounded blocks without mmap; duplicated handles have independent
logical cursors. There is no unsafe mapped-file lifetime to maintain. Mmap is
not currently enabled as an optimization.

POSIX uses file sync plus directory fsync, including the parent entries of
directories newly created by the adapter. Existing ancestor directories must
already satisfy the caller/filesystem durability contract. Windows uses share-delete handles,
exclusive file locks and `MoveFileExW` with write-through for replacement and
publication. Windows durability/replacement behavior still requires its CI and
filesystem validation; POSIX guarantees must not be inferred from a Linux run.

The WASM guest imports only `bkndb_host_v1.io_call`. It can address fixed roles
for the database, lock, compaction temporary and explicitly selected backup. Host
paths never enter the guest. No WASI/general filesystem, Redb, UniFFI, mmap or
native transaction worker thread is linked into this module.

## Commit and recovery

A nonempty transaction appends one complete WAL frame and syncs it before
publishing the new memtable. Cross-table updates are one atomic batch. Dropping
an uncommitted transaction discards its pending map. Errors before writing WAL
leave it uncommitted. `CommitUnknown` means writing/syncing may have taken effect:
reopen and reconcile using application operation IDs; do not blindly retry.

Flush/compaction occur after durability. Their failure does not turn a durable
commit into a reported rollback: commit returns success, the engine records
`last_maintenance_error()`, and subsequent operations require reopen. Explicit
maintenance returns its own error. Poisoned internal locks also require reopen.
Already-created immutable snapshots remain independent.

A stale compaction candidate is removed only after the live source blocks validate.
An empty live file with such a candidate requires explicit inspection; repair never
initializes an empty source. Normal recovery rejects complete bad WAL checksums/headers and malformed
metadata. It only discards an incomplete final append or output behind a valid
maintenance fence, after referenced SSTables have been validated. This is a crash
recovery contract, not detection of every possible arbitrary truncation: removing
an entire committed tail without leaving evidence is inherently indistinguishable
from a shorter history without an external witness. CRC32 detects accidental
corruption; it is not authentication against deliberate modification.

`Engine::repair_to(source, destination)` explicitly salvages a valid WAL prefix
into a new verified file, reports recovered records and discarded suffix bytes,
and preserves source data bytes. It requires valid roots, manifest and SSTables;
unrecoverable metadata/block corruption is reported, not guessed around. Keep the
original and report before inspecting the destination. `verify_integrity()` scans
blocks, ordering, Bloom consistency, entry counts, manifest and WAL.

## Format v3 and migration

A `.bkndb` suffix does not identify a backend. Redb and LSM files are incompatible.
LSM v1/v2 can be opened read-only when their structures validate. Writes and
compaction require explicit migration with `migrate_to(new_path)` / `backup_to`.
The source is never upgraded in place.

The header has two 64-byte slots (`BKNDBLS1`, version, sequence, manifest offset
and length, manifest CRC, slot CRC; integer fields big-endian). Both slots refer
to the latest checkpoint before further WAL commits are permitted. Manifest and
record bincode encodings remain fixed-width little-endian, with bounded decoding.
V3 WAL frames are `BKW3 | payload_length:u32 | payload_crc:u32 | header_crc:u32 |
payload`; numeric header fields are big-endian. An empty record is a synced
maintenance fence, never a user commit. SSTable v2 remains block-checksummed,
optionally LZ4 compressed, with checksummed Bloom/index metadata. V1 unchecked
blocks are explicitly identified by integrity reports.

Table prefixes use a one-byte UTF-8 byte length (maximum 255); key/value lengths
are bounded, offsets use checked arithmetic, Bloom hashing is frozen to the
original 64-bit little-endian SipHash encoding across native and wasm32. V3 file
interchange is tested in both directions, including graph IDs greater than 2^53.
Record IDs and handles remain uint64. Tabular core integer values are signed
64-bit; the Go SDK preserves integers outside ±2^53 as `json.Number`, including
nested properties/search results. Smaller numeric values keep their existing
float64 representation.

Limits: WAL transaction/frame and SSTable block/metadata at most 64 MiB; flush
threshold 1..64 MiB; recovery's accounted memtable at most 256 MiB. Compaction
trigger is at least two. WAL growth also triggers flushing, including repeated
overwrites of the same key. Streaming replay avoids retaining every historical
record in memory. These are safety limits, not exact process-RSS guarantees.

With both facade features enabled, `bkndb::migration::redb_to_lsm(&open_redb,
new_path)` and `lsm_to_redb(&open_lsm, new_path)` stream a consistent raw snapshot,
including internal tables, through bounded batches into a temporary destination,
verify it, then publish without replacing an existing destination. Opening the
source remains the caller's responsibility (including Redb's own recovery).

## Go usage

Both drivers require Go 1.25+. Native CGO remains the default and needs a compiler
C at build time (MinGW-w64 on Windows). Compiler C compiles the Go bridge and links
the already-built Rust archive; consumers do not compile Rust or C separately.
The executable needs no separate BknDb library; OS system libraries may remain
dynamic. Rust archive baselines remain glibc 2.28 and macOS deployment target 11.0.
[Go 1.25 applications require macOS 12+](https://go.dev/doc/go1.25#darwin); newer Go
toolchains may impose higher OS requirements. Windows CGO needs a DWARF-5-capable
MinGW compiler with Go 1.25.

```go
// Native CGO: Open defaults to Redb; OpenLSM explicitly chooses LSM.
db, err := bkndb.OpenLSM("app.bkndb")
```

```bash
# Entire BknDb inside wazero, including SQL/graph/search. No compiler C required.
CGO_ENABLED=0 go build -tags=bkndb_wasm .
```

With this tag, `Open(path)` returns an explanatory error: it cannot open Redb.
Use `OpenLSM(path)` explicitly. `InMemory`, `DB`, `Tx`, SQL, graph and search keep
the same API. WASM InMemory uses ephemeral host memory and never creates files.
The module is embedded, so users need no separate .wasm file or Rust installation.
The supported target set is Linux/macOS amd64/arm64 and Windows amd64; browser,
Go/wasm hosts, Alpine/musl and other targets are outside this validation phase.

```go
ctx, cancel := context.WithCancel(context.Background())
defer cancel()
db, err := bkndb.OpenLSMWithWASMConfig("app.bkndb", bkndb.WASMConfig{
    Context: ctx,
    MemoryLimitMiB: 256, // default; configurable from 16 through 4096
})
```

Calls to each runtime are serialized; separate databases have separate instances.
The guest's linear-memory limit defaults to 256 MiB. Each ABI request/response is
at most 16 MiB. Host ephemeral storage also has a configured bound. These limits
exclude Go/runtime compiled-code overhead. A trap or cancellation invalidates the
instance and closes host file handles/locks. An interrupted commit can have an
unknown outcome; reopen and reconcile. An idle cancelled context also releases
resources. Untrusted/reused transaction handles cannot affect a new transaction.

## Maintainer builds and acceptance

```bash
rustup target add wasm32-unknown-unknown
python3 scripts/go/wasm.py build
python3 scripts/go/wasm.py verify
python3 scripts/go/wasm.py interchange
python3 scripts/go/wasm.py consumer
python3 scripts/go/package.py build linux_amd64
CGO_ENABLED=1 go test ./... # inside bindings/go/bkndb
CGO_ENABLED=0 go test -tags=bkndb_wasm ./... # same directory
cargo check --workspace --locked
cargo test --workspace --locked --release
cargo test -p bkndb-engine --features fuzzing --release
cargo test -p bkndb --features lsm-backend --release --test migration
cargo test -p bkndb-engine --release --test model million_operations -- --ignored --nocapture
cargo test -p bkndb-engine --release --test model eight_hour_soak -- --ignored --nocapture
cargo +nightly fuzz run decoders -- -max_total_time=3600 -max_len=1048576
```

Ordinary CI runs correctness/model/fault/process-kill tests across five native
platforms, five Go WASM runtime tests, decoder fuzzing and ABI fuzzing. Preparation
packages a WASM module plus all five native archives from the same source
fingerprint. The distribution PR must include these ordinary Git blobs, header
and manifests; downloads attached to a GitHub Release are insufficient for Go.
No Git LFS. Source fixes invalidate artifacts but need no version change until
publication; existing public tags are never moved or overwritten.

The eight-hour continuous soak uses an explicitly provisioned self-hosted runner
labelled `bkndb-soak` through the manual LSM workflow. Standard [GitHub-hosted job limits](https://docs.github.com/en/actions/reference/limits)
allow six hours and cannot satisfy eight continuous hours. No such runner or passing soak is
assumed. Stability promotion requires observed passing results for all gates,
review of remaining audit risks and a separate release/default-selection decision.

A repeatable compression-friendly KV smoke benchmark is provided:

```bash
cargo build --release -p bkndb --features lsm-backend --example storage_bench
/usr/bin/time -v target/release/examples/storage_bench lsm /tmp/new-lsm.bkndb 100000
/usr/bin/time -v target/release/examples/storage_bench redb /tmp/new-redb.bkndb 100000
```

It reports throughput, batch commit/read p50/p95/p99, file growth, final compaction
latency and completed compaction temporary/current-file byte peaks; `/usr/bin/time`
reports peak RSS. Record machine, filesystem, toolchain, batch size and payload.
The disk peaks exclude old files retained by snapshots and unfinished failed
maintenance. This workload is not sufficient to claim that LSM generally outperforms Redb.
