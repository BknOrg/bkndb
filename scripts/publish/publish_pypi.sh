#!/usr/bin/env bash
# Automated Publishing Script for BknDb to PyPI (Bash / Linux / macOS)
# Modes:
# 1. 'github-tag' (default, recommended): pushes tag python-vX.Y.Z to trigger GitHub Actions
# 2. 'local': builds and uploads directly via maturin

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
PYTHON_DIR="$WORKSPACE_ROOT/bindings/python"

MODE="github-tag"
DRY_RUN=false
TEST_PYPI=false
USERNAME="__token__"
PASSWORD=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --mode)
      MODE="$2"
      shift 2
      ;;
    --dry-run)
      DRY_RUN=true
      shift
      ;;
    --test-pypi)
      TEST_PYPI=true
      shift
      ;;
    --username|-u)
      USERNAME="$2"
      shift 2
      ;;
    --password|-p)
      PASSWORD="$2"
      shift 2
      ;;
    -h|--help)
      echo "Usage: ./publish_pypi.sh [--mode github-tag|local] [--dry-run] [--test-pypi] [-u <user>] [-p <pass>]"
      exit 0
      ;;
    *)
      echo "Unknown option: $1"
      exit 1
      ;;
  esac
done

echo "=========================================================="
echo "         BknDb PyPI Automated Publisher                  "
echo "=========================================================="

VERSION=$(grep -m 1 '^version =' "$PYTHON_DIR/pyproject.toml" | cut -d '"' -f 2)
echo "Detected Python Package Version: v${VERSION}"
echo "Selected Mode: ${MODE}"

# 0. Pre-flight Version Consistency Check
echo ""
echo "Checking version consistency across repository..."
"$SCRIPT_DIR/check_versions.sh" "$VERSION"

if [[ "$MODE" == "github-tag" ]]; then
  TAG_NAME="python-v${VERSION}"
  echo ""
  echo "[GitHub Tag Release Flow]"
  echo "This triggers .github/workflows/python-wheels.yml for cross-platform wheels."

  if [[ "$DRY_RUN" == true ]]; then
    echo "[DRY-RUN] Would create and push tag: ${TAG_NAME}"
    exit 0
  fi

  cd "$WORKSPACE_ROOT"

  if git rev-parse "$TAG_NAME" >/dev/null 2>&1; then
    echo "Warning: Tag '${TAG_NAME}' already exists locally. Deleting and recreating..."
    git tag -d "$TAG_NAME"
    git push origin ":refs/tags/${TAG_NAME}" 2>/dev/null || true
  fi

  echo "Creating Git Tag: ${TAG_NAME}"
  git tag -a "$TAG_NAME" -m "Release Python bindings v${VERSION} to PyPI"

  echo "Pushing Tag to GitHub..."
  git push origin "$TAG_NAME"

  echo ""
  echo "[SUCCESS] Tag ${TAG_NAME} pushed to GitHub!"
  echo "Track progress at: https://github.com/BknOrg/bkndb/actions"
  exit 0
fi

if [[ "$MODE" == "local" ]]; then
  echo ""
  echo "[Local Build & Upload Flow]"
  cd "$PYTHON_DIR"

  MATURIN_BIN="maturin"
  if ! command -v maturin &>/dev/null; then
    MATURIN_BIN="python -m maturin"
  fi

  if [[ "$DRY_RUN" == true ]]; then
    echo "[DRY-RUN] Building local wheel and sdist with maturin..."
    $MATURIN_BIN build --release --sdist --out dist
    echo "[OK] Built artifacts successfully in $PYTHON_DIR/dist"
    exit 0
  fi

  PUBLISH_ARGS=()
  if [[ "$TEST_PYPI" == true ]]; then
    echo "Target Registry: TestPyPI"
    PUBLISH_ARGS+=(--repository testpypi)
  else
    echo "Target Registry: Production PyPI"
  fi

  if [[ -n "$PASSWORD" ]]; then
    PUBLISH_ARGS+=(-u "$USERNAME" -p "$PASSWORD")
  fi

  echo "Running maturin publish..."
  $MATURIN_BIN publish "${PUBLISH_ARGS[@]}"

  echo ""
  echo "[SUCCESS] Published Python package v${VERSION} to PyPI!"
fi
