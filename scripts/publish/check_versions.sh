#!/usr/bin/env bash
# Version Consistency Checker for BknDb
# Verifies that root Cargo.toml, pyproject.toml, and all sub-crate internal dependencies
# are 100% synchronized before publishing.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$WORKSPACE_ROOT"

EXPECTED_VERSION="${1:-}"

echo "=========================================================="
echo "         BknDb Version Consistency Pre-flight           "
echo "=========================================================="

# 1. Determine Target Version from root Cargo.toml if not passed
ROOT_VERSION=$(grep -E '^version = "[^"]+"' Cargo.toml | head -n1 | sed -E 's/version = "([^"]+)"/\1/')
if [ -z "$ROOT_VERSION" ]; then
    echo "Error: Could not read version from root Cargo.toml"
    exit 1
fi

TARGET_VERSION="${EXPECTED_VERSION:-$ROOT_VERSION}"
echo -e "Target Version to Validate : \033[1;33mv$TARGET_VERSION\033[0m\n"

MISMATCHES=()

check_version() {
    local file_path="$1"
    local pattern="$2"
    local desc="$3"

    if [ ! -f "$file_path" ]; then
        echo -e "  \033[1;33m[WARNING]\033[0m File not found: $file_path"
        return
    fi

    local matched_line
    matched_line=$(grep -E "$pattern" "$file_path" | head -n1 || true)

    if [ -n "$matched_line" ]; then
        local found_ver
        found_ver=$(echo "$matched_line" | sed -E 's/.*version = "([^"]+)".*/\1/')
        if [ "$found_ver" = "$TARGET_VERSION" ]; then
            echo -e "  \033[1;32m[OK]\033[0m $file_path \033[90m($desc: v$found_ver)\033[0m"
        else
            echo -e "  \033[1;31m[MISMATCH]\033[0m \033[1;33m$file_path\033[0m"
            echo -e "             \033[1;31mExpected: v$TARGET_VERSION, Found: v$found_ver ($desc)\033[0m"
            MISMATCHES+=("$file_path -> $desc is v$found_ver (needs v$TARGET_VERSION)")
        fi
    else
        echo -e "  \033[1;33m[WARNING]\033[0m $file_path (Pattern '$desc' not found)"
        MISMATCHES+=("$file_path -> $desc (pattern not found)")
    fi
}

# --- Checks ---
# 1. Root Cargo.toml
check_version "Cargo.toml" '^version = "[^"]+"' "workspace.package.version"

# 2. Python pyproject.toml
check_version "bindings/python/pyproject.toml" '^version = "[^"]+"' "project.version"

# 3. Main facade crate internal dependencies
check_version "crates/bkndb/Cargo.toml" 'bkndb-core = \{ version = "[^"]+"' "dep:bkndb-core"
check_version "crates/bkndb/Cargo.toml" 'bkndb-storage-redb = \{ version = "[^"]+"' "dep:bkndb-storage-redb"
check_version "crates/bkndb/Cargo.toml" 'bkndb-storage-mem = \{ version = "[^"]+"' "dep:bkndb-storage-mem"
check_version "crates/bkndb/Cargo.toml" 'bkndb-storage-lsm = \{ version = "[^"]+"' "dep:bkndb-storage-lsm"

# 4. Storage crates internal dependencies to bkndb-core
check_version "crates/bkndb-storage-lsm/Cargo.toml" 'bkndb-core = \{ version = "[^"]+"' "dep:bkndb-core"
check_version "crates/bkndb-storage-mem/Cargo.toml" 'bkndb-core = \{ version = "[^"]+"' "dep:bkndb-core"
check_version "crates/bkndb-storage-redb/Cargo.toml" 'bkndb-core = \{ version = "[^"]+"' "dep:bkndb-core"

echo ""
if [ ${#MISMATCHES[@]} -gt 0 ]; then
    echo "=========================================================="
    echo "  VERSION MISMATCH DETECTED IN ${#MISMATCHES[@]} LOCATION(S)! "
    echo "=========================================================="
    echo "Please update the following files before publishing:"
    for m in "${MISMATCHES[@]}"; do
        echo -e "  * \033[1;31m$m\033[0m"
    done
    echo ""
    echo -e "Tip: You can synchronize all versions automatically with:"
    echo -e "     ./scripts/publish/set_version.sh $TARGET_VERSION"
    exit 1
else
    echo "=========================================================="
    echo -e "  \033[1;32mALL PROJECT VERSIONS ARE 100% SYNCHRONIZED (v$TARGET_VERSION)!\033[0m"
    echo "=========================================================="
    exit 0
fi
