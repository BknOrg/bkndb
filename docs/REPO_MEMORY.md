# BknDb repository memory

## Purpose and architecture

BknDb is an embedded Rust hybrid relational/property-graph database with full-text
and vector search, with Python, Go, Kotlin and Swift bindings. `BknDb::open` and
native Go `Open` default to Redb. Never assume Redb and LSM formats interchange
because both may use `.bkndb`; migrations explicitly create new files.

Working source version is 0.3.1. The LSM/WASM changes are unreleased work;
source version is not evidence that any particular tag/package contains them.
No default promotion, registry publication, tag mutation or history rewrite is
part of this work. If commits are later requested, use BknOrg identity without
AI attribution. The user prefers Indonesian.

The maintainer requested an OS-reinstallation backup on the separate branch
`backup/lsm-wasm-cli-wip-2026-11-19`, keeping `main` untouched. See
`docs/RESUME_WORK.md` to restore and continue; this backup is not a stable release.

## LSM and WASM architecture under validation

- `bkndb-engine`: independent, unpublished internal crate, no `bkndb-core`, SQL,
  graph/search or Redb dependency. Transactional KV, owned writer transactions,
  immutable snapshots, scans, backup, integrity and maintenance; typed errors.
- `bkndb-storage-lsm`: compatibility adapter to core `StorageBackend`.
- `bkndb-wasm`: the same core SQL, graph and search on LSM, with a bounded versioned
  buffer/RPC ABI. No Redb, UniFFI, mmap or native transaction workers in the guest.
- `io::StorageIo` / `StorageFile`: positioned I/O, sync, locking and atomic
  publication/replacement. Native uses buffered blocks; mmap is currently disabled.
  Fault tests model unsynced data and directory updates separately.
- LSM v3 uses checked WAL headers/payloads and synced maintenance fences; both
  root slots describe the current checkpoint before later commits proceed.
  V1/v2 open read-only when validated and migrate to a new v3 file.
- One data file plus a stable `.lock` sidecar; compaction/backup/migration may use
  temporary files. Never remove an active lock sidecar. Symlinks normalize to the
  same database path; hard-linked databases are rejected.
- Durable commit success is separate from post-commit maintenance failure. An
  unsafe engine requires reopen; `last_maintenance_error()` preserves its cause.
  `CommitUnknown` requires application reconciliation, not blind retry.
- Normal recovery rejects complete corrupt WAL frames. Explicit `repair_to`
  preserves source bytes and reports discarded suffix data. It requires valid
  roots/manifest/SSTables; arbitrary corruption is not silently guessed around.
- Explicit `bkndb::migration::{redb_to_lsm,lsm_to_redb}` convert an open source
  snapshot through bounded batches into a verified new destination.

See `docs/LSM_ENGINE.md` for contracts and `docs/LSM_AUDIT.md` for reproduced bugs,
regressions, audit scope, assumptions and remaining gates. Stability is not yet
claimed. Full compaction remains the initial strategy; no faster-than-Redb claim
is supported by the current smoke benchmarks.

## Go drivers and distribution

Both drivers require Go 1.25+. Native CGO supports Linux/macOS amd64/arm64 and
Windows amd64, links the explicit package-local static archive and needs a compiler
C at build time (MinGW-w64 on Windows). Consumers need no Rust or separate BknDb
library; system OS libraries may remain dynamic. Native release baselines remain
glibc 2.28 and macOS deployment target 11.0 for Rust archives. Go 1.25 applications
require macOS 12+, and newer Go/C toolchains can raise application requirements.
Windows CGO with Go 1.25 requires a DWARF-5-capable MinGW compiler.

`CGO_ENABLED=0 go build -tags=bkndb_wasm .` selects wazero (pinned 1.12.0). Use
`OpenLSM` explicitly; `Open` returns an explanatory error with this tag instead of
silently changing its Redb meaning. `InMemory`, DB/Tx, SQL, graph and search remain
available. The module is embedded; guest access is restricted to host-provided
file roles. One instance serializes calls; databases have separate instances.
Typed graph IDs remain uint64. Query/property/search integers outside ±2^53 use
`json.Number`; smaller values retain float64 compatibility. Raw text rejects NUL
and invalid UTF-8, and search/index parameters are checked before uint32 conversion.
Default guest linear memory is 256 MiB, configurable through
`OpenLSMWithWASMConfig`. Trap/cancellation closes the instance and host resources;
an interrupted commit can be unknown. Browser/Alpine/musl/other targets are outside
this validation phase.

Release sequence remains source PR → manual distribution preparation → review/
merge ordinary Git archive/WASM blobs and manifests → manual validated Go tag.
Preparation builds a single portable WASM artifact and five native archives at
one revision; checksum, source fingerprint, toolchain, header, version and ZIP
limits are checked. Existing tags are never retargeted. CI failures before
publication do not consume a version. No Git LFS or Release-only attachments for
files needed by `go get`.

## Build/test commands and verified limits

- `cargo check --workspace --locked`
- `cargo test --workspace --release --locked`
- `cargo test -p bkndb-engine --features fuzzing --release --locked`
- `cargo test -p bkndb --features lsm-backend --release --test migration`
- `cargo test -p bkndb-engine --release --test model million_operations -- --ignored --nocapture`
- `cargo test -p bkndb-engine --release --test model eight_hour_soak -- --ignored --nocapture`
- `rustup target add wasm32-unknown-unknown`
- `python3 scripts/go/wasm.py build` / `verify` / `interchange` / `consumer`
- `python3 scripts/go/package.py build linux_amd64` (choose host platform), then
  `verify` / `consumer`; normal FFI build remains `cargo build --release -p bkndb-ffi`.
- Inside Go module: `CGO_ENABLED=1 go test ./...` and
  `CGO_ENABLED=0 go test -tags=bkndb_wasm ./...`.
- `python3 -m unittest discover -s scripts/go -p 'test_*.py'` and version checks
  in `scripts/publish` (including existing WASM/distribution manifests).
- `cargo +nightly fuzz run decoders -- -max_total_time=60 -timeout=5 -max_len=1048576`

Verified local execution is Linux amd64 (including both drivers on Go 1.25.0),
including one million model operations,
fault/power-loss simulation, subprocess kill at persistence boundaries, native–
WASM file interchange and Redb↔LSM migration. Remote five-platform runtime results
and an eight-hour soak have not been observed for these working changes. Do not
claim a stable candidate before those gates and remaining audit review pass.
The manual soak job needs a provisioned self-hosted `bkndb-soak` runner; a normal
GitHub-hosted job's duration limit cannot meet eight continuous hours.

Treat this memory as an onboarding snapshot; current manifests/source/tests are
authoritative. Keep credentials, private data and transient machine state out.


## Native CLI validation candidate

- Separate unpublished `bkndb-cli` crate, executable `bkndb`, directly uses Rust
  libraries; Redb remains creation default. No server daemon or Go dependency.
- Three phases have local implementations: maintenance/inspection, SQL+graph+
  search+dump/CSV+shell, then diagnostics/application KV/bench/stress/completions.
- Native engine `open_existing(path, read_only)` never initializes missing/empty
  sources. Read-only LSM replays into memory without root/WAL/maintenance writes;
  a stable lock sidecar may be created. Redb inspection locks original and opens
  a private copy, reporting the original recovery marker.
- Source format is detected by signature; administration is offline. New output
  files are validated in private staging directories and published by atomic
  no-replace rename (Linux/macOS) or Windows write-through move, with file/directory
  sync. No overwrite flag. Kill tests cover before/after publication and commit.
- Query mutations require `--write`; scripts default atomic. The shared lexer
  splits scripts and recognizes shell transaction controls; parsing caps nesting,
  token count and NOT operators. Owned shell transaction needs rollback after
  failure; unknown commit/recovery-required closes the session.
- Shared `RelationalDb::scan_rows` streams validated rows from one snapshot for
  checks/CSV, while general query results may still materialize in engine memory.
- `GraphDb::verify_integrity` checks canonical records, both adjacency directions
  and ID-counter bounds in one snapshot. Malformed adjacency returns corruption
  rather than panicking; edge types above 65535 UTF-8 bytes are rejected before
  allocating IDs. Stored relational schemas are validated before using their PK,
  and truncated secondary-index keys return an error instead of slicing panic.
- Dump v1 preserves physical records/counters/indexes and is explicitly tied to
  core encoding 0.3.1. Import reconstructs derived graph, secondary/UNIQUE,
  full-text and vector indexes in private staging before publication, preserving
  schema/constraints and vector parameters rather than trusting raw index bytes.
  CSV uses tagged PropValue JSON cells and existing schema; record/column limits
  are enforced for both export and import.
  Nonfinite float wire values use FloatBits; uint64 graph IDs stay integral.
- CLI KV uses reserved physical table `__bkndb_cli_kv_v1` and isolated namespaces,
  never arbitrary graph/SQL tables. SQL schemas cannot use that reserved name.
- `cargo test -p bkndb-cli --features test-failpoints --release --locked` runs CLI
  integration/process-kill tests. `scripts/cli/pty_smoke.py`, `candidate.py smoke`,
  and tooling unit tests validate terminal and standalone runtime behavior.
- `scripts/cli/candidate.py prepare` tests/builds a production binary (without
  test failpoints), checks imports and runs one million model operations; manifests
  carry checksum, toolchain and source fingerprint. `validate` requires five native
  results plus matching engine/fuzz/review and eight-hour soak evidence; no tags or
  publication. `.github/workflows/cli.yml` prepares candidates, fuzzes inputs, and
  optionally runs continuous soak on self-hosted `bkndb-soak`.
- Candidate fingerprints use consistent POSIX path ordering across OSes and cover
  engine CI plus fuzz harnesses/corpus. Preparation fails if inputs change while
  checks run. Soak evidence rejects nonfinite durations, binds the exact binary
  checksum, and fails if source/binary changes during the run. PTY tests wait for
  a new-line prompt, not an intermediate editor redraw.
- User chose to withhold stable CLI release until ALL three phases and LSM gates
  finish. Remote five-platform execution and eight-hour soak remain unobserved.
  See docs/CLI.md for full contracts and remaining acceptance limits.
- Local CLI verification on 2026-11-17: 255 workspace tests passed, 25 CLI
  candidate tests (including process kills), five tooling tests, PTY/isolated
  runtime smoke and separate one-million-operation CLI stress passed. CLI fuzz
  smoke completed 61992 executions with ASAN and LeakSanitizer disabled locally.
  Windows GNU check is not runtime validation. Native candidate checksums/input
  fingerprints matched; stable status remains withheld pending the remote and
  continuous-soak gates.
