#!/usr/bin/env bash
#
# Build the no-JS same-module Pyodide-WASI artifact for Wasmtime.
#
# Output:
#   dist/pyodide-wasi/wasmsh_pyodide_wasi.wasm
#   dist/pyodide-wasi/manifest.json
#
# This is a build path parallel to build-custom.sh. Both share the same
# Pyodide source, CPython static library, and wasmsh Rust crate. This path
# links them into a standalone WASI-compatible Wasm module intended for
# direct instantiation by Wasmtime (no JS glue, no Pyodide JS bridge).
#
# Prerequisites:
#   - The standard Pyodide build must have been run first (build-custom.sh)
#     so that pyodide-src/cpython/installs/ and pyodide-build/ exist.
#   - Alternatively, set PYODIDE_CPYTHON_DIR / WASMSH_PYODIDE_LIB to point
#     at pre-built artifacts.
#
# Environment:
#   SKIP_PYODIDE_WASI=1   Skip entirely
#   WASI_CLEAN=1          Force a full rebuild
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
# shellcheck source=versions.env
source "$SCRIPT_DIR/versions.env"

if [ "${SKIP_PYODIDE_WASI:-0}" = "1" ]; then
    echo "SKIP_PYODIDE_WASI=1 — skipping"
    exit 0
fi

export PATH="$HOME/.cargo/bin:$(rustc --print sysroot 2>/dev/null)/bin:$PATH"

DIST_DIR="$REPO_ROOT/dist/pyodide-wasi"
BUILD_DIR="$SCRIPT_DIR/wasi-build"
PYODIDE_SRC="$SCRIPT_DIR/pyodide-src"

echo "=== wasmsh Pyodide WASI build ==="
echo "Pyodide version: $PYODIDE_VERSION"
echo "Emscripten ver:  $EMSCRIPTEN_VERSION"

if [ "${WASI_CLEAN:-0}" = "1" ] && [ -d "$BUILD_DIR" ]; then
    echo "WASI_CLEAN=1 — removing build dir"
    rm -rf "$BUILD_DIR" "$DIST_DIR"
fi

mkdir -p "$BUILD_DIR" "$DIST_DIR"

# ── Verify prerequisites ─────────────────────────────────────

# CPython static library from the Pyodide build.
CPYTHON_VERSION_SHORT="3.13"
CPYTHON_INSTALL="${PYODIDE_CPYTHON_DIR:-$PYODIDE_SRC/cpython/installs/python-${CPYTHON_VERSION_SHORT}.*}"
# Glob to find the actual version directory.
CPYTHON_INSTALL=$(echo $CPYTHON_INSTALL)
if [ ! -d "$CPYTHON_INSTALL" ]; then
    echo "ERROR: CPython install not found. Run build-custom.sh first."
    echo "  Looked for: $PYODIDE_SRC/cpython/installs/python-${CPYTHON_VERSION_SHORT}.*"
    exit 1
fi
# Use the trampoline-free variant for standalone builds.
# CPython's PY_CALL_TRAMPOLINE uses call_indirect with type inspection
# which doesn't work in standalone wasm (Wasmtime enforces exact type
# matching). We strip emscripten_trampoline.o from the archive so
# CPython dispatches C function calls directly.
CPYTHON_LIB_ORIG="$CPYTHON_INSTALL/lib/libpython${CPYTHON_VERSION_SHORT}.a"
CPYTHON_LIB="$BUILD_DIR/libpython${CPYTHON_VERSION_SHORT}_no_trampoline.a"
if [ ! -f "$CPYTHON_LIB" ] || [ "${WASI_CLEAN:-0}" = "1" ]; then
    cp "$CPYTHON_LIB_ORIG" "$CPYTHON_LIB"
    LLVM_AR="$PYODIDE_SRC/emsdk/emsdk/upstream/bin/llvm-ar"
    "$LLVM_AR" d "$CPYTHON_LIB" emscripten_trampoline.o 2>/dev/null || true
    echo "Stripped PY_CALL_TRAMPOLINE from CPython archive."
fi
CPYTHON_INCLUDE="$CPYTHON_INSTALL/include/python${CPYTHON_VERSION_SHORT}"
if [ ! -f "$CPYTHON_LIB" ]; then
    echo "ERROR: libpython${CPYTHON_VERSION_SHORT}.a not found at $CPYTHON_LIB"
    exit 1
fi
echo "CPython lib:     $CPYTHON_LIB ($(du -h "$CPYTHON_LIB" | cut -f1))"

# Python stdlib for embedding.
PYTHON_STDLIB="$CPYTHON_INSTALL/lib/python${CPYTHON_VERSION_SHORT}"
if [ ! -d "$PYTHON_STDLIB" ]; then
    echo "ERROR: Python stdlib not found at $PYTHON_STDLIB"
    exit 1
fi

# libffi from the Pyodide build.
LIBFFI="$CPYTHON_INSTALL/lib/libffi.a"
# libffi is optional for the standalone path (ctypes may not work).
LIBFFI_FLAG=""
if [ -f "$LIBFFI" ]; then
    LIBFFI_FLAG="$LIBFFI"
    echo "libffi:          $LIBFFI"
fi

# wasmsh-pyodide staticlib.
PYODIDE_BUILD="${WASMSH_PYODIDE_BUILD:-$SCRIPT_DIR/pyodide-build}"
RUNTIME_LIB="${WASMSH_PYODIDE_LIB:-$PYODIDE_BUILD/wasm32-unknown-emscripten/release/libwasmsh_pyodide.a}"
if [ ! -f "$RUNTIME_LIB" ]; then
    echo "Building wasmsh-pyodide staticlib..."
    rustup target add wasm32-unknown-emscripten || true
    RUSTFLAGS="-C symbol-mangling-version=v0 ${RUSTFLAGS:-}" \
    CARGO_TARGET_DIR="$PYODIDE_BUILD" \
    cargo build \
        --manifest-path "$REPO_ROOT/crates/wasmsh-pyodide/Cargo.toml" \
        --target wasm32-unknown-emscripten \
        --release
fi
if [ ! -f "$RUNTIME_LIB" ]; then
    echo "ERROR: wasmsh-pyodide staticlib not found at $RUNTIME_LIB"
    exit 1
fi
echo "Runtime lib:     $RUNTIME_LIB ($(du -h "$RUNTIME_LIB" | cut -f1))"

# ── Set up Emscripten ─────────────────────────────────────────

# Use Pyodide's bundled emsdk (version-locked to match the CPython build).
EMSDK_ENV="$PYODIDE_SRC/emsdk/emsdk/emsdk_env.sh"
if [ ! -f "$EMSDK_ENV" ]; then
    echo "ERROR: Pyodide's emsdk not found. Run build-custom.sh first."
    echo "  Looked for: $EMSDK_ENV"
    exit 1
fi
# shellcheck disable=SC1090
source "$EMSDK_ENV" 2>/dev/null
if ! command -v emcc &>/dev/null; then
    echo "ERROR: emcc not found after sourcing emsdk_env.sh"
    exit 1
fi
echo "Using emcc:      $(emcc --version | head -1)"

# ── Generate the C shim ──────────────────────────────────────

# Provide stubs for symbols that the wasmsh-pyodide Rust code expects
# from the JS host. In the standalone path these are no-ops.
SHIM_C="$BUILD_DIR/standalone_shim.c"
cat > "$SHIM_C" << 'SHIM_EOF'
/*
 * Standalone shim for the no-JS Pyodide artifact.
 *
 * Provides stubs for extern functions that the wasmsh-pyodide Rust code
 * expects from the JS host (browser-worker.js or node-host.mjs). In the
 * standalone Wasmtime path, these either return error indicators or are
 * no-ops.
 */
#include <stdlib.h>
#include <string.h>
#include <stdint.h>

/* wasmsh_js_http_fetch: network backend stub.
 * Returns a JSON error response allocated with malloc (caller frees). */
/* Entry point — the standalone artifact is a reactor, not a command.
 * The Wasmtime host calls exports directly; main() is unused. */
int main(void) { return 0; }

/* Host import for HTTP fetch — implemented by the Wasmtime runner.
 * Parameters are wasm pointers to null-terminated C strings + body bytes.
 * Returns a wasm pointer to a malloc'd JSON response string.
 * If the host import is not provided (stub), returns 0. */
extern int __wasmsh_host_fetch(
    const char* url,
    const char* method,
    const char* headers_json,
    const unsigned char* body,
    unsigned int body_len,
    int follow_redirects
) __attribute__((import_module("env"), import_name("__wasmsh_host_fetch")));

/* wasmsh_js_http_fetch: routes through the host import.
 * Returns a JSON response allocated with malloc (caller frees). */
char* wasmsh_js_http_fetch(
    const char* url,
    const char* method,
    const char* headers_json,
    const unsigned char* body,
    unsigned int body_len,
    int follow_redirects
) {
    int ptr = __wasmsh_host_fetch(url, method, headers_json,
                                  body, body_len, follow_redirects);
    if (ptr != 0) return (char*)(uintptr_t)ptr;
    /* Fallback if host import returned null. */
    const char* err = "{\"error\":\"network not available in standalone mode\"}";
    size_t len = strlen(err);
    char* result = (char*)malloc(len + 1);
    if (result) memcpy(result, err, len + 1);
    return result;
}

/* Pyodide/CPython-specific stubs for the standalone path. */
void* py_jsnull = 0;
int _Py_emscripten_runtime(void) { return 0; }
int _emscripten_system(const char* cmd) { (void)cmd; return -1; }

/* CPython trampoline bypass for the standalone path.
 *
 * In the Pyodide/JS path, CPython uses PY_CALL_TRAMPOLINE to dispatch
 * C function calls through a trampoline that inspects function types
 * via JS's wasmTable.get(). This doesn't work in standalone wasm because
 * call_indirect enforces exact type matching.
 *
 * Instead of trying to make the trampoline work, we override
 * _PyEM_TrampolineCall itself (the C function, not the JS fallback).
 * Since the shim is an object file and CPython is an archive, the linker
 * prefers the shim's definition.
 *
 * PyCFunctionWithKeywords always takes (self, args, kw) → 3 params.
 * For METH_NOARGS/METH_O (2 params), we cast to the 2-param type.
 * The trampoline decides which based on the method flags, which we
 * access through the PyCFunction API.
 */
typedef void* PyObject;
typedef PyObject* (*PyCFunctionWithKeywords)(PyObject*, PyObject*, PyObject*);
typedef int (*CountArgsFunc)(PyCFunctionWithKeywords);

/* _PyEM_TrampolineCall: dispatches C function calls by argument count.
 * The count_args function pointer is set by the Wasmtime runner during
 * setup_trampoline. If count_args is 0 (not set up), falls back to 3 args. */
extern char _PyRuntime[];
typedef PyObject* (*Fn0)(void);
typedef PyObject* (*Fn1)(PyObject*);
typedef PyObject* (*Fn2)(PyObject*, PyObject*);
typedef PyObject* (*Fn4)(PyObject*, PyObject*, PyObject*, PyObject*);
typedef void (*Vd1)(PyObject*);
typedef void (*Vd2)(PyObject*, PyObject*);

PyObject* _PyEM_TrampolineCall(
    PyCFunctionWithKeywords func,
    PyObject* self, PyObject* args, PyObject* kw
) {
    int offset = 8952; /* offsetof(_PyRuntimeState, emscripten_count_args_function) */
    CountArgsFunc count_args = *(CountArgsFunc*)(_PyRuntime + offset);

    if (count_args == 0)
        return func(self, args, kw);

    switch (count_args(func)) {
        case 0: return ((Fn0)func)();
        case 1: return ((Fn1)func)(self);
        case 2: return ((Fn2)func)(self, args);
        case 3: return func(self, args, kw);
        case 4: return ((Fn4)func)(self, args, kw, (PyObject*)0);
        /* Void-returning variants (10+N): */
        case 11: ((Vd1)func)(self); return (PyObject*)0;
        case 12: ((Vd2)func)(self, args); return (PyObject*)0;
        default: return func(self, args, kw);
    }
}

typedef struct { int placeholder; } _PyRuntimeState;
void _Py_EmscriptenTrampoline_Init(_PyRuntimeState* rt) { (void)rt; }

PyObject* _PyEM_TrampolineCall_JS(
    PyCFunctionWithKeywords func,
    PyObject* arg1, PyObject* arg2, PyObject* arg3
) {
    return func(arg1, arg2, arg3);
}
CountArgsFunc _PyEM_GetCountArgsPtr(void) { return (CountArgsFunc)0; }
void _PyEM_InitTrampoline_js(void) {}
/* This must match offsetof(_PyRuntimeState, emscripten_count_args_function)
 * as compiled by this CPython build. The value 8952 was extracted from the
 * original emscripten_trampoline.o in the CPython 3.13.2 archive. */
const int _PyEM_EMSCRIPTEN_COUNT_ARGS_OFFSET = 8952;

/* libffi JS stubs — ctypes is not functional in standalone mode. */
void ffi_call_js(void) {}
void ffi_closure_alloc_js(void) {}
void ffi_closure_free_js(void) {}
void ffi_prep_closure_loc_js(void) {}

/* msync stub */
int _msync_js(int addr, int len, int prot, int flags, int fd, long long off) {
    (void)addr; (void)len; (void)prot; (void)flags; (void)fd; (void)off;
    return 0;
}

/* Load embedded file data into the in-memory filesystem (memfs.c).
 * Called from a constructor during _initialize. Data format: packed
 * array of { char* name, size_t len, void* content } terminated by null. */
extern void memfs_mkdir(const char* path);
extern void memfs_create_file(const char* path, const unsigned char* data, unsigned int len);

static void _ensure_dirs(const char* path) {
    char buf[512];
    size_t len = strlen(path);
    if (len >= sizeof(buf)) return;
    memcpy(buf, path, len + 1);
    /* Find last slash to get parent path. */
    for (int i = (int)len - 1; i > 0; i--) {
        if (buf[i] == '/') { buf[i] = '\0'; break; }
    }
    /* Create each component. */
    for (int i = 1; buf[i]; i++) {
        if (buf[i] == '/') {
            buf[i] = '\0';
            memfs_mkdir(buf);
            buf[i] = '/';
        }
    }
    memfs_mkdir(buf);
}

void _emscripten_fs_load_embedded_files(void* ptr) {
    unsigned char* p = (unsigned char*)ptr;
    while (1) {
        unsigned int name_addr = *(unsigned int*)p; p += 4;
        if (name_addr == 0) break;
        unsigned int len = *(unsigned int*)p; p += 4;
        unsigned int data_addr = *(unsigned int*)p; p += 4;
        const char* name = (const char*)(uintptr_t)name_addr;
        const unsigned char* data = (const unsigned char*)(uintptr_t)data_addr;
        _ensure_dirs(name);
        memfs_create_file(name, data, len);
    }
}
SHIM_EOF

echo "Generated standalone shim: $SHIM_C"

# ── Link the standalone artifact ──────────────────────────────

ARTIFACT="$DIST_DIR/wasmsh_pyodide_wasi.wasm"

# Exported functions (Emscripten convention: C names with _ prefix in the
# EXPORTED_FUNCTIONS list, but the wasm export strips the prefix).
EXPORTED_FUNCTIONS='["_malloc","_free","_wasmsh_pyodide_boot","_wasmsh_runtime_new","_wasmsh_runtime_handle_json","_wasmsh_runtime_free","_wasmsh_runtime_free_string","_wasmsh_probe_version","_wasmsh_probe_write_text","_wasmsh_probe_file_equals","__PyRuntime","__PyEM_EMSCRIPTEN_COUNT_ARGS_OFFSET"]'

# Prepare embedded Python stdlib. Files are embedded into the wasm data
# section via --embed-file. During _initialize, our C implementation of
# _emscripten_fs_load_embedded_files loads them into the in-memory memfs.
STDLIB_EMBED="$BUILD_DIR/python_stdlib"
if [ ! -d "$STDLIB_EMBED/lib/python${CPYTHON_VERSION_SHORT}" ] || [ "${WASI_CLEAN:-0}" = "1" ]; then
    echo "Preparing embedded Python stdlib..."
    rm -rf "$STDLIB_EMBED"
    mkdir -p "$STDLIB_EMBED/lib/python${CPYTHON_VERSION_SHORT}"
    rsync -a \
        --exclude='test/' --exclude='tests/' --exclude='__pycache__/' \
        --exclude='tkinter/' --exclude='turtle*' --exclude='idlelib/' \
        --exclude='ensurepip/' --exclude='turtledemo/' \
        --exclude='*.pyc' --exclude='*.pyo' \
        "$PYTHON_STDLIB/" "$STDLIB_EMBED/lib/python${CPYTHON_VERSION_SHORT}/"

    # Embed micropip + packaging for standalone package installation.
    echo "  Installing micropip + packaging..."
    MICROPIP_TMP="$BUILD_DIR/micropip_tmp"
    rm -rf "$MICROPIP_TMP"
    pip3 install micropip==0.11.0 packaging --target="$MICROPIP_TMP" --no-compile --quiet 2>/dev/null
    # Copy into site-packages (standard Python package location).
    SITE_PKG="$STDLIB_EMBED/lib/python${CPYTHON_VERSION_SHORT}/site-packages"
    mkdir -p "$SITE_PKG"
    cp -r "$MICROPIP_TMP/micropip" "$SITE_PKG/"
    cp -r "$MICROPIP_TMP/packaging" "$SITE_PKG/"
    rm -rf "$MICROPIP_TMP"

    # Add _micropip_sync.py: synchronous micropip.install() wrapper.
    # asyncio.run() doesn't work in standalone WASI (no sockets for event
    # loop self-pipe), so we patch asyncio.gather to run sequentially and
    # drive the coroutine via send().
    cat > "$SITE_PKG/_micropip_sync.py" << 'SYNC_EOF'
"""Synchronous micropip.install() for standalone WASI (no event loop)."""

def install(requirements, **kwargs):
    """Install packages via micropip without asyncio.run().

    Patches asyncio.gather to run coroutines sequentially, then
    drives micropip.install() as a coroutine via send().
    """
    import asyncio
    import asyncio.tasks

    # Patch asyncio primitives to work without an event loop.
    # micropip uses gather + create_task which both require a running loop.
    _orig_gather = asyncio.gather
    _orig_gather_tasks = asyncio.tasks.gather
    _orig_create_task = asyncio.create_task
    _orig_create_task_tasks = asyncio.tasks.create_task

    async def _seq_gather(*coros, return_exceptions=False):
        results = []
        for c in coros:
            try:
                results.append(await c)
            except Exception as e:
                if return_exceptions:
                    results.append(e)
                else:
                    raise
        return results

    def _fake_create_task(coro, **kw):
        """Wrap coroutine as a Task-like object that awaits directly."""
        class _FakeTask:
            def __init__(self, c): self._coro = c; self._result = None; self._done = False
            def __await__(self):
                self._result = yield from self._coro.__await__()
                self._done = True
                return self._result
        return _FakeTask(coro)

    asyncio.gather = _seq_gather
    asyncio.tasks.gather = _seq_gather
    asyncio.create_task = _fake_create_task
    asyncio.tasks.create_task = _fake_create_task

    # Patch micropip's fetch to use _wasmsh_fetch (host import) instead
    # of urllib.request.urlopen (which needs sockets).
    import _wasmsh_fetch
    import json, base64

    async def _host_fetch_bytes(url, kwargs):
        if url.startswith("emfs:"):
            path = url[len("emfs:"):]
            with open(path, "rb") as f:
                return f.read()
        resp_json = _wasmsh_fetch.fetch(url, "GET")
        resp = json.loads(resp_json)
        if "error" in resp:
            raise ValueError(f"fetch failed: {resp['error']}")
        return base64.b64decode(resp.get("body_base64", ""))

    async def _host_fetch_string_and_headers(url, kwargs):
        if url.startswith("emfs:"):
            path = url[len("emfs:"):]
            with open(path, "rb") as f:
                return f.read().decode(), {}
        resp_json = _wasmsh_fetch.fetch(url, "GET")
        resp = json.loads(resp_json)
        if "error" in resp:
            raise ValueError(f"fetch failed: {resp['error']}")
        body = base64.b64decode(resp.get("body_base64", "")).decode()
        headers = dict(resp.get("headers", []))
        return body, headers

    # Patch at EVERY import site — module-level bindings override class attrs.
    import micropip._compat
    import micropip.wheelinfo
    import micropip.package_index
    micropip._compat.fetch_bytes = _host_fetch_bytes
    micropip._compat.fetch_string_and_headers = _host_fetch_string_and_headers
    micropip.wheelinfo.fetch_bytes = _host_fetch_bytes
    micropip.package_index.fetch_string_and_headers = _host_fetch_string_and_headers

    try:
        import micropip
        coro = micropip.install(requirements, **kwargs)
        try:
            coro.send(None)
        except StopIteration:
            pass
    finally:
        asyncio.gather = _orig_gather
        asyncio.tasks.gather = _orig_gather_tasks
        asyncio.create_task = _orig_create_task
        asyncio.tasks.create_task = _orig_create_task_tasks
SYNC_EOF

    # Add _emfs_handler.py: urllib opener for emfs: URLs.
    cat > "$SITE_PKG/_emfs_handler.py" << 'EMFS_EOF'
"""Register emfs: URL handler for micropip local wheel installs."""
import io
import urllib.request
import urllib.response

class _EmfsHandler(urllib.request.BaseHandler):
    def emfs_open(self, req):
        path = req.full_url[len("emfs:"):]
        with open(path, "rb") as f:
            data = f.read()
        return urllib.response.addinfourl(
            io.BytesIO(data),
            {"content-length": str(len(data))},
            req.full_url,
        )

def install():
    urllib.request.install_opener(urllib.request.build_opener(_EmfsHandler))
EMFS_EOF
    echo "  Stdlib size: $(du -sh "$STDLIB_EMBED/lib" | cut -f1)"
fi

MEMFS_C="$SCRIPT_DIR/wasi-shims/memfs.c"
FETCH_MODULE_C="$SCRIPT_DIR/wasi-shims/wasmsh_fetch_module.c"

echo "Linking artifact..."
echo "  STANDALONE_WASM + in-wasm memfs, embedded stdlib"

ARTIFACT_RAW="$BUILD_DIR/wasmsh_pyodide_raw.wasm"

# STANDALONE_WASM=1 gives us WASI imports for stdin/stdout/stderr.
# Our memfs.c provides the filesystem (overrides weak stubs from
# the standalone library). No JS filesystem, no WASI filesystem.
emcc \
    "$SHIM_C" \
    "$MEMFS_C" \
    "$FETCH_MODULE_C" \
    "$RUNTIME_LIB" \
    "$CPYTHON_LIB" \
    $LIBFFI_FLAG \
    -o "$ARTIFACT_RAW" \
    -I"$CPYTHON_INCLUDE" \
    -s STANDALONE_WASM=1 \
    -Wl,--entry=_initialize \
    -nostartfiles \
    "$PYODIDE_SRC/emsdk/emsdk/upstream/emscripten/cache/sysroot/lib/wasm32-emscripten/crt1_reactor.o" \
    -s EXPORTED_FUNCTIONS="$EXPORTED_FUNCTIONS" \
    -s IMPORTED_MEMORY=0 \
    -s ALLOW_MEMORY_GROWTH=1 \
    -s INITIAL_MEMORY=33554432 \
    -s MAXIMUM_MEMORY=2147483648 \
    -s ERROR_ON_UNDEFINED_SYMBOLS=1 \
    -s ALLOW_TABLE_GROWTH=1 \
    -s STACK_SIZE=2097152 \
    -s SUPPORT_LONGJMP=wasm \
    -fwasm-exceptions \
    --embed-file "$STDLIB_EMBED/lib@/lib" \
    -O2 \
    -lm -lz -lbz2

# ── Convert legacy exception handling to exnref ───────────────
# Emscripten's SUPPORT_LONGJMP=wasm generates legacy try/catch
# instructions that Wasmtime's Cranelift doesn't support. Convert
# them to the standardized exnref proposal using wasm-opt.
WASM_OPT="$PYODIDE_SRC/emsdk/emsdk/upstream/bin/wasm-opt"
if [ ! -x "$WASM_OPT" ]; then
    echo "ERROR: wasm-opt not found at $WASM_OPT"
    exit 1
fi
echo "Converting legacy exception handling to exnref..."
"$WASM_OPT" \
    --enable-bulk-memory \
    --enable-exception-handling \
    --enable-nontrapping-float-to-int \
    --enable-mutable-globals \
    --enable-sign-ext \
    --enable-multivalue \
    --translate-to-exnref \
    --no-validation \
    -o "$ARTIFACT" \
    "$ARTIFACT_RAW"
rm -f "$ARTIFACT_RAW"

if [ ! -f "$ARTIFACT" ]; then
    echo "ERROR: artifact not produced at $ARTIFACT"
    exit 1
fi

ARTIFACT_SIZE=$(wc -c < "$ARTIFACT" | tr -d ' ')
echo "Artifact: $ARTIFACT ($ARTIFACT_SIZE bytes)"

# ── Generate manifest ────────────────────────────────────────

cat > "$DIST_DIR/manifest.json" << MANIFEST_EOF
{
  "artifact": "wasmsh_pyodide_wasi.wasm",
  "entryExport": "_start",
  "bootExport": "wasmsh_pyodide_boot",
  "stdlibMode": "embedded",
  "pyodideVersion": "$PYODIDE_VERSION",
  "emscriptenVersion": "$EMSCRIPTEN_VERSION",
  "cpythonVersion": "$(basename "$CPYTHON_INSTALL" | sed 's/python-//')",
  "artifactSize": $ARTIFACT_SIZE,
  "buildDate": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "notes": "Standalone WASI-compatible artifact for Wasmtime. No JS glue required."
}
MANIFEST_EOF

echo "Manifest: $DIST_DIR/manifest.json"

echo ""
echo "=== Pyodide WASI build complete ==="
echo "Output: $DIST_DIR/"
ls -lh "$DIST_DIR/"
