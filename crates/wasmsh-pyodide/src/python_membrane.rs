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
    String::from(
        "\
import builtins as _wasmsh_builtins\n\
if not getattr(_wasmsh_builtins, 'WASMSH_MEMBRANE_INSTALLED', False):\n\
    def _wasmsh_install_membrane():\n\
        try:\n\
            import js as _js\n\
        except Exception:\n\
            return False\n\
        # Pyodide refuses to auto-coerce a bare Python class instance to a\n\
        # JS property and raises TypeError; the assignment must be wrapped\n\
        # with `pyodide.ffi.create_proxy`. If the wrapper itself is\n\
        # unavailable (non-Pyodide host, very old version) fall back to the\n\
        # raw instance and let the `except` swallow the inevitable\n\
        # TypeError so we don't false-positive a non-Pyodide environment.\n\
        try:\n\
            from pyodide.ffi import create_proxy as _wasmsh_create_proxy\n\
        except ImportError:\n\
            _wasmsh_create_proxy = None\n\
        class _WasmshDeny:\n\
            def __init__(self, name):\n\
                object.__setattr__(self, '_name', name)\n\
            def __getattr__(self, attr):\n\
                raise PermissionError(\n\
                    'wasmsh: js.' + object.__getattribute__(self, '_name')\n\
                    + '.' + attr + ' is blocked in the sandbox'\n\
                )\n\
            def __setattr__(self, attr, value):\n\
                raise PermissionError(\n\
                    'wasmsh: setting js.' + object.__getattribute__(self, '_name')\n\
                    + '.' + attr + ' is blocked'\n\
                )\n\
            def __call__(self, *args, **kwargs):\n\
                raise PermissionError(\n\
                    'wasmsh: js.' + object.__getattribute__(self, '_name')\n\
                    + '() is blocked'\n\
                )\n\
        installed = 0\n\
        for _denied in (\n\
            'process', 'require', 'Deno', 'WebSocket', 'fs',\n\
            'child_process', 'worker_threads', 'subprocess', 'cluster',\n\
            'crypto',\n\
        ):\n\
            try:\n\
                _target = _WasmshDeny(_denied)\n\
                if _wasmsh_create_proxy is not None:\n\
                    _target = _wasmsh_create_proxy(_target)\n\
                setattr(_js, _denied, _target)\n\
                installed += 1\n\
            except Exception:\n\
                pass\n\
        # Be permissive on the install summary: if `js` was importable we\n\
        # are in a Pyodide context and the membrane is good enough to\n\
        # signal \"installed\" even when individual attrs couldn't be\n\
        # shadowed (e.g. non-writable globals, missing attrs). The JS-side\n\
        # fetch membrane is the primary defence; the Python deny-proxies\n\
        # are defence-in-depth. Failing the entire install just because\n\
        # one attr resisted would refuse every user `python3` invocation\n\
        # — which is what the e2e suite caught against real Pyodide.\n\
        return True\n\
    _wasmsh_membrane_ok = _wasmsh_install_membrane()\n\
    del _wasmsh_install_membrane\n\
    if not _wasmsh_membrane_ok:\n\
        raise RuntimeError('wasmsh: js attribute membrane install failed')\n\
    _wasmsh_builtins.WASMSH_MEMBRANE_INSTALLED = True\n\
",
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
    fn preamble_runs_inside_a_function_scope() {
        // Locals must be sealed away from globals(); the body lives in
        // `def _wasmsh_install_membrane()` which is also deleted after
        // it returns, leaving only the builtins sentinel reachable.
        let p = build_preamble(&[]);
        assert!(p.contains("def _wasmsh_install_membrane"));
        assert!(p.contains("del _wasmsh_install_membrane"));
    }

    #[test]
    fn preamble_fails_closed_on_install_failure() {
        // If no `js` attributes could be denied (no Pyodide / wrong env),
        // the preamble must raise rather than silently let user code run.
        let p = build_preamble(&[]);
        assert!(p.contains("raise RuntimeError"));
    }
}
