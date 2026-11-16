# BknDb repository memory

## Purpose and architecture

BknDb is an embedded Rust hybrid relational/property-graph database with full-text
and vector search. Workspace crates separate the core, Redb, memory and optional
LSM storage backends, the public `bkndb` facade and `bkndb-ffi`. Bindings exist for
Python, Go, Kotlin and Swift. The FFI crate exposes C ABI plus UniFFI interfaces.

`BknDb::open` defaults to Redb. LSM is optional and has a different file format;
a shared `.bkndb` suffix does not imply interchangeability or automatic migration.
Go persistent databases continue using Redb and native filesystem I/O.

## Versions and Go distribution

Workspace and Python package versions are 0.3.1. Internal crate dependency
versions and Cargo.lock workspace entries must stay synchronized. Go gets its
version from `bindings/go/bkndb/v0.3.1`, not a field in go.mod. A version bump in
source does not mean a release is published.

The Go driver uses CGO on Linux amd64/arm64, macOS amd64/arm64 and Windows amd64.
Its header is copied from `bindings/bkndb.h`; it links the explicit package-local
`lib/<os>_<arch>/libbkndb_ffi.a`. No BknDb DLL extraction or dynamic loader exists.
Go public API and C ABI are unchanged by this packaging migration. Windows now
requires CGO and MinGW-w64 at build time; compiler-free Windows builds are no
longer supported. CGO-disabled and unsupported-platform builds fail explicitly.

Published modules are intended to include all five static archives. Consumers
need Go 1.22+, CGO and a C compiler, but no Rust, local repo replacement or separate
BknDb runtime library. System OS libraries may still be dynamic. Release Linux
archives target glibc 2.28+, macOS requires 11.0+. The final executable
may require a newer libc depending on the consumer's C build toolchain. Alpine/musl and WASM are outside
the supported Go distribution. Cross compilation also needs a target C compiler.

## Commands and workflows

- Rust: `cargo check --workspace --locked`; CI tests with
  `cargo test --workspace --release`. Release mode matters because some tests
  enforce performance budgets.
- Standard native FFI: `cargo build --release -p bkndb-ffi` (other bindings).
- Go maintainer build: install the Rust target, then
  `python3 scripts/go/package.py build linux_amd64` (select host platform).
  This uses the `go-dist` profile with debug info stripped.
- Go checks: `CGO_ENABLED=1 go test ./...` inside `bindings/go/bkndb`, then
  `python3 scripts/go/package.py consumer` from repo root. Packaging checks:
  `python3 -m unittest discover -s scripts/go -p 'test_*.py'`.
- Version checks: `bash scripts/publish/check_versions.sh` or the PowerShell
  counterpart. They check an existing combined Go distribution manifest too.
- Source version updates: scripts under `scripts/publish`; after changing source
  or versions, old static archives must be rebuilt rather than relabeled.

CI builds all five Go targets and tests an external consumer via a local module
proxy with its own cache, without `replace`. Release preparation uses manylinux
2.28, macOS deployment target 11.0 and Windows GNU, pinned Rust 1.99.1. Local Linux
build records use the real host glibc and cannot masquerade as release builds.

Release sequence: merge source into main → manually run Prepare Go distribution
PR → review/merge generated archives and manifest → manually run Publish Go module
tag. The tag must reference a commit already containing ordinary Git archive
blobs. GitHub Release attachments/Git LFS do not satisfy Go module distribution.
Checksums, input fingerprints, toolchain/baselines, header equality, five-target
coverage and Go ZIP size limits are checked before tagging. Preparation/publication requests are serialized. Tag publication
retries succeed only when an existing remote tag resolves to the validated commit;
conflicting tags are never moved. Transient CI failures can rerun failed jobs;
source fixes require a new workflow run at the new commit, with the same version
until a public tag exists. Target artifacts are retained for 30 days. Publishing Rust/Python
remains separate; `publish_all` does not publish the Go module.

## Working conventions and caveats

The user prefers Indonesian explanations. Treat this file as an onboarding
snapshot; verify current manifests/source. Update it when workflows change.
Do not store credentials, private data, transient machine state, or claim that
cross-platform CI passed without observing its results. Building archives locally
only verifies that host; release readiness requires all five CI targets.
