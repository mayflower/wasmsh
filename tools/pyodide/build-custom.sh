#!/usr/bin/env bash
set -euo pipefail

# ── Paths ───────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
# shellcheck source=versions.env
source "$SCRIPT_DIR/versions.env"
PYODIDE_SRC="$SCRIPT_DIR/pyodide-src"
DIST_DIR="$REPO_ROOT/dist/pyodide-custom"

export PATH="$HOME/.cargo/bin:$(rustc --print sysroot 2>/dev/null)/bin:$PATH"

echo "=== wasmsh custom Pyodide build ==="
echo "Pyodide version: $PYODIDE_VERSION"
echo "Repo root:       $REPO_ROOT"

# ── Clone Pyodide source ───────────────────────────────────
if [ ! -d "$PYODIDE_SRC" ]; then
    echo "Cloning Pyodide $PYODIDE_VERSION..."
    git clone --depth 1 --branch "$PYODIDE_VERSION" \
        https://github.com/pyodide/pyodide.git "$PYODIDE_SRC"
else
    echo "Pyodide source already present."
fi

# ── Setup Pyodide's emsdk ──────────────────────────────────
cd "$PYODIDE_SRC"

PYODIDE_EMCC_VERSION="$EMSCRIPTEN_VERSION"
echo "Pyodide emscripten version: $PYODIDE_EMCC_VERSION"

if [ ! -f emsdk/emsdk/.complete ]; then
    echo "Setting up Pyodide's emsdk (ccache build skipped — cmake compat)..."
    cd emsdk
    [ -d emsdk ] && rm -rf emsdk
    git clone --depth 1 https://github.com/emscripten-core/emsdk.git
    cd emsdk
    ./emsdk install --build=Release "$PYODIDE_EMCC_VERSION"

    # Apply Pyodide's emscripten patches.
    #
    # `--forward` makes `patch` exit 1 if *all* hunks are already applied
    # (which is fine on a re-run of this block after a partial success) and
    # exit 2+ on real failures (corrupt patch, hunk drift after an emsdk
    # bump).  We distinguish those exit codes so a genuine patch failure
    # aborts the build instead of silently leaving emsdk half-patched.
    cd upstream/emscripten
    patch_status=0
    cat ../../../patches/*.patch | patch -p1 --forward || patch_status=$?
    case "$patch_status" in
        0) ;;  # all patches applied cleanly
        1) echo "  (some Pyodide emscripten patches were already applied — ok on re-run)" ;;
        *)
            echo "ERROR: applying Pyodide emscripten patches failed (patch exit $patch_status)"
            find . -name '*.rej' -print 2>/dev/null || true
            exit 1
            ;;
    esac
    cd ../..

    # Activate WITHOUT ccache (--embedded enables ccache which fails to build).
    # Only mark emsdk as complete AFTER every step in this block has succeeded,
    # so a future run re-enters the block if anything above errored.
    ./emsdk activate --build=Release "$PYODIDE_EMCC_VERSION"
    touch .complete
    cd "$PYODIDE_SRC"
fi

# Source Pyodide's emsdk. Do not swallow activate errors: if it prints a
# warning or fails, we want to see it, and if PATH / EMSDK / LLVM_ROOT don't
# get set, the `command -v emcc` check below is the last line of defense.
# shellcheck disable=SC1091
source emsdk/emsdk/emsdk_env.sh

if ! command -v emcc &>/dev/null; then
    echo "ERROR: emcc not found after emsdk setup."
    exit 1
fi
echo "Using emcc: $(emcc --version | head -1)"

# Pyodide's Makefiles use sed -i (GNU style). macOS sed is different.
if command -v gsed &>/dev/null; then
    export SED=gsed
else
    export SED=sed
fi

# ── Build the wasmsh-pyodide runtime staticlib ──────────────
# We link a single staticlib (`libwasmsh_pyodide.a`) into the Pyodide wasm.
# The standalone `wasmsh-pyodide-probe` crate (exercised by
# `e2e/build-contract`) also builds a staticlib, but only one may be linked
# into the production wasm: with `MAIN_MODULE=1` emscripten passes
# `--whole-archive` to the linker and any transitive dep shared by two
# staticlibs (here `wasmsh-protocol` → `serde_core`) becomes a
# duplicate-symbol link error.  The probe's three C ABI helpers are
# therefore compiled into `wasmsh-pyodide` itself — see
# `crates/wasmsh-pyodide/src/probe.rs`.
#
# `-C symbol-mangling-version=v0` re-mangles crates compiled from source by
# this cargo invocation (our workspace crates and from-source deps such as
# `serde`, `serde_json`).  Without it, the legacy mangling produces names
# containing `$` (e.g.
# `_ZN68_$LT$serde_json..read..StrRead$u20$as$u20$...`) which then land in
# the wasm and fail when em++ tries to emit JS bindings for them.
# v0 mangling uses only `[A-Za-z0-9_]` so these names become JS-safe.
#
# Note that precompiled Rust `std` (shipped via rustup) keeps the legacy
# mangling regardless of this flag and still produces `$`-bearing symbols
# from its inlined generics.  Those are filtered separately by the
# emscripten.py patch further down in this script.
echo "Building wasmsh-pyodide runtime staticlib with Pyodide's emsdk..."
# `rustup target add` is idempotent: with the target already installed it
# exits 0 even when offline (no network call is made).  `|| true` therefore
# only matters when the target is missing AND rustup itself fails — we keep
# it so a transient registry hiccup doesn't abort an otherwise-cached build.
# stderr is intentionally NOT redirected so real failures stay visible in
# the log; the `cargo build` below is the actual gate and will fail loudly
# if the target wasn't installed.
rustup target add wasm32-unknown-emscripten || true

PYODIDE_BUILD="$SCRIPT_DIR/pyodide-build"
RUSTFLAGS="-C symbol-mangling-version=v0 ${RUSTFLAGS:-}" \
CARGO_TARGET_DIR="$PYODIDE_BUILD" \
cargo build \
    --manifest-path "$REPO_ROOT/crates/wasmsh-pyodide/Cargo.toml" \
    --target wasm32-unknown-emscripten \
    --release

RUNTIME_LIB="$PYODIDE_BUILD/wasm32-unknown-emscripten/release/libwasmsh_pyodide.a"
if [ ! -f "$RUNTIME_LIB" ]; then
    echo "ERROR: Runtime lib not found at $RUNTIME_LIB"
    exit 1
fi
echo "Runtime lib: $RUNTIME_LIB ($(du -h "$RUNTIME_LIB" | cut -f1))"

# We patch the Pyodide Makefile to inject $RUNTIME_LIB into the link
# command, but we cannot make it a true `make` dependency without forking
# the upstream rule.  When the wasmsh source changes, cargo correctly
# updates $RUNTIME_LIB but `make` still thinks `dist/pyodide.asm.wasm` is
# up to date and skips the relink — silently shipping a wasm that does
# not contain the new code.  Defend against that here: if the staticlib
# is newer than the wasm, force a relink by deleting the wasm output.
PYODIDE_DIST_WASM="$PYODIDE_SRC/dist/pyodide.asm.wasm"
if [ -f "$PYODIDE_DIST_WASM" ] && [ "$RUNTIME_LIB" -nt "$PYODIDE_DIST_WASM" ]; then
    echo "Runtime lib is newer than $PYODIDE_DIST_WASM — forcing relink."
    rm -f "$PYODIDE_DIST_WASM" "$PYODIDE_SRC/dist/pyodide.asm.js"
fi

# ── Patch Pyodide to link the runtime staticlib ────────────
#
# Every sed patch below uses a post-sed verification: the `check` function
# asserts that the expected marker text is actually present in the target
# file after the sed runs.  If the sed silently no-op'ed (because upstream
# Pyodide shifted whitespace, renamed a variable, reordered a comma list,
# …) the verification fires and the build aborts — instead of producing
# a "successful" wasm that's missing required exports or runtime methods.
echo "Patching Pyodide Makefile..."

check() {
    # check <file> <marker_regex> <what_we_tried_to_do>
    local file="$1" marker="$2" label="$3"
    if ! grep -Eq "$marker" "$file"; then
        echo "ERROR: $label — expected marker /$marker/ not found in $file after patch."
        echo "       Upstream Pyodide likely changed the line we target; see tools/pyodide/versions.env."
        exit 1
    fi
}

# Add the runtime staticlib to the Pyodide link command.  Guard on the
# full path substring (unique to our substitution) rather than a short
# token that could collide with unrelated changes.
if ! grep -qF "$RUNTIME_LIB" Makefile; then
    "$SED" -i "s|\$(CXX) -o dist/pyodide.asm.js -lpyodide src/core/main.o|\$(CXX) -o dist/pyodide.asm.js -lpyodide src/core/main.o $RUNTIME_LIB|" Makefile
    check Makefile "$(printf '%s' "$RUNTIME_LIB" | sed 's/[][\\.*^$]/\\&/g')" \
        "patching Pyodide link command to include $RUNTIME_LIB"
    echo "  Patched link command to include the wasmsh runtime lib."
else
    echo "  Link command already patched."
fi

# Add ccall/cwrap to EXPORTED_RUNTIME_METHODS for the MAIN_MODULE build.
# Guard on the exact patched form — a loose substring like "ccall" may
# match unrelated Pyodide code and fail open.
if ! grep -qF "wasmTable,ERRNO_CODES,ccall,cwrap" Makefile.envs; then
    "$SED" -i "s|-sEXPORTED_RUNTIME_METHODS='wasmTable,ERRNO_CODES'|-sEXPORTED_RUNTIME_METHODS='wasmTable,ERRNO_CODES,ccall,cwrap'|" Makefile.envs
    check Makefile.envs "wasmTable,ERRNO_CODES,ccall,cwrap" \
        "adding ccall,cwrap to EXPORTED_RUNTIME_METHODS"
    echo "  Added ccall,cwrap to EXPORTED_RUNTIME_METHODS."
else
    echo "  ccall,cwrap already in EXPORTED_RUNTIME_METHODS."
fi

# We keep `MAIN_MODULE=1` (the Pyodide upstream default) so that compiled
# side modules (numpy, pandas, scipy, DuckDB, …) can resolve CPython /
# libc / libstdc++ symbols against the main module via wasm dynamic
# linking. The `MAIN_MODULE=2` alternative requires an explicit export
# list, which is brittle to maintain across Pyodide upgrades and was the
# root cause of the v0.5.7 regression.
#
# MAIN_MODULE=1 has one Rust-specific footgun: emscripten then tries to
# emit JS bindings for every linked symbol, including Rust legacy-mangled
# names containing `$` and `..`. Two mitigations work together:
#   (1) `-C symbol-mangling-version=v0` above — covers our crates and
#       from-source deps.
#   (2) The `emscripten.py` patch below — covers precompiled std.

# Add probe + runtime symbols to EXPORTED_FUNCTIONS so they survive
# tree-shaking and are reachable via ccall.  MAIN_MODULE=1 also exports
# everything else automatically.
#
# Guard on the LAST symbol added (`_wasmsh_runtime_free_string`) so that
# a future sed edit which drops a symbol from the list is detected — if
# we grepped for the first symbol, a partial substitution would still pass.
if ! grep -qF "_wasmsh_runtime_free_string" Makefile.envs; then
    "$SED" -i 's|EXPORTS=_main|EXPORTS=_main \\\n   ,_wasmsh_probe_version \\\n   ,_wasmsh_probe_write_text \\\n   ,_wasmsh_probe_file_equals \\\n   ,_wasmsh_runtime_new \\\n   ,_wasmsh_runtime_handle_json \\\n   ,_wasmsh_runtime_free \\\n   ,_wasmsh_runtime_free_string|' Makefile.envs
    for sym in _wasmsh_probe_version _wasmsh_probe_write_text _wasmsh_probe_file_equals \
               _wasmsh_runtime_new _wasmsh_runtime_handle_json _wasmsh_runtime_free \
               _wasmsh_runtime_free_string; do
        check Makefile.envs "$sym" "adding $sym to EXPORTS"
    done
    echo "  Added wasmsh_probe_* and wasmsh_runtime_* to EXPORTS."
else
    echo "  wasmsh_probe_* and wasmsh_runtime_* already in EXPORTS."
fi

# Patch upstream emscripten to filter out exports whose names are not valid
# JS identifiers, instead of erroring out. Rust's precompiled std library
# uses legacy mangling that produces symbol names containing `$` and `..`
# (e.g. `_ZN72_$LT$$RF$str$u20$as$u20$alloc..ffi..c_str...$E`), which em++
# rejects when generating Module bindings. The patched code drops them from
# the JS bindings list while leaving them in the wasm exports table — so
# dynamic linking from compiled side modules can still resolve them, while
# the JS glue stays valid.
#
# Two call sites in emscripten.py need the same treatment:
#   1. `unexpected_exports` — controls the auto-exported user-symbol block
#      (keepalive / EMSCRIPTEN_KEEPALIVE), and is where emscripten errors
#      out on invalid identifiers today.
#   2. `function_exports` / `global_exports` — feed `make_export_wrappers`
#      and `create_receiving`, which emit `var __ZN..$..E = wasmExports[..]`
#      declarations that later fail parsing in `acorn-optimizer.mjs` with
#      "Unexpected token" if any symbol has invalid-identifier bytes.
#
# Idempotency: each patch is applied independently with a per-patch
# "already applied / needs applying / block missing" tri-state, so a
# half-patched file (e.g. patch 1 landed from an older version of this
# script) still gets patch 2 on the next run. The alternative — a single
# bash-level grep guard — was the root cause of the v0.5.8 release bug.
export EMSCRIPTEN_PY="$PYODIDE_SRC/emsdk/emsdk/upstream/emscripten/tools/emscripten.py"
python3 - <<'PYEOF'
import os, pathlib, sys
p = pathlib.Path(os.environ["EMSCRIPTEN_PY"])
src = p.read_text()
original = src

# Patch 1: replace the `unexpected_exports` exit-with-error block.
old1 = (
    "  # Rust side modules may have exported symbols that are not valid\n"
    "  # identifiers. They are meant to be called from native code in the main\n"
    "  # module not from JavaScript anyways, so don't perform this check on them.\n"
    "  if not settings.SIDE_MODULE:\n"
    "    for n in unexpected_exports:\n"
    "      if not n.isidentifier():\n"
    "        exit_with_error(f'invalid export name: {n}')\n"
)
new1 = (
    "  # wasmsh patch: filter invalid identifier exports instead of erroring.\n"
    "  # Rust side/main modules link std library code using legacy mangling\n"
    "  # which produces symbol names containing '$' and '..'. Such symbols\n"
    "  # are meant to be reachable via wasm dynamic linking, not JavaScript,\n"
    "  # so we drop them from the JS bindings list while leaving them in the\n"
    "  # wasm exports table.\n"
    "  unexpected_exports = [e for e in unexpected_exports if e.isidentifier()]\n"
)

# Patch 2: filter function_exports + global_exports right after they are
# initialized from metadata, before they feed the JS-side wrapper generators.
old2 = (
    "  else:\n"
    "    function_exports = metadata.function_exports\n"
    "    tag_exports = metadata.tag_exports\n"
    "    global_exports = metadata.global_exports\n"
)
new2 = (
    "  else:\n"
    "    function_exports = metadata.function_exports\n"
    "    tag_exports = metadata.tag_exports\n"
    "    global_exports = metadata.global_exports\n"
    "\n"
    "  # wasmsh patch: drop wasm function/global exports whose names are not\n"
    "  # valid JS identifiers (legacy Rust mangling produces '$' and '..').\n"
    "  # They stay in the wasm exports table for native dynamic linking but\n"
    "  # are skipped by make_export_wrappers / create_receiving so the\n"
    "  # generated pyodide.asm.js parses cleanly with acorn-optimizer.\n"
    "  function_exports = {k: v for k, v in function_exports.items() if k.isidentifier()}\n"
    "  global_exports = {k: v for k, v in global_exports.items() if k.isidentifier()}\n"
)

def apply_patch(label, src, old, new):
    """Per-patch tri-state: apply / already applied / raise.

    Returns (new_src, status) where status is 'applied' or 'skipped'.
    Raises SystemExit if neither old nor new is present (unexpected upstream).
    """
    if new in src:
        return src, "skipped"
    if old in src:
        return src.replace(old, new), "applied"
    raise SystemExit(
        f"ERROR: {p}: patch {label!r} could not find its anchor block "
        f"(neither pre- nor post-patch form present). "
        f"Likely emsdk version mismatch — see tools/pyodide/versions.env."
    )

src, status1 = apply_patch("unexpected_exports", src, old1, new1)
src, status2 = apply_patch("function_exports", src, old2, new2)

if src != original:
    p.write_text(src)

print(f"  emscripten.py patches: unexpected_exports={status1}, function_exports={status2}.")
PYEOF
# Invalidate Python bytecode cache so the patched module is reloaded
# (runs unconditionally — stale .pyc would defeat the live patch).
find "$PYODIDE_SRC/emsdk/emsdk/upstream/emscripten/tools" \
    -name '__pycache__' -type d -exec rm -rf {} +

# Add runtime methods needed by the FS test harness.
# Must run AFTER the ccall/cwrap patch above.  Guard on the full patched
# marker (last symbol in the comma list) so partial substitution is
# detected and so we don't match an unrelated stringToNewUTF8 occurrence
# elsewhere in Makefile.envs.
if ! grep -qF "ccall,cwrap,stringToNewUTF8,UTF8ToString,callMain,FS" Makefile.envs; then
    "$SED" -i "s|ccall,cwrap'|ccall,cwrap,stringToNewUTF8,UTF8ToString,callMain,FS'|" Makefile.envs
    check Makefile.envs "ccall,cwrap,stringToNewUTF8,UTF8ToString,callMain,FS" \
        "adding FS harness runtime methods"
    echo "  Added stringToNewUTF8,UTF8ToString,callMain,FS to EXPORTED_RUNTIME_METHODS."
else
    echo "  FS harness runtime methods already in EXPORTED_RUNTIME_METHODS."
fi

# Fix libffi autoreconf on modern Linux.
# libffi's configure.ac uses LT_SYS_SYMBOL_USCORE removed in libtool 2.5+.
# Wrapper patches configure.ac before running real autoreconf.
WRAPPER_DIR="$PYODIDE_SRC/.bin-wrappers"
mkdir -p "$WRAPPER_DIR"
# Find the real autoreconf path before shadowing it.
REAL_AUTORECONF="$(which autoreconf 2>/dev/null || echo /usr/bin/autoreconf)"
cat > "$WRAPPER_DIR/autoreconf" << ENDWRAPPER
#!/bin/sh
if [ -f configure.ac ] && grep -q LT_SYS_SYMBOL_USCORE configure.ac; then
    sed -i '/LT_SYS_SYMBOL_USCORE/d' configure.ac
fi
exec "$REAL_AUTORECONF" "\$@"
ENDWRAPPER
chmod +x "$WRAPPER_DIR/autoreconf"
export PATH="$WRAPPER_DIR:$PATH"
echo "  Installed autoreconf wrapper to patch libffi configure.ac."

PYODIDE_CDN="https://cdn.jsdelivr.net/pyodide/v${PYODIDE_VERSION}/full"

# ── Build CPython + core ────────────────────────────────────
echo "Building Pyodide core (this takes a while on first run)..."

# Install Python build deps into a venv
if [ ! -d ".venv" ]; then
    python3 -m venv .venv
    .venv/bin/pip install --quiet pyodide-build
fi
export PATH="$PYODIDE_SRC/.venv/bin:$PATH"

# Build the core module + stdlib + JS loader
make dist/pyodide.asm.js dist/python_stdlib.zip dist/pyodide.js

# ── Assemble the custom distribution ───────────────────────
echo "Assembling custom distribution..."
mkdir -p "$DIST_DIR"

# Copy the core files needed for Node loading
cp dist/pyodide.asm.js "$DIST_DIR/"
cp dist/pyodide.asm.wasm "$DIST_DIR/"

# NOTE: Deno compat is handled at runtime in node-module.mjs by pre-loading
# pyodide.asm.js via createRequire() before loadPyodide runs. No build-time
# patching of ENVIRONMENT_IS_NODE is needed.

# Copy the JS loader + stdlib + generated pyodide.asm.js sibling files.
# pyodide.js, pyodide.mjs and python_stdlib.zip are load-bearing — the
# runtime does not boot without them — so we fail loudly if any are
# missing. package.json / pyodide-lock.json / repodata.json are
# conditional (some Pyodide versions don't emit repodata.json at all),
# so we log which optional files were skipped instead of exiting.
REQUIRED_DIST_FILES=(
    dist/pyodide.js
    dist/pyodide.mjs
    dist/python_stdlib.zip
)
OPTIONAL_DIST_FILES=(
    dist/package.json
    dist/pyodide-lock.json
    dist/repodata.json
)
for f in "${REQUIRED_DIST_FILES[@]}"; do
    if [ ! -f "$f" ]; then
        echo "ERROR: required Pyodide build output missing: $f"
        exit 1
    fi
    cp "$f" "$DIST_DIR/"
done
for f in "${OPTIONAL_DIST_FILES[@]}"; do
    if [ -f "$f" ]; then
        cp "$f" "$DIST_DIR/"
    else
        echo "  (optional file not produced by Pyodide make: $f — skipping)"
    fi
done

# ── Fetch micropip + packaging + lockfile from Pyodide CDN ─────
# micropip is tagged "always" in standard Pyodide — every sandbox
# should have it available via `import micropip` out of the box.
#
# Use `curl -fsSL`:
#   -f → fail on HTTP 4xx/5xx (without this, an error page HTML body gets
#        written to the output file as if it were the real artifact).
#   -s → silent progress.
#   -S → still print errors on failure.
#   -L → follow redirects (jsDelivr redirects to its CDN edge).

download_required() {
    # download_required <url> <dest>
    local url="$1" dest="$2"
    if [ -f "$dest" ]; then
        return 0
    fi
    echo "  Downloading $url"
    curl -fsSL "$url" -o "$dest" || {
        echo "ERROR: failed to download $url"
        rm -f "$dest"
        exit 1
    }
}

echo "Fetching pyodide-lock.json from CDN..."
download_required "$PYODIDE_CDN/pyodide-lock.json" "$DIST_DIR/pyodide-lock.json"
# Validate the downloaded file is actually JSON (curl -f catches HTTP
# errors, but a CDN may still serve a truncated / wrong-content-type body).
python3 -c "
import json, sys
try:
    with open('$DIST_DIR/pyodide-lock.json') as f:
        data = json.load(f)
    assert 'packages' in data and isinstance(data['packages'], dict), 'missing packages dict'
    print(f'  Lockfile contains {len(data[\"packages\"])} package entries.')
except Exception as e:
    print(f'ERROR: $DIST_DIR/pyodide-lock.json is not a valid Pyodide lockfile: {e}', file=sys.stderr)
    sys.exit(1)
"

echo "Fetching micropip + packaging + sqlite3 wheels from CDN..."
# micropip + packaging are hard requirements — the sandbox cannot run
# `pip install` without micropip, and micropip pulls in `packaging`
# transitively.  sqlite3 is a cpython_module in Pyodide 0.26+ — the
# standard library entry is a shim that calls `loadPackage("sqlite3")`
# at first import, so we must bundle the wheel to keep the sandbox
# offline-capable.
for whl_name in micropip packaging sqlite3; do
    whl_file=$(python3 -c "
import json, sys
lock = json.load(open('$DIST_DIR/pyodide-lock.json'))
pkg = lock['packages'].get('$whl_name')
if not pkg:
    print(f'ERROR: \"$whl_name\" missing from pyodide-lock.json', file=sys.stderr)
    sys.exit(1)
print(pkg['file_name'])
")
    download_required "$PYODIDE_CDN/$whl_file" "$DIST_DIR/$whl_file"
    # Sanity-check: a wheel must be at least a few KB. Anything smaller is
    # a truncated download or an HTML error body that slipped through.
    whl_size=$(wc -c < "$DIST_DIR/$whl_file")
    if [ "$whl_size" -lt 1024 ]; then
        echo "ERROR: $whl_file is suspiciously small (${whl_size} bytes) — likely truncated."
        exit 1
    fi
done

echo "=== Custom Pyodide build complete ==="
echo "Distribution: $DIST_DIR"
ls -lh "$DIST_DIR/"

# node is required to package the runtime assets for the npm + Python
# consumers.  Every supported build environment (CI, dev laptop, docker
# image) already has node because Pyodide's own build depends on it.
# Fail loudly rather than shipping stale / empty assets.
if ! command -v node >/dev/null 2>&1; then
    echo "ERROR: node not found on PATH — cannot package runtime assets for npm/Python consumers."
    exit 1
fi
echo "Packaging runtime assets for npm and Python consumers..."
node "$SCRIPT_DIR/package-runtime-assets.mjs"
