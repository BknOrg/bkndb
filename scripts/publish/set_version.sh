#!/usr/bin/env bash
# Synchronize Version Across Entire BknDb Repository
# Updates root Cargo.toml, pyproject.toml, and all sub-crate internal dependencies.

set -euo pipefail

NEW_VERSION="${1:-}"

if [ -z "$NEW_VERSION" ]; then
    echo "Usage: $0 <new_version> (e.g. 0.2.1, 0.3.0)"
    exit 1
fi

# Strip optional leading 'v'
NEW_VERSION="${NEW_VERSION#v}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$WORKSPACE_ROOT"

echo "=========================================================="
echo "         BknDb Version Synchronizer                      "
echo "=========================================================="
echo -e "Bumping all components to version: \033[1;33mv$NEW_VERSION\033[0m\n"

# Helper for cross-platform in-place sed
sed_inplace() {
    local expr="$1"
    local file="$2"
    if [[ "$OSTYPE" == "darwin"* ]]; then
        sed -i '' -E "$expr" "$file"
    else
        sed -i -E "$expr" "$file"
    fi
}

# 1. Cargo.toml (root package version)
sed_inplace "s/^version = \"[^\"]+\"/version = \"$NEW_VERSION\"/" Cargo.toml
echo -e "  \033[1;32m[UPDATED]\033[0m Cargo.toml"

# 2. bindings/python/pyproject.toml
sed_inplace "s/^version = \"[^\"]+\"/version = \"$NEW_VERSION\"/" bindings/python/pyproject.toml
echo -e "  \033[1;32m[UPDATED]\033[0m bindings/python/pyproject.toml"

# 3. crates/bkndb/Cargo.toml
sed_inplace "s/bkndb-core = \{ version = \"[^\"]+\"/bkndb-core = \{ version = \"$NEW_VERSION\"/" crates/bkndb/Cargo.toml
sed_inplace "s/bkndb-storage-redb = \{ version = \"[^\"]+\"/bkndb-storage-redb = \{ version = \"$NEW_VERSION\"/" crates/bkndb/Cargo.toml
sed_inplace "s/bkndb-storage-mem = \{ version = \"[^\"]+\"/bkndb-storage-mem = \{ version = \"$NEW_VERSION\"/" crates/bkndb/Cargo.toml
sed_inplace "s/bkndb-storage-lsm = \{ version = \"[^\"]+\"/bkndb-storage-lsm = \{ version = \"$NEW_VERSION\"/" crates/bkndb/Cargo.toml
echo -e "  \033[1;32m[UPDATED]\033[0m crates/bkndb/Cargo.toml"

# 4. Storage crates internal dependencies to bkndb-core
for f in crates/bkndb-storage-lsm/Cargo.toml crates/bkndb-storage-mem/Cargo.toml crates/bkndb-storage-redb/Cargo.toml; do
    sed_inplace "s/bkndb-core = \{ version = \"[^\"]+\"/bkndb-core = \{ version = \"$NEW_VERSION\"/" "$f"
    echo -e "  \033[1;32m[UPDATED]\033[0m $f"
done

echo ""
echo "Updating Cargo.lock to match new workspace version..."
if command -v cargo >/dev/null 2>&1; then
    cargo check --workspace --quiet || true
    echo -e "  \033[1;32m[OK]\033[0m Cargo.lock updated successfully"
fi

echo ""
# Run validation check
"$SCRIPT_DIR/check_versions.sh" "$NEW_VERSION"
