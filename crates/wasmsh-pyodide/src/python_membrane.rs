//! Python-side membrane installed once per Pyodide instance.
//!
//! The membrane (audit C1/C2) does two things from inside Python:
//!
//! 1. Replaces `js.fetch` with a Python wrapper that gates calls through
//!    the same host allowlist that `curl`/`wget` use. Because
//!    `pyodide.http.pyfetch` and `micropip` both end up resolving
//!    `js.fetch` at call time, replacing the attribute here automatically
//!    routes every Python network path through the allowlist check.
//!
//! 2. Replaces dangerous attributes on the `js` proxy
//!    (`process`, `require`, `Deno`, `WebSocket`, `fs`, `child_process`,
//!    `worker_threads`, `subprocess`, `cluster`, `crypto`) with deny
//!    proxies that raise `PermissionError` on any read or call. This is
//!    the Python-side counterpart to the JS host's globalThis.fetch wrap
//!    in `session-worker.mjs`; together they form the membrane.
//!
//! Idempotency: the preamble is wrapped in a `try: WASMSH_MEMBRANE_INSTALLED
//! except NameError: ...` guard so re-running is a no-op. The Rust caller
//! also tracks installation state in a thread-local to avoid the
//! PyRun_SimpleString roundtrip on every python invocation.

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

fn build_preamble(allowed_hosts: &[String]) -> String {
    // Build a Python list literal of the allowed hosts. Each host string
    // is JSON-encoded so quotes / backslashes are handled cleanly.
    let mut list = String::from("[");
    for (i, h) in allowed_hosts.iter().enumerate() {
        if i > 0 {
            list.push_str(", ");
        }
        list.push_str(&serde_json::to_string(h).unwrap_or_else(|_| "\"\"".into()));
    }
    list.push(']');

    // The preamble itself. Multiline Python; carefully indented because
    // PyRun_SimpleString expects exec-mode source. Triple-quoted in Rust;
    // we DO NOT use format! beyond the allowlist slot to keep the script
    // safe from injection through future surprises.
    format!(
        "\
try:\n\
    WASMSH_MEMBRANE_INSTALLED  # noqa: F821\n\
except NameError:\n\
    import sys as _sys\n\
    import urllib.parse as _urlparse\n\
    try:\n\
        import js as _js\n\
    except Exception:\n\
        _js = None\n\
    _WASMSH_ALLOWED_HOSTS = {hosts}\n\
\n\
    def _wasmsh_host_allowed(url):\n\
        try:\n\
            parsed = _urlparse.urlparse(url)\n\
            if parsed.scheme.lower() not in ('http', 'https'):\n\
                return False\n\
            host = (parsed.hostname or '').lower().rstrip('.')\n\
            if not host:\n\
                return False\n\
            for pat in _WASMSH_ALLOWED_HOSTS:\n\
                p = pat.lower()\n\
                colon = p.rfind(':')\n\
                if colon > 0 and p[colon+1:].isdigit():\n\
                    p = p[:colon]\n\
                if p.startswith('*.'):\n\
                    if host.endswith('.' + p[2:]):\n\
                        return True\n\
                elif host == p:\n\
                    return True\n\
            return False\n\
        except Exception:\n\
            return False\n\
\n\
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
\n\
    if _js is not None:\n\
        try:\n\
            _wasmsh_original_fetch = _js.fetch\n\
        except Exception:\n\
            _wasmsh_original_fetch = None\n\
\n\
        def _wasmsh_brokered_fetch(input, init=None, *_a, **_kw):\n\
            try:\n\
                url = input if isinstance(input, str) else getattr(input, 'url', None) or str(input)\n\
            except Exception:\n\
                url = ''\n\
            if not _wasmsh_host_allowed(url):\n\
                raise PermissionError(\n\
                    'wasmsh: host denied by sandbox allowlist: ' + str(url)\n\
                )\n\
            if _wasmsh_original_fetch is None:\n\
                raise PermissionError('wasmsh: no underlying fetch available')\n\
            if init is None:\n\
                return _wasmsh_original_fetch(input)\n\
            return _wasmsh_original_fetch(input, init)\n\
\n\
        if _wasmsh_original_fetch is not None:\n\
            try:\n\
                _js.fetch = _wasmsh_brokered_fetch\n\
            except Exception:\n\
                # Some Pyodide versions don't allow rebinding js.fetch from\n\
                # Python; defer to the JS-host globalThis.fetch wrapper.\n\
                pass\n\
\n\
        for _denied in (\n\
            'process', 'require', 'Deno', 'WebSocket', 'fs',\n\
            'child_process', 'worker_threads', 'subprocess', 'cluster',\n\
            'crypto',\n\
        ):\n\
            try:\n\
                setattr(_js, _denied, _WasmshDeny(_denied))\n\
            except Exception:\n\
                pass\n\
\n\
    WASMSH_MEMBRANE_INSTALLED = True\n\
",
        hosts = list,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preamble_contains_each_allowed_host() {
        let p = build_preamble(&[
            "api.example.com".into(),
            "*.internal.test".into(),
        ]);
        assert!(p.contains("api.example.com"));
        assert!(p.contains("*.internal.test"));
    }

    #[test]
    fn preamble_is_self_guarded() {
        let p = build_preamble(&[]);
        assert!(p.contains("WASMSH_MEMBRANE_INSTALLED"));
    }
}
