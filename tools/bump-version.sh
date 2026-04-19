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
#   - deploy/helm/wasmsh/Chart.yaml (appVersion; chart version stays
#     independent because the chart's lifecycle is not tied 1:1 to the
#     app version — bump it deliberately when the chart API changes)

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

# Rust workspace. Two places need bumping in Cargo.toml:
#   1. [workspace.package].version — the version inherited by all member
#      crates via `version.workspace = true`.
#   2. The internal-dep block in [workspace.dependencies] — every
#      `wasmsh-* = { version = "X.Y.Z", path = "..." }` entry. `cargo
#      publish` consumes the `version` field, not `path`, so these must
#      move in lockstep with #1 or published crates will pin each other
#      at an older version than what they actually ship with.
sedi "s/^version = \".*\"/version = \"$VERSION\"/" "$REPO_ROOT/Cargo.toml"
sedi -E "s/^(wasmsh-[a-z_-]+ = \{ version = )\"[0-9]+\.[0-9]+\.[0-9]+\"/\1\"$VERSION\"/" "$REPO_ROOT/Cargo.toml"
echo "  Cargo.toml (workspace + internal dep pins)"

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

# Helm chart appVersion.  The chart's own `version:` is left alone — chart
# versioning is semver over the chart API, not over the app it deploys,
# and is bumped deliberately when values schema or template surface moves.
sedi -E "s/^(appVersion: )\"[^\"]+\"/\1\"$VERSION\"/" "$REPO_ROOT/deploy/helm/wasmsh/Chart.yaml"
echo "  deploy/helm/wasmsh/Chart.yaml (appVersion only)"

# Regenerate Cargo.lock files
echo "  Regenerating lockfiles..."
(cd "$REPO_ROOT" && cargo generate-lockfile 2>/dev/null) || true
for crate in wasmsh-pyodide-probe wasmsh-pyodide; do
    (cd "$REPO_ROOT/crates/$crate" && cargo generate-lockfile 2>/dev/null) || true
done

echo "Done. All packages set to $VERSION"
