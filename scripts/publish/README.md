# BknDb Release & Publishing Automation

Automated scripts for releasing `bkn-db` across registries (**Crates.io** and **PyPI**).

---

## Overview of Scripts

| Script | Purpose |
|---|---|
| [`check_versions.ps1`](check_versions.ps1) / [`.sh`](check_versions.sh) | Pre-flight validator: checks whether root `Cargo.toml`, `pyproject.toml`, and all internal sub-crate dependencies are 100% in sync. Lists any mismatched files. |
| [`set_version.ps1`](set_version.ps1) / [`.sh`](set_version.sh) | Synchronizer: bumps version across all 6 repository files and updates `Cargo.lock` in one command. |
| [`publish_crates.ps1`](publish_crates.ps1) / [`.sh`](publish_crates.sh) | Publishes all Rust workspace crates to **crates.io** in strict dependency order with automatic index propagation wait times. |
| [`publish_pypi.ps1`](publish_pypi.ps1) / [`.sh`](publish_pypi.sh) | Publishes the Python package `bkndb` to **PyPI** (via automated GitHub Actions cross-compilation tag or local `maturin`). |
| [`publish_all.ps1`](publish_all.ps1) / [`.sh`](publish_all.sh) | Master orchestrator: runs pre-flight version check, tests, publishes to Crates.io, and triggers the PyPI release. |

---

## 0. Version Management & Pre-flight Consistency

Before publishing, all crates and bindings must share the exact same version string. If any internal dependency or `pyproject.toml` is left on an older version, publishing will either fail or release broken dependencies.

### Check Version Synchronization:
```powershell
# Validates all 6 locations against the version in root Cargo.toml:
.\scripts\publish\check_versions.ps1

# Or validate against a specific expected version:
.\scripts\publish\check_versions.ps1 -ExpectedVersion 0.3.1
```

### Auto-Bump / Synchronize All Versions in 1 Command:
```powershell
# Automatically updates Cargo.toml, pyproject.toml, and all internal crate dependencies,
# runs 'cargo check' to refresh Cargo.lock, and verifies everything:
.\scripts\publish\set_version.ps1 0.3.1
```

Bash equivalents (`./scripts/publish/check_versions.sh` and `./scripts/publish/set_version.sh`) are also available.

---

## 1. Publishing to Crates.io

Crates.io enforces that dependent crates must already exist on crates.io before a crate depending on them can be published. The script enforces the exact tier hierarchy:

```
[Tier 1] bkndb-core
   │
   ▼ (wait for index propagation, default ~45s)
[Tier 2] bkndb-storage-mem, bkndb-storage-lsm, bkndb-storage-redb
   │
   ▼ (wait for index propagation, default ~45s)
[Tier 3] bkndb (main facade)
```

### Usage (PowerShell):
```powershell
# Test without publishing (dry-run):
.\scripts\publish\publish_crates.ps1 -DryRun

# Full publish:
.\scripts\publish\publish_crates.ps1

# Custom index wait time (e.g. 60 seconds):
.\scripts\publish\publish_crates.ps1 -WaitSeconds 60
```

### Usage (Bash):
```bash
./scripts/publish/publish_crates.sh --dry-run
./scripts/publish/publish_crates.sh --wait-seconds 60
```

---

## 2. Publishing to PyPI

Because `bkndb` contains compiled Rust FFI code, distributing on PyPI requires compiled wheels for each target operating system (Windows, Linux, macOS) or a source distribution.

The script supports two modes:

### Mode A: `GitHubTag` (Recommended for Production)
Creates and pushes a tag `python-v<version>` (e.g. `python-v0.3.1`). This triggers `.github/workflows/python-wheels.yml`, which compiles native wheels across:
* Windows x64
* Linux x86_64 & aarch64 (`manylinux`)
* macOS Intel (x86_64) & Apple Silicon (`arm64`)
* Source distribution (`.tar.gz`)

Then it automatically uploads all packages to PyPI via **Trusted Publishing** (no local credentials required).

```powershell
# Dry run:
.\scripts\publish\publish_pypi.ps1 -DryRun

# Release to PyPI via GitHub Tag:
.\scripts\publish\publish_pypi.ps1 -Mode GitHubTag
```

### Mode B: `Local` (For quick local or test publishing)
Uses `maturin` locally on your machine:
```powershell
# Publish to TestPyPI:
.\scripts\publish\publish_pypi.ps1 -Mode Local -TestPyPI

# Publish to Production PyPI:
.\scripts\publish\publish_pypi.ps1 -Mode Local -Password "<your_pypi_token>"
```

---

## 3. Master Release Orchestrator (`publish_all`)

Coordinates the entire release pipeline:
1. Runs workspace tests (`cargo test --workspace`).
2. Publishes Rust crates to crates.io.
3. Tags and publishes Python wheels to PyPI.

```powershell
# Dry run to verify everything safely:
.\scripts\publish\publish_all.ps1 -DryRun

# Execute full release:
.\scripts\publish\publish_all.ps1
```


## 4. Go Static Module Distribution

Go publishing is a separate, explicit workflow; `publish_all` continues to publish
Rust and Python only. The Go submodule has no version field in `go.mod`: its
version comes from tag `bindings/go/bkndb/v0.3.1`.

1. Update the workspace/Python version and lockfile, then merge source changes
   into `main`. Run `check_versions.sh` (or `.ps1`). Old Go manifests must be
   rebuilt when versions change.
2. Run **Prepare Go distribution PR** (`go-prepare.yml`) manually on `main`.
   CI builds and tests Linux amd64/arm64 in manylinux glibc 2.28 containers,
   macOS amd64/arm64 with deployment target 11.0, and Windows amd64 using
   `x86_64-pc-windows-gnu` and MinGW-w64. Rust is pinned to 1.99.1.
3. Review and merge the generated `codex/go-distribution-0.3.1` PR. It contains
   ordinary Git blobs for all five static archives plus per-target build records
   and a combined manifest. GitHub Actions must be allowed to create pull requests
   in repository settings. The job that opens this PR has contents/PR write
   permissions; ordinary build jobs have read-only contents access.
4. Run **Publish Go module tag** (`go-release.yml`) manually on `main`. It verifies
   version, header equality, checksums, source fingerprint, pinned Rust toolchain,
   Linux baseline, complete platform coverage and ZIP sizes before tagging the
   exact validated commit. A retry succeeds if the existing tag already references that exact commit;
   tags pointing to another commit are rejected, never replaced.
5. Consumers can then run `go get .../bindings/go/bkndb@v0.3.1`. Uploading an archive
   to GitHub Releases alone does not include it in a Go module download.

Preparation tests an isolated consumer through a local Go file proxy, including
package tests, disk reopen/transaction smoke checks, executable import inspection,
and execution with build tools removed from PATH. Rust/Cargo source directories
are absent from the consumer's downloaded module. System libraries are still
permitted; only a separate BknDb shared library is forbidden.

### Local development and verification

Python 3.9+, Rust, Go and a native C compiler are required by maintainers:

```bash
rustup target add x86_64-unknown-linux-gnu
python3 scripts/go/package.py build linux_amd64
python3 scripts/go/package.py verify
python3 scripts/go/package.py consumer
python3 -m unittest discover -s scripts/go -p 'test_*.py'
```

Select the corresponding supported platform for other hosts. Windows needs
`x86_64-pc-windows-gnu`, not MSVC; use MinGW-w64 GCC on PATH. On PowerShell use
`python` and `$env:CGO_ENABLED = "1"`.

Local Linux archives record the actual host glibc and are suitable for local
checks. The release validator rejects them unless built with glibc 2.28. Release
builds reject custom RUSTFLAGS to avoid machine-specific CPU requirements.
`native_static_libs` in each build record preserves rustc's linker requirements;
consumer linking verifies that the CGO system-library flags satisfy them.

`package.py assemble` requires all five fresh build records and produces the
combined manifest. `package.py verify --all` and `package.py zip --all` validate a
complete distribution. ZIP compressed and uncompressed sizes must each stay at
or below 500 MiB. Archives must not use Git LFS or symlinks. Updating Rust or the
build inputs requires rebuilding the archives and reviewing toolchain/baseline
checks together; do not copy old metadata onto new archives.


### Recovering from CI failures without consuming a version

Keep version 0.3.1 until its final Go tag is published. A failed build or failed
PR creation does not publish that version. For a transient runner/network failure,
use **Re-run failed jobs** on the same run. Successful target artifacts are retained
for 30 days, and rerun uploads replace only that target's artifact. If artifacts
have expired, rerun all jobs. Preparation and publication requests are serialized
to prevent concurrent updates.

If source or workflow configuration needs fixing, commit the fix with version
0.3.1 unchanged and start a **new** preparation run at the new commit. GitHub's
Re-run button always uses the original commit and will not pick up a source fix.
All target archives must then be rebuilt from the new source fingerprint.

If tag publication loses its response after GitHub accepted the push, rerun the
publication workflow. It reconciles remote state and succeeds when the tag points
to the validated commit. It never force-pushes a tag. After a public tag exists,
source fixes require a new patch version, such as 0.3.2.
