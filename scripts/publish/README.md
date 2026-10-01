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
.\scripts\publish\check_versions.ps1 -ExpectedVersion 0.3.0
```

### Auto-Bump / Synchronize All Versions in 1 Command:
```powershell
# Automatically updates Cargo.toml, pyproject.toml, and all internal crate dependencies,
# runs 'cargo check' to refresh Cargo.lock, and verifies everything:
.\scripts\publish\set_version.ps1 0.3.0
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
Creates and pushes a tag `python-v<version>` (e.g. `python-v0.2.0`). This triggers `.github/workflows/python-wheels.yml`, which compiles native wheels across:
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
