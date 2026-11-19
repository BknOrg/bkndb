# LSM audit and acceptance status

Status: **implementation under validation; not a stable-candidate declaration**.
Review date: 2026-11-17. Redb remains the default. Nothing in this change publishes
a package, changes a tag or rewrites Git history. Current source version remains
0.3.1; these working changes are not evidence of a released version.

## Confirmed findings, highest impact first

| Finding | Impact and evidence | Resolution / regression |
| --- | --- | --- |
| C1: nonempty short input was initialized as an empty database | `open` accepted a file shorter than the 128-byte header and overwrote it. Regression failed against the original implementation. | Only an exactly empty file initializes. Foreign/truncated nonempty inputs fail without changing data bytes. `nonempty_short_file_is_rejected_without_modification`. |
| C2: complete corrupt WAL frames were silently treated as an interrupted tail | A flipped byte in an acknowledged transaction could disappear on open; the damaged suffix was truncated. Regression failed against the original implementation. | Complete bad headers/checksums fail normal open. V3 checks header length fields as well as payload CRC. Explicit `repair_to` salvages into a new file and reports discarded bytes. `corruption_in_a_committed_wal_is_not_silently_discarded`. |
| C3: maintenance failure reported a durable commit as failed | A failure opening the compaction temporary happened after WAL sync and memtable publication, but `commit` returned error. Regression failed against the original implementation. Blind retries could duplicate effects. | Separate durable outcome from post-commit maintenance; record maintenance cause and require reopen on unsafe state. `maintenance_failure_does_not_report_durable_commit_as_aborted`. |
| C4: graph ID counter overflow | `current + count` could wrap in release builds and reuse IDs, or panic in checked builds, when the counter was exhausted. | Checked addition returns `ResourceLimit`, leaving transaction uncommitted. `graph_id_exhaustion_is_an_error_instead_of_wrapping`. |

The first three findings were reproduced before extraction/fixes. C4 follows from
the unchecked arithmetic and has an explicit exhausted-counter regression. Tests
added after a fix are not presented as proof that a historical failure was run.

A further recovery-copy regression was reproduced before its fix during this
audit: opening a live file with damaged SSTable data removed a stale compaction
file before validating the source. Cleanup now verifies all source blocks first;
invalid/empty sources preserve the candidate for explicit inspection. The new
repair API also refuses to initialize an empty source, preserving source bytes.

## Additional hardening and design changes

These are separately identified from the reproduced historical bugs:

- Stable lock sidecar across replacement, canonical symlink paths and refusal of
  hard-linked database files. A second writer is rejected while the first lives,
  including after compaction; kernel locks release after process kill.
- Positioned I/O replaces shared OS cursor reads and mandatory mmap. Each cloned
  wrapper has its own logical cursor. Block reads validate offsets and lengths.
  This removes a shared-cursor concurrency risk rather than claiming a reproduced
  historical data race on every OS.
- A synced WAL maintenance fence separates acknowledged records from incomplete
  SSTable/manifest output. Both root slots mirror a completed checkpoint before
  later commits proceed. Regression damages either root after a later WAL commit
  and verifies that the later commit remains visible.
- After a WAL append/sync error, outcome can be unknown and further use requires
  reopen. A rename/directory-sync/flush failure also disables new operations.
  Poisoned internal state is not silently adopted as healthy.
- Decoder checks cover checked ranges, bounded metadata/frame/block allocations,
  full SSTable index shape, ascending generations/block bounds, entry counts,
  Bloom shape and frozen cross-width hashing. Generic bincode reads use explicit
  fixed-width encoding, limits and rejection of trailing bytes.
- Bloom lengths are checked against their fixed-width representation before
  copying bits. One fuzzer run labelled a malformed Bloom input as slow; a
  separate replay took 0 ms. **That slowdown is unconfirmed**, not a confirmed
  decoder bug. The input is retained as a malformed-length regression.
- Replay streams records into the memtable instead of retaining the entire WAL
  history. Both WAL bytes and memtable growth trigger checkpoints; repeated
  overwrites cannot bypass the WAL threshold. Pending transaction accounting,
  flush thresholds and recovery allocations have explicit limits. A projected
  transaction must fit the recovery bound before acknowledgement; replay removes
  replaced entries first so an atomic batch does not exceed it transiently.
- Handle registries reject exhaustion without wrapping/reusing IDs. Go
  transactions retain their DB until completion; cancellation and constructor
  failures release runtime resources. In-memory stats and default vector metric
  behavior are checked against both drivers. Shared JSON result decoding also
  preserves large integers as `json.Number` instead of rounding through float64.
  Shared argument checks reject raw NUL/invalid UTF-8 and overflowing search/index
  parameters before native C-string or uint32 conversion, avoiding truncated input.
- Writer metadata limits match reader limits, so oversized indexes fail before
  publication instead of producing a checkpoint that cannot reopen.
- Fresh maintenance files use exclusive creation; backup/migration publication
  refuses existing destinations atomically. Partial failures cannot delete an
  unrelated preexisting temporary file.
- Legacy v1/v2 containers are read-only migration sources; no silent format
  upgrade. Explicit Redb↔LSM snapshot conversion and LSM repair/migration publish
  verified new destination files.

## Durability state machine reviewed

1. A writer owns one permit, takes a snapshot and buffers updates privately.
2. A checked whole-transaction WAL frame is appended and synced. Errors once
   writing starts are `CommitUnknown`; no automatic retry is claimed safe.
3. The batch becomes visible through one state write lock. Existing read snapshots
   keep their old immutable Arc references.
4. Flush first syncs a maintenance fence, then writes SSTable and manifest, syncs
   body data, writes/syncs each root slot, then publishes the new SSTable set.
5. Full compaction writes and verifies a fresh complete container, atomically
   replaces the data path, persists the directory/replacement barrier and only
   then publishes new handles/state. Old snapshots retain old file handles.
6. Any failure after state/durability could diverge requires reopen. Reopen
   validates metadata/SSTable ranges before truncating an interrupted suffix.

Assumptions and limits are in [the engine contract](LSM_ENGINE.md). CRC is not
cryptographic authentication. No engine without an external witness can detect
removal of an entire committed tail that leaves an otherwise valid shorter
history. Legacy v1 blocks have no checksums and are reported as unchecked.

## Validation performed locally

The execution platform is Linux amd64. Rust stable 1.99.1 built native and WASM;
nightly 1.102.0 (2026-11-16) built the decoder sanitizer harness. Go 1.25.0
executed both native CGO and CGO-free WASM suites. These facts do not
stand in for macOS/Windows runtime results.

- Existing workspace release tests, LSM/core conformance (KV, relational, SQL,
  graph, transactions, hybrid queries, full-text and vector search).
- New durability, malformed input, hot-key WAL, header redundancy, graph overflow,
  explicit repair and migration tests.
- Deterministic reference-map model: one million operations, put/delete/scan,
  commit/rollback, reopen, snapshot during compaction and backup.
- Fault I/O model: errors before/after mutations, short writes and storage-full
  errors; explicit simulated loss of unsynced file bytes and directory updates.
  Every tested acknowledged transaction survives and multi-table changes remain
  atomic. This is a model of persistence, not proof about every device controller.
- Subprocess tests: actual process kill, lock release, uncommitted rollback, and
  kill before/after every instrumented write/sync/truncate/replace/directory
  operation in a flush/compaction path. These test process death separately from
  the simulated power-loss model.
- Native→WASM→native file interchange, including graph IDs above 2^53 and a
  persisted full-text index; a native Rust process also holds the lock while a
  WASM opener is refused. Redb↔LSM migration preserves SQL/graph/search data.
- Go API tests with the explicit WASM tag and CGO disabled, plus a race-detector
  run with the WASM tag. Runtime cancellation, file locks, backup no-clobber,
  old transaction handles and foreign-file rejection have regressions.
- Both consumer modes use a real module ZIP served by a local proxy and a private
  module cache, without `replace` or Rust source. Executables run from a clean
  directory with build tools excluded from PATH. The WASM executable has no
  dynamic section; native imports are checked for absence of a BknDb library.
- Decoder fuzz smoke with saved valid/invalid corpus and fixed seed 21892722;
  ABI fuzz smoke. A final decoder run completed 894,238 executions with address
  sanitizer and no finding. LeakSanitizer cannot run under this environment's
  ptrace restrictions and was disabled for that run; leak checking is not claimed.

Exact commands are in [the guide](LSM_ENGINE.md). Fuzz corpus is in
`fuzz/corpus/decoders`; Go fuzz seeds are in `wazero_test.go`. A bounded fuzz run
only covers the inputs reached during that run.

The Go 1.25 minimum also implies macOS 12+ for applications, independently of
the Rust archive deployment target 11.0. This compatibility constraint is
documented using the [official release notes](https://go.dev/doc/go1.25#darwin).

## Remaining acceptance gates and risks

| Gate | Current status |
| --- | --- |
| Linux local correctness/model/fault/crash/migration | Passing executed checks; keep reviewing newly added code. |
| Five native OS/architecture runtime jobs | Workflow provided; remote results have not been observed for these working changes. |
| Five Go WASM runtime jobs | Workflow provided. Cross-building a test executable is not executing it. |
| Continuous eight-hour soak | Harness/workflow provided; not run to completion. Requires provisioned self-hosted `bkndb-soak`, because a standard hosted job cannot last eight continuous hours. |
| Extended fuzz/stress history | Smoke runs passed; longer runs, more seeds and coverage review remain necessary. |
| OS/filesystem durability contract | POSIX/Windows adapters supplied; macOS/Windows replacement, old readers and filesystem semantics require actual runtime CI. Network filesystems are outside the verified contract. |
| Performance claims | Reproducible KV smoke tool supplied. No claim that LSM beats Redb; production workload benchmarks and latency/resource budgets remain to be established. |
| Release artifacts | Local host artifacts and workflow supplied. Five fresh native archives and their combined release manifest still require CI preparation/review. |
| Stable promotion | Not approved or performed; no default change. |

The audit focuses on LSM durability, storage decoders and the WASM boundary. It is
not a formal proof, exhaustive review of all SQL/search algorithms, complete
filesystem power-failure certification, or proof of absolute absence of bugs.

## Additional native CLI boundary regressions

CLI implementation review reproduced the following shared-core defects before
fixing them, with regressions exercised through both native storage backends:

- Truncated graph adjacency keys caused an out-of-bounds panic during neighbor
  queries. Decoding now validates exact length and UTF-8 and propagates corruption.
- Edge types larger than the persisted u16 length field were accepted in release
  builds. Single/bulk writes and typed neighbor queries now reject oversized types
  before allocating IDs or constructing truncated prefixes.
- A stored catalog entry naming a nonexistent primary key caused `check` to
  panic. Catalog decoding now validates schema structure before callers use it.
- Truncated numeric/UUID secondary-index keys caused a slice panic. Index decoding
  now rejects missing key suffixes explicitly.

The CLI graph check now validates mandatory adjacency in both directions and
counter bounds. Full dump import rebuilds derived indexes from canonical data,
retaining UNIQUE constraints and vector parameters, before publishing a verified
new file. A valid dump checksum alone does not establish semantic validity.

Release-tool regression tests additionally reject NaN/infinite soak durations and
evidence, mismatched soak executable checksums, stale fingerprints and tampered
candidate binaries. These checks do not authenticate manually supplied evidence
and do not replace review of underlying CI, fuzz and continuous-soak logs.

These are additional correctness checks, not a claim that SQL, search or every
malformed application value has been exhaustively audited. The remaining gates
above still apply to the combined CLI/engine candidate.
