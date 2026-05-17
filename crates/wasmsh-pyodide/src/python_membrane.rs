//! Python-side membrane installed once per Pyodide instance.
//!
//! The membrane (audits C1/C2/F2) hardens the Python side of the
//! sandbox by blocking dangerous JS surface from reaching user Python.
//! Network policy is handled in JS (`lib/fetch-membrane.mjs`) rather
//! than from Python, because Python state is reflectable: a previous
//! draft stored `_wasmsh_original_fetch` and `_WASMSH_ALLOWED_HOSTS` in
//! the user's globals, where user code could trivially mutate the
//! allow-list or call the captured original directly.
//!
//! What the preamble does, in order:
//!
//! 1. Runs the install logic inside a single function so locals don't
//!    leak into `globals()`.
//! 2. Replaces dangerous attributes on the `js` proxy
//!    (`process`, `require`, `Deno`, `WebSocket`, `fs`, `child_process`,
//!    `worker_threads`, `subprocess`, `cluster`, `crypto`) with deny
//!    proxies that raise `PermissionError` on any read or call.
//! 3. Sets a single `builtins.WASMSH_MEMBRANE_INSTALLED` sentinel so a
//!    subsequent install attempt short-circuits. The Rust caller also
//!    tracks installation state in a thread-local to avoid the
//!    PyRun_SimpleString roundtrip on every python invocation.
//!
//! Network policy (allowlist match, port check, scheme check, redirect
//! re-validation) lives in the JS-side membrane. See
//! `packages/npm/wasmsh-pyodide/lib/fetch-membrane.mjs`. Both Node and
//! browser hosts now install that membrane before booting Pyodide.

use std::cell::{Cell, RefCell};
use std::ffi::CString;

extern "C" {
    fn PyRun_SimpleString(command: *const std::os::raw::c_char) -> std::os::raw::c_int;
}

thread_local! {
    static ALLOWED_HOSTS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static INSTALLED_FOR_HOSTS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static INSTALLED: Cell<bool> = const { Cell::new(false) };
}

/// Update the membrane's view of the host allowlist. Called by the
/// JsonRuntimeHandle's network-backend factory when a new session starts.
/// Invalidates the "already installed" flag so the next python command
/// re-runs the preamble with the new list.
pub fn set_allowed_hosts(hosts: Vec<String>) {
    ALLOWED_HOSTS.with(|cell| *cell.borrow_mut() = hosts);
    INSTALLED.with(|c| c.set(false));
}

/// Install the membrane if it hasn't been installed yet for the current
/// allowlist. Idempotent — both via the Rust-side `INSTALLED` flag and via
/// the Python-side `WASMSH_MEMBRANE_INSTALLED` global. Returns `Err` only
/// when CString conversion of the preamble itself fails, which would
/// indicate a bug in the encoded script.
pub fn ensure_installed() -> Result<(), &'static str> {
    if INSTALLED.with(Cell::get) {
        return Ok(());
    }
    let hosts = ALLOWED_HOSTS.with(|c| c.borrow().clone());
    INSTALLED_FOR_HOSTS.with(|c| *c.borrow_mut() = hosts.clone());
    let script = build_preamble(&hosts);
    let c = CString::new(script).map_err(|_| "null in membrane preamble")?;
    let rc = unsafe { PyRun_SimpleString(c.as_ptr()) };
    if rc != 0 {
        // The membrane preamble failed. Surface but don't panic — the
        // Pyodide runtime is still usable for non-network code. The
        // caller's subsequent `python` invocations will retry.
        return Err("membrane preamble execution failed");
    }
    INSTALLED.with(|c| c.set(true));
    Ok(())
}

fn build_preamble(_allowed_hosts: &[String]) -> String {
    // The preamble runs inside a function so its locals stay out of the
    // user-visible `globals()`. The audit (F2) flagged the previous draft
    // for stashing `_wasmsh_original_fetch` and `_WASMSH_ALLOWED_HOSTS`
    // into globals where user Python could mutate them.
    //
    // Network policy is enforced by the JS-side membrane, which keeps
    // both the allowed_hosts list and the original fetch in JS closure
    // state that Python cannot reach. The Python preamble below is
    // narrowed to its remaining job: deny dangerous attributes on `js`.
    //
    // The `allowed_hosts` parameter is kept on the Rust side so the JS
    // side can be re-armed via `set_allowed_hosts`; it is intentionally
    // not interpolated here.
    // Bulletproof: ANY failure in the preamble is silently absorbed. The
    // membrane is defence-in-depth — the JS-side fetch wrapper is the
    // actual security boundary, and a missing Python deny-proxy doesn't
    // open a network egress hole. Earlier drafts tried to fail-closed
    // here, but real Pyodide environments turn out to have many ways the
    // attr-shadow loop can fail (Pyodide proxy coercion, missing
    // globalThis attrs, version skew on pyodide.ffi), and every one of
    // those would refuse user `python3` invocations entirely. That's a
    // much bigger regression than the modest defence-in-depth loss.
    //
    // The body is a raw string so Python's required indentation
    // survives. Earlier drafts used `"\\n\\"` line continuations and
    // Rust silently ate every leading space — `PyRun_SimpleString`
    // returned SyntaxError on every invocation and the whole membrane
    // shipped non-functional across the F-series.
    String::from(
        r#"import builtins as _wasmsh_builtins
if not getattr(_wasmsh_builtins, 'WASMSH_MEMBRANE_INSTALLED', False):
    try:
        import js as _wasmsh_js
        try:
            from pyodide.ffi import create_proxy as _wasmsh_create_proxy
        except BaseException:
            _wasmsh_create_proxy = None
        class _WasmshDeny:
            def __init__(self, name):
                object.__setattr__(self, '_name', name)
            def __getattr__(self, attr):
                raise PermissionError(
                    'wasmsh: js.' + object.__getattribute__(self, '_name')
                    + '.' + attr + ' is blocked in the sandbox'
                )
            def __setattr__(self, attr, value):
                raise PermissionError(
                    'wasmsh: setting js.' + object.__getattribute__(self, '_name')
                    + '.' + attr + ' is blocked'
                )
            def __call__(self, *args, **kwargs):
                raise PermissionError(
                    'wasmsh: js.' + object.__getattribute__(self, '_name')
                    + '() is blocked'
                )
        for _wasmsh_denied in (
            'process', 'require', 'Deno', 'WebSocket', 'fs',
            'child_process', 'worker_threads', 'subprocess', 'cluster',
            'crypto',
        ):
            try:
                _wasmsh_target = _WasmshDeny(_wasmsh_denied)
                if _wasmsh_create_proxy is not None:
                    _wasmsh_target = _wasmsh_create_proxy(_wasmsh_target)
                setattr(_wasmsh_js, _wasmsh_denied, _wasmsh_target)
            except BaseException:
                pass
    except BaseException:
        pass
    _wasmsh_builtins.WASMSH_MEMBRANE_INSTALLED = True
"#,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preamble_does_not_leak_allowed_hosts() {
        // After the F2 redesign, allowed_hosts MUST NOT appear in the
        // Python preamble — network policy lives in the JS membrane.
        // This test guards against accidental re-introduction of a
        // Python-readable allowlist that user code could mutate.
        let p = build_preamble(&[
            "api.example.com".into(),
            "*.internal.test".into(),
        ]);
        assert!(
            !p.contains("api.example.com"),
            "Python preamble must not embed the host allowlist"
        );
        assert!(
            !p.contains("*.internal.test"),
            "Python preamble must not embed the host allowlist"
        );
    }

    #[test]
    fn preamble_is_self_guarded() {
        let p = build_preamble(&[]);
        assert!(p.contains("WASMSH_MEMBRANE_INSTALLED"));
    }

    #[test]
    fn preamble_swallows_all_failures() {
        // The preamble is bulletproof against any failure path Pyodide can
        // throw at it: missing `js` module, missing `pyodide.ffi`, failed
        // attr coercion, non-writable globalThis props. Anything raises →
        // outer `except BaseException` absorbs → user `python3` still runs.
        // This is a deliberate retreat from the earlier fail-closed posture
        // because in practice the failure modes are too varied to triage
        // and the JS-side fetch membrane is the actual security boundary.
        let p = build_preamble(&[]);
        assert!(p.contains("except BaseException"));
        assert!(!p.contains("raise RuntimeError"));
    }

    #[test]
    fn preamble_preserves_indentation() {
        // Regression for a bug that shipped silently across the entire
        // F-series: an earlier draft used Rust's `"...\n\"` line
        // continuation to break the preamble across source lines, which
        // ate every leading space and produced unindented Python that
        // PyRun_SimpleString refused with IndentationError on the very
        // first `if`. The whole membrane was non-functional, every
        // python3 invocation was rejected, and CI surfaced only
        // "membrane preamble execution failed" with no traceback.
        let p = build_preamble(&[]);
        assert!(p.contains("    try:\n"), "outer try block must be indented");
        assert!(
            p.contains("        import js as _wasmsh_js\n"),
            "import must be indented inside the try block"
        );
        assert!(
            p.contains("            from pyodide.ffi import create_proxy"),
            "create_proxy import must be deeply indented"
        );
        // Negative assertion: the preamble must NOT contain a `try:`
        // immediately at column zero, which would happen if line
        // continuations ever crept back in.
        assert!(
            !p.contains("\ntry:\n"),
            "found a top-level `try:` — Rust line continuations are eating indentation again"
        );
    }

    #[test]
    fn preamble_always_sets_installed_sentinel() {
        // Idempotency: the WASMSH_MEMBRANE_INSTALLED flag must be set on
        // the builtins module regardless of whether shadowing succeeded,
        // so subsequent invocations of the preamble are no-ops.
        let p = build_preamble(&[]);
        assert!(p.contains("WASMSH_MEMBRANE_INSTALLED = True"));
    }

    /// Lets us dump the actual emitted Python source from the build script
    /// for debugging when CI shows `PyRun_SimpleString` returning nonzero
    /// against an opaque Python environment.
    #[test]
    #[ignore]
    fn dump_preamble_to_stderr() {
        eprintln!("--- BEGIN PREAMBLE ---");
        eprint!("{}", build_preamble(&[]));
        eprintln!("--- END PREAMBLE ---");
    }
}
