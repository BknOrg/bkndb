# Native BknDb CLI

## Status and architecture

The three implementation phases are present in the local workspace as
`bkndb-cli`, a Rust crate with a `bkndb` binary. This is an **unreleased validation
candidate**, not a stable release. Workspace version 0.3.1 does not imply an
existing published package includes the CLI. CLI and engine registry publication
are disabled for now. Redb remains the default; LSM is selected explicitly.

The binary embeds the Rust engine, SQL, graph and search code. Applications using
the library do not need the CLI. No server process, listener or background daemon
is installed. Rust/build tools are needed to build it, not to execute the result;
OS system libraries can remain dynamic.

Build: `cargo build --release --locked -p bkndb-cli`.
Local install: `cargo install --locked --path crates/bkndb-cli`.
Use the complete repository until dependency crates are published. CI pins Rust
1.99.1; no lower Rust MSRV is claimed by this work.

The candidate matrix covers Linux amd64/arm64 (manylinux glibc 2.28 builds), macOS
amd64/arm64 (deployment target 11.0), and Windows amd64 (GNU/MinGW-w64). These are
CI targets, not assertions that remote runtime validation has passed. Browser,
Alpine/musl and other platforms are outside the initial matrix. Native CLI does
not depend on Go or its separate macOS minimum-version requirement.

## Phase 1: inspection and maintenance

```
bkndb create FILE [--backend redb|lsm]
bkndb info FILE
bkndb stats FILE
bkndb check FILE
bkndb doctor FILE
bkndb backup FILE --output NEW_FILE
bkndb restore BACKUP --output NEW_FILE
bkndb migrate FILE --to redb|lsm --output NEW_FILE
bkndb repair FILE --output NEW_FILE
bkndb compact FILE
bkndb capabilities [FILE]
bkndb version
```

Existing files are detected by signature, not extension. Optional `--backend`
asserts that detection, never overrides it. Missing, empty or foreign sources are
not initialized. Only `create` creates an empty database. Backend conversions and
LSM legacy upgrades create new files; formats are never interpreted interchangeably.

All administration is offline. A compatible exclusive lock is retained until
source use is finished. A busy database fails with exit 4; close the application
and retry. Never remove/replace an active `.lock` file. Symlinks are normalized;
hard-linked database sources are rejected. Filesystem paths and reserved
operational names must not be concurrently manipulated by noncooperating actors.
Network filesystems are not certified by these gates.

LSM inspection uses an existing, read-only file handle, strict WAL decoding and
in-memory replay. It never truncates a tail, rewrites roots or deletes compaction
candidates. It may create the stable lock sidecar. Redb inspection holds a
compatible file lock, copies the source to private temporary storage, and performs
upstream operations on that copy. This needs temporary disk space up to the source
size plus Redb recovery overhead. The upstream recovery marker is reported; a
recovered scratch copy does not establish that the original is clean.

`check` verifies backend checksums/encoding and additionally decodes graph records,
checks graph adjacency in both directions and graph ID counter bounds, validates
stored catalog definitions, streams relational rows through schema validation, and reads
index registries. The report describes its scope. It does **not** establish
exhaustive correctness of every derived search/property index or uniqueness constraint
or arbitrary application KV payload. Legacy unchecksummed structures are identified
by the LSM report. `doctor` gives recommendations and never repairs automatically.

Backup/restore/migration validate a consistent copy before publishing. `repair`
only supports LSM salvage with valid roots/manifest/SSTables and a recoverable WAL
prefix. It preserves source bytes, publishes a verified new destination and reports
what was discarded. Discarded bytes yield exit 10 and structured details. Redb
salvage is unsupported; upstream recovery is not advertised as a salvage command.

Destinations must not exist, including symlinks, and their parent must already be
a directory. Outputs are built inside private staging directories in the destination
filesystem. Publication uses atomic no-replace rename (`renameat2` on Linux,
`renamex_np` on macOS, `MoveFileExW` on Windows), not a hard-link/unlink window.
Unsupported filesystem semantics fail instead of falling back to overwrite.
Data and directory synchronization precede success. If directory sync fails after
publication, exit 7 states that the output already exists; inspect it before retrying.

`compact` intentionally modifies the original. Back up first. Successful durable
commits are not described as rolled back merely because later maintenance failed.
An untrusted engine state or commit-unknown error ends a shell session and requires
reopening/reconciliation. The CLI never automatically retries an uncertain commit.

A killed process can leave `.bkndb-stage-*` directories. Preserve them until the
source/output is checked; no global automatic deletion scans other invocations'
files. Never delete an active database's operational files as cleanup.

## Phase 2: queries, graph and search

```
bkndb sql FILE --query SQL [--params JSON] [--write]
bkndb sql FILE --file SCRIPT [--params JSON] [--write] [--per-statement]
bkndb tables FILE
bkndb schema FILE [TABLE]
bkndb indexes FILE [TABLE]
bkndb shell FILE [--write] [--history HISTORY_FILE]

bkndb graph query FILE QUERY [--params JSON]
bkndb graph node add FILE LABEL [--properties JSON] --write
bkndb graph node get FILE ID
bkndb graph node update FILE ID --properties JSON --write
bkndb graph node delete FILE ID --write
bkndb graph node list FILE [--label LABEL] [--limit N]
bkndb graph edge add FILE FROM TYPE TO [--properties JSON] --write
bkndb graph edge get FILE ID
bkndb graph edge update FILE ID --properties JSON --write
bkndb graph edge delete FILE ID --write
bkndb graph edge list FILE [--limit N]
bkndb graph neighbors FILE ID [--direction out|in]

bkndb search text FILE TABLE COLUMN QUERY [--limit N]
bkndb search vector FILE TABLE COLUMN '[1.0,0.0]' [--metric cosine|dot|euclidean] [--exact] [--limit N]
bkndb index create FILE secondary|text|vector|property TABLE_OR_LABEL COLUMN [OPTIONS] --write
bkndb index drop FILE secondary|text|vector|property TABLE_OR_LABEL COLUMN --write
```

Node/edge updates merge supplied properties. Deleting nodes follows the library's
edge cleanup contract. Index creation/drop uses the same transactional APIs as
applications. Vector indexes accept `--m` (2..16384) and `--ef-construction`
(1..16384); query vectors contain 1..65536 finite components. List/search limits
are at most one million results. Property indexes use a graph label and property;
the other index kinds use a relational table and column.

SQL is the existing BknDb subset, not PostgreSQL SQL. Write classification uses
the parsed AST, including statements preceded by comments. SQL input is limited to
16 MiB, each parsed statement to 200000 tokens, nesting to 64 and NOT operators to
128. These limits also protect shared SQL/graph parsing from excessive recursion.

Scripts are split with the engine lexer, respecting quoted delimiters and comments.
All statements are parsed before execution. Default scripts execute in one transaction
and rollback together on failure. `--per-statement` explicitly permits earlier
commits to survive a later runtime failure; exit 10 reports completed statements.
Commit-unknown remains exit 8. Do not blindly retry either mode after uncertain I/O.

Parameters accept ordinary JSON arrays (positional) or objects (named). Property
integers must fit signed 64 bits; graph IDs use separate uint64 arguments and are
never converted to floating point. Responses use tagged PropValues such as
`{"Int":9007199254740993}` and `{"Bytes":[0,255]}`. To pass a typed value in ordinary
JSON, wrap it: `{"$bkndb":{"Timestamp":123}}`. Nested lists/maps are supported.
Nonfinite floats use `{"FloatBits":"7ff8000000000029"}` to preserve IEEE-754 bits;
normal JSON has no NaN/Infinity numbers. Consumers must use integer-aware JSON
parsing if they need full-width IDs and values.

The shell supports `.help`, `.info`, `.stats`, `.tables`, `.schema [TABLE]`,
`.indexes [TABLE]`, `.mode table|json|jsonl|csv`, `.timer on|off`, `.read FILE`, `.quit`.
SQL input is multiline and ends at a lexer-recognized final semicolon. Explicit
`BEGIN;`, `COMMIT;`, `ROLLBACK;` controls are shell commands, not new library SQL
statements. They cannot nest. A failed transaction must be rolled back before
continuing. `.read` participates in an active transaction or uses one atomic script.
EOF and `.quit` rollback pending writes. Terminal Ctrl-C rolls back pending work;
query-time cancellation is cooperative at CLI operation/statement boundaries,
not guaranteed instantaneous interruption inside a single engine query/fsync.
An observed successful commit is reported as committed.

Interactive editing provides end-of-line editing, Up/Down history and Tab completion
for commands, SQL keywords and catalog table names. History is disabled on disk by
default. `--history` explicitly stores query text (potentially sensitive); Unix new
history files use mode 0600. This is a minimal editor, not a complete SQL IDE.

## Dumps and CSV

```
bkndb export FILE --output NEW_DUMP
bkndb import NEW_FILE --input DUMP [--backend redb|lsm] --write
bkndb export FILE --table TABLE --output NEW_CSV
bkndb import EXISTING_FILE --input CSV --table TABLE --backend redb|lsm --write
```

Full dump version 1 is newline-delimited JSON with a format/core-encoding header,
hex-encoded physical table/key/value records, and a SHA-256/count footer. It preserves
binary values, full-width IDs, schemas, counters and index definitions. The dump
contains physical index records, but import rebuilds derived indexes from canonical
data before publishing a destination.
This initial physical dump is explicitly bound to `bkndb-core-0.3.1`; incompatible
future encodings require an explicit format migration. It is not an unversioned
logical exchange format. Import reconstructs graph label/property, relational secondary/UNIQUE, full-text
and vector indexes. Vector metric, M and ef-construction parameters are retained;
approximate index layout and tie ordering need not be byte-identical. A failed
rebuild never publishes a partially imported database. Rebuilding can materialize
rows and hit engine transaction limits, in which case use a native backup instead.

Dump export/import stream bounded records; raw records are limited to 16 MiB and
encoded lines to 32 MiB. Table names are bounded to 255 bytes and at most 16384
tables; duplicate/unordered keys and repeated table sections are rejected. SHA-256
checks integrity, not authenticity. Treat database/dump files as sensitive data.
Import commits bounded batches only into a private new database, validates it, then
publishes. Truncation, footer mismatch, trailing bytes or invalid logical records
prevent publication. Existing databases are never merged by full import.

CSV is single-table interchange with an existing schema. The header must name every
column exactly once, with at most 16384 columns per record. Each cell is JSON-encoded tagged PropValue, including FloatBits,
then CSV quoted; this distinguishes NULL, empty strings, blobs and nested values.
It is deliberately not automatic inference from arbitrary third-party CSV. CSV import
is one transaction and rejects duplicate keys/type/constraint violations atomically.
Large imports can hit engine transaction limits; they are not silently split.

Row inspection and CSV export stream rows. SQL/query APIs can still materialize query
results, sorts and aggregations; CLI streaming output does not impose a global engine
memory ceiling. LSM recovery retains its own documented resource limits.

## Phase 3: diagnostics and deterministic workloads

```
bkndb debug header|manifest|wal|sstable FILE
bkndb kv get FILE NAMESPACE utf8:KEY
bkndb kv put FILE NAMESPACE utf8:KEY hex:00ff --write
bkndb kv delete FILE NAMESPACE utf8:KEY --write
bkndb kv scan FILE NAMESPACE [--limit N]
bkndb bench NEW_FILE [--backend redb|lsm] [--operations N] [--seed N]
bkndb stress NEW_FILE [--backend redb|lsm] [--operations N] [--seed N]
bkndb completion bash|zsh|fish|powershell
```

Debug commands support validated LSM structures, omit raw payloads and cap SSTable
summaries at 10000. They use strict opening; they are not forensic decoders that
promise to inspect arbitrary corrupt roots. Repair is a separate explicit operation.

KV commands only access the reserved CLI application table `__bkndb_cli_kv_v1`,
with length-prefixed namespaces (1..64 ASCII letters/digits/_/-). SQL schema creation
cannot claim that table. Keys/values require explicit `utf8:` or `hex:` encoding;
responses use hex. This interface cannot write arbitrary internal graph/SQL tables.

Bench/stress require a new dedicated file. They never run synthetic writes on an
existing database. Workloads have a saved seed, 10000-key domain, 32-byte values,
1000-operation batches and a reference model. Stress adds deletes, rollbacks,
full scans, periodic compaction and reopen. The report includes throughput,
commit-batch/scan p50/p95/p99, file size, final compaction time and available disk/RSS
metrics. These are synthetic measurements, not claims of superiority over Redb.

## Output and exit codes

`--format table|json|jsonl` is global. JSON success reports contain `schema_version`,
`operation`, `status` and `data`. Errors go to stderr with a numeric code, message
and optional structured details. stdout is reserved for result data; prompts,
progres/timers and errors go to stderr. JSONL emits envelopes per top-level list
item. SQL tabular CSV output is supported with `--format csv` (one statement);
non-tabular shell notices in CSV mode go to stderr as JSON.

| Exit | Meaning |
|---|---|
| 0 | Success within reported scope |
| 1 | Internal/bootstrap failure |
| 2 | Invalid arguments/input, constraints or unsupported command usage |
| 3 | Filesystem/backend I/O failure |
| 4 | Database locked |
| 5 | Corruption/invalid stored encoding |
| 6 | Unsupported format/capability |
| 7 | Recovery required or published-output directory-sync uncertainty |
| 8 | Commit outcome unknown; reconcile before retrying |
| 9 | Resource limit |
| 10 | Partial result, including repair loss or per-statement script failure |
| 130 | Cooperative cancellation |

A broken stdout pipe or output failure after a write does not undo a completed
commit. Reconcile database state before retrying a writing command whose report
could not be delivered.

## Verification and release gates

```
cargo test -p bkndb-cli -p bkndb-engine --features bkndb-cli/test-failpoints,bkndb-engine/test-io --release --locked
cargo test --workspace --release --locked
python3 -m unittest discover -s scripts/cli -p 'test_*.py'
python3 scripts/cli/pty_smoke.py --binary target/release/bkndb
python3 scripts/cli/candidate.py smoke --binary target/release/bkndb
cargo +nightly fuzz run cli_inputs -- -max_total_time=60 -timeout=5 -max_len=65536
python3 scripts/cli/candidate.py prepare --target x86_64-unknown-linux-gnu --output dist/cli/x86_64-unknown-linux-gnu
```

`test-failpoints` is for tests only: subprocesses pause at persistence/publication
boundaries so the parent can kill them without destructors. `prepare` rebuilds
without that feature before packaging. Candidate manifests record target, SHA-256,
toolchain and normalized source fingerprint (including fuzz harnesses/corpus and engine CI); changed inputs
require fresh evidence. Preparation rejects custom Rust flags and source changes
during a build. Soak rejects nonfinite durations, binds evidence to its executable
checksum, and refuses source/binary changes during execution.
The isolated runtime test moves the executable, removes build tools from PATH and
runs persistence, SQL, graph, text/vector search and backup on both backends.

`.github/workflows/cli.yml` runs five native candidates with process-crash tests,
model stress, runtime dependency inspection and Unix PTY smoke. The separate fuzz
job retains corpus/artifacts. Ordinary CI has read-only permissions and never
publishes, creates tags or opens PRs.

An optional manual eight-hour CLI soak requires a provisioned self-hosted runner
labelled `bkndb-soak`. It is not run as a normal pull-request job. Start with:
`python3 scripts/cli/candidate.py soak --binary target/release/bkndb --output cli-soak --hours 8`.
The script refuses shorter acceptance runs and writes a success report only after
continuous completion. Engine soak/crash/property/fault gates remain separately
required; CLI stress is not a substitute for engine qualification.

Before a stable release, gather all five target directories and `soak.json`,
`engine.json`, `fuzz.json`, `audit.json` in one evidence directory. Run
`python3 scripts/cli/candidate.py validate --directory EVIDENCE`. Each report must
match schema 1, workspace version and current source fingerprint. Engine/fuzz/audit
reports require `passed: true`; soak requires >=28800 finite continuous seconds and a checksum matching a tested
candidate executable. Audit
also requires a named reviewer, reviewed scope and `critical_findings: []`.
Reviewers must attach underlying CI/soak/fuzz logs; hand-written booleans alone
are not independent evidence. This validator does not authenticate GitHub runs.

No stable designation until all three phases, cross-platform runtime results,
engine acceptance gates, eight-hour soak and review are complete. A missing gate
must fail closed. Candidate validation creates no Git tag or registry publication;
those remain explicit subsequent release actions.

### Local verification snapshot (2026-11-17)

Executed on Linux amd64 with Rust 1.99.1: 255 workspace tests passed (six
long-running/optional tests ignored by that command); the candidate preparation
also passed 25 CLI tests including two process-kill tests, engine fault/model
tests, Unix PTY smoke, isolated runtime smoke and a separate million-operation
CLI stress run. Five Python release-tool tests passed. The native candidate's
checksum and source fingerprint matched the final inputs; dynamic imports were
OS libraries only, with no BknDb shared library.

A fixed-seed CLI-input fuzz smoke completed 61,992 executions in 61 seconds with
AddressSanitizer and no finding. LeakSanitizer was disabled because of local
ptrace restrictions, so leak checking is not claimed. This run exercised the
CLI argument, SQL/script, parameter, CSV and dump parsers; it is bounded coverage,
not an exhaustive decoder or semantic audit.

Windows GNU `cargo check` is cross-compilation evidence only. Runtime results for
all five targets, the continuous eight-hour soak and final review remain required.
The local host build is not a certification of the Linux glibc 2.28 release
baseline; that build must come from the configured manylinux CI environment.
