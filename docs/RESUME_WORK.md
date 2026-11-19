# Resume the unfinished LSM, WASM and CLI work

This is a work-in-progress backup, not a stable release. The snapshot is on
`backup/lsm-wasm-cli-wip-2026-11-19`; it must not be interpreted as a promotion
of LSM, a published package, or an approval to merge into `main`.

## Restore after reinstalling the OS

```sh
git clone --branch backup/lsm-wasm-cli-wip-2026-11-19 https://github.com/BknOrg/bkndb.git
cd bkndb
```

Read `docs/REPO_MEMORY.md`, `docs/LSM_AUDIT.md`, `docs/LSM_ENGINE.md` and
`docs/CLI.md` before continuing. The workspace version remains 0.3.1; it does
not establish that these changes have been published under that version.

Install the Rust toolchain specified by the workflows (currently 1.99.1) and
the OS linker/build tools, then use:

```sh
cargo build --release --locked -p bkndb-cli
./target/release/bkndb --help
cargo test --workspace --release --locked
cargo test -p bkndb-cli --features test-failpoints --release --locked
python3 -m unittest discover -s scripts/cli -p 'test_*.py'
```

The checkout includes source, lockfiles, tests, fuzz corpus, workflows,
documentation, the embedded WASM module and the current Linux amd64 static
archive. Build caches, temporary test databases/logs and the local CodeRecall
index are not part of this backup. Rebuild those caches as needed. `.mcp.json`
contains local executable/repository paths; adjust them for the new installation.

## What still needs qualification

- Redb stays the default; LSM is opt-in and uses a different data format.
- CLI and engine changes remain validation candidates. The latest recorded local
  checks are described in the repository memory and audit, not a stable guarantee.
- Obtain actual runtime CI results on the five supported native platforms, run
  the continuous eight-hour soak, and complete the remaining audit/review gates.
- Rebuild and verify native/WASM distribution artifacts from matching source
  fingerprints before preparing any release. The backed-up host archive is not
  a certified five-platform release distribution.
- Do not publish packages, create/retarget version tags, merge into `main`, or
  change the default backend as part of restoring this backup.
