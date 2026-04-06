#!/usr/bin/env bash
set -euo pipefail

# Bump the version across all package manifests in the wasmsh monorepo.
#
# Usage: tools/bump-version.sh 0.6.0
#
# Updates:
#   - Cargo.toml (workspace version, inherited by 15 crates)
#   - crates/wasmsh-pyodide-probe/Cargo.toml (excluded from workspace)
#   - crates/wasmsh-pyodide/Cargo.toml (excluded from workspace)
#   - packages/npm/wasmsh-pyodide/package.json
#   - packages/python/wasmsh-pyodide-runtime/pyproject.toml

if [ $# -ne 1 ]; then
    echo "Usage: $0 <version>"
    echo "Example: $0 0.6.0"
    exit 1
fi

VERSION="$1"

if ! echo "$VERSION" | grep -qE '^[0-9]+\.[0-9]+\.[0-9]+'; then
    echo "Error: version must be semver (e.g. 0.6.0), got: $VERSION"
    exit 1
fi

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

# Portable sed -i (macOS vs GNU)
sedi() {
    if sed --version 2>/dev/null | grep -q GNU; then
        sed -i "$@"
    else
        sed -i '' "$@"
    fi
}

echo "Bumping all packages to $VERSION ..."

# Rust workspace
sedi "s/^version = \".*\"/version = \"$VERSION\"/" "$REPO_ROOT/Cargo.toml"
echo "  Cargo.toml (workspace)"

# Excluded Rust crates
for crate in wasmsh-pyodide-probe wasmsh-pyodide; do
    sedi "s/^version = \".*\"/version = \"$VERSION\"/" "$REPO_ROOT/crates/$crate/Cargo.toml"
    echo "  crates/$crate/Cargo.toml"
done

# npm package
sedi "s/\"version\": \".*\"/\"version\": \"$VERSION\"/" "$REPO_ROOT/packages/npm/wasmsh-pyodide/package.json"
echo "  packages/npm/wasmsh-pyodide/package.json"

# Python package
sedi "s/^version = \".*\"/version = \"$VERSION\"/" "$REPO_ROOT/packages/python/wasmsh-pyodide-runtime/pyproject.toml"
echo "  packages/python/wasmsh-pyodide-runtime/pyproject.toml"

# Regenerate Cargo.lock files
echo "  Regenerating lockfiles..."
(cd "$REPO_ROOT" && cargo generate-lockfile 2>/dev/null) || true
for crate in wasmsh-pyodide-probe wasmsh-pyodide; do
    (cd "$REPO_ROOT/crates/$crate" && cargo generate-lockfile 2>/dev/null) || true
done

echo "Done. All packages set to $VERSION"
