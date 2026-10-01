#!/usr/bin/env bash
# Master Release & Publishing Orchestrator for BknDb (Bash / Linux / macOS)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$WORKSPACE_ROOT"

DRY_RUN=""
SKIP_TESTS=false
CRATES_ONLY=false
PYPI_ONLY=false
PYPI_MODE="github-tag"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --dry-run)
      DRY_RUN="--dry-run"
      shift
      ;;
    --skip-tests)
      SKIP_TESTS=true
      shift
      ;;
    --crates-only)
      CRATES_ONLY=true
      shift
      ;;
    --pypi-only)
      PYPI_ONLY=true
      shift
      ;;
    --pypi-mode)
      PYPI_MODE="$2"
      shift 2
      ;;
    -h|--help)
      echo "Usage: ./publish_all.sh [--dry-run] [--skip-tests] [--crates-only] [--pypi-only] [--pypi-mode github-tag|local]"
      exit 0
      ;;
    *)
      echo "Unknown option: $1"
      exit 1
      ;;
  esac
done

echo "=========================================================="
echo "         BknDb Unified Release & Publisher               "
echo "=========================================================="

RUST_VER=$(grep -m 1 '^version =' Cargo.toml | cut -d '"' -f 2)
PY_VER=$(grep -m 1 '^version =' bindings/python/pyproject.toml | cut -d '"' -f 2)

echo "Rust Workspace Version   : v${RUST_VER}"
echo "Python Package Version   : v${PY_VER}"

# Pre-flight: Check that all versions match across the entire workspace
echo ""
echo "Checking repository-wide version consistency..."
"$SCRIPT_DIR/check_versions.sh" "$RUST_VER"

# 1. Pre-flight tests
if [[ "$SKIP_TESTS" == false && -z "$DRY_RUN" ]]; then
  echo ""
  echo "[Step 1/3] Running Cargo Workspace Tests..."
  cargo test --workspace
  echo "[OK] All Cargo tests passed!"
fi

# 2. Publish to Crates.io
if [[ "$PYPI_ONLY" == false ]]; then
  echo ""
  echo "[Step 2/3] Publishing to Crates.io..."
  bash "$SCRIPT_DIR/publish_crates.sh" ${DRY_RUN}
fi

# 3. Publish to PyPI
if [[ "$CRATES_ONLY" == false ]]; then
  echo ""
  echo "[Step 3/3] Publishing to PyPI..."
  bash "$SCRIPT_DIR/publish_pypi.sh" --mode "$PYPI_MODE" ${DRY_RUN}
fi

echo ""
echo "=========================================================="
echo "         RELEASE PIPELINE COMPLETED SUCCESSFULLY!         "
echo "=========================================================="
