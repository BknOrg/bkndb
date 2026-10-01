#!/usr/bin/env bash
# Automated Publishing Script for BknDb to Crates.io (Bash / Linux / macOS)
# Enforces exact dependency-tier order with verification & index propagation delays.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$WORKSPACE_ROOT"

DRY_RUN=""
ALLOW_DIRTY=""
WAIT_SECONDS=45

while [[ $# -gt 0 ]]; do
  case "$1" in
    --dry-run)
      DRY_RUN="--dry-run"
      shift
      ;;
    --allow-dirty)
      ALLOW_DIRTY="--allow-dirty"
      shift
      ;;
    --wait-seconds)
      WAIT_SECONDS="$2"
      shift 2
      ;;
    -h|--help)
      echo "Usage: ./publish_crates.sh [--dry-run] [--allow-dirty] [--wait-seconds <sec>]"
      exit 0
      ;;
    *)
      echo "Unknown option: $1"
      exit 1
      ;;
  esac
done

echo "=========================================================="
echo "       BknDb Crates.io Automated Publisher               "
echo "=========================================================="

VERSION=$(grep -m 1 '^version =' Cargo.toml | cut -d '"' -f 2)
echo "Detected Workspace Version: v${VERSION}"

# 0. Pre-flight Version Consistency Check
echo ""
echo "[Step 0/3] Checking version consistency across repository..."
"$SCRIPT_DIR/check_versions.sh" "$VERSION"

publish_crate() {
  local crate_name="$1"
  local description="$2"

  echo ""
  echo "--> Publishing crate: ${crate_name} (${description})..."
  cargo publish -p "${crate_name}" ${DRY_RUN} ${ALLOW_DIRTY}
  echo "[OK] Successfully processed ${crate_name}."
}

wait_propagation() {
  local sec="$1"
  if [[ -n "$DRY_RUN" ]]; then
    echo "Dry run: skipping index propagation wait."
    return
  fi

  echo ""
  echo "Waiting ${sec} seconds for crates.io index propagation..."
  while [ "$sec" -gt 0 ]; do
    printf "\rRemaining: %2d s" "$sec"
    sleep 1
    sec=$((sec - 1))
  done
  printf "\rIndex propagation wait complete!       \n"
}

# --- TIER 1: Core Primitives ---
echo ""
echo "[Tier 1/3] Publishing Core Foundation"
publish_crate "bkndb-core" "Core primitives and interfaces"

wait_propagation "$WAIT_SECONDS"

# --- TIER 2: Storage Engines ---
echo ""
echo "[Tier 2/3] Publishing Storage Backends"
publish_crate "bkndb-storage-mem" "In-memory backend"
publish_crate "bkndb-storage-lsm" "LSM-Tree persistent storage (.bkndb)"
publish_crate "bkndb-storage-redb" "Optional redb backend"

wait_propagation "$WAIT_SECONDS"

# --- TIER 3: Main Facade ---
echo ""
echo "[Tier 3/3] Publishing Main Facade Crate"
publish_crate "bkndb" "Top-level embedded database engine"

echo ""
echo "=========================================================="
echo "   ALL CRATES PUBLISHED SUCCESSFULLY TO CRATES.IO!        "
echo "=========================================================="
echo "Verified crates at: https://crates.io/crates/bkndb"
