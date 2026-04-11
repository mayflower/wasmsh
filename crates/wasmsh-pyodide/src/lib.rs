//! Pyodide-facing C ABI for the wasmsh runtime.
//!
//! Exposes a narrow JSON-based protocol over C ABI functions so that
//! the wasmsh shell can be driven from JavaScript inside a Pyodide module.
//!
//! # Exported functions
//!
//! - `wasmsh_runtime_new()` → opaque handle (with python command handler)
//! - `wasmsh_runtime_handle_json(handle, json)` → JSON result string
//! - `wasmsh_runtime_free(handle)` — drop the runtime
//! - `wasmsh_runtime_free_string(ptr)` — free a string returned by `handle_json`

use std::ffi::{CStr, CString};
use std::os::raw::c_char;

use wasmsh_json_bridge::{JsonRuntimeConfig, JsonRuntimeHandle};
use wasmsh_protocol::{DiagnosticLevel, WorkerEvent};

mod network;
mod probe;
mod python_cmd;

/// Opaque handle wrapping a shared JSON runtime bridge.
struct RuntimeHandle {
    runtime: JsonRuntimeHandle,
}

/// Create a new wasmsh runtime instance with python/python3 command support.
///
/// Returns an opaque pointer. Must be freed with [`wasmsh_runtime_free`].
#[no_mangle]
pub extern "C" fn wasmsh_runtime_new() -> *mut RuntimeHandle {
    let runtime = JsonRuntimeHandle::with_config(JsonRuntimeConfig {
        external_handler: Some(Box::new(python_cmd::handle_python_command)),
        network_backend_factory: Some(Box::new(|allowed_hosts| {
            Box::new(network::PyodideNetworkBackend::new(allowed_hosts))
        })),
    });
    Box::into_raw(Box::new(RuntimeHandle { runtime }))
}

/// Process a JSON-encoded `HostCommand` and return a JSON-encoded event array.
#[no_mangle]
pub extern "C" fn wasmsh_runtime_handle_json(
    handle: *mut RuntimeHandle,
    json_ptr: *const c_char,
) -> *mut c_char {
    if handle.is_null() || json_ptr.is_null() {
        let err = vec![WorkerEvent::Diagnostic(
            DiagnosticLevel::Error,
            "null pointer passed to wasmsh_runtime_handle_json".into(),
        )];
        let json = serde_json::to_string(&err).unwrap_or_else(|_| "[]".to_string());
        return alloc_cstring(&json);
    }

    let json_str = match unsafe { CStr::from_ptr(json_ptr) }.to_str() {
        Ok(s) => s,
        Err(_) => {
            let err = vec![WorkerEvent::Diagnostic(
                DiagnosticLevel::Error,
                "invalid UTF-8 in JSON command".into(),
            )];
            let json = serde_json::to_string(&err).unwrap_or_else(|_| "[]".to_string());
            return alloc_cstring(&json);
        }
    };

    let rt = unsafe { &mut (*handle).runtime };
    alloc_cstring(&rt.handle_json(json_str))
}

/// Free a runtime instance created by [`wasmsh_runtime_new`].
#[no_mangle]
pub extern "C" fn wasmsh_runtime_free(handle: *mut RuntimeHandle) {
    if !handle.is_null() {
        drop(unsafe { Box::from_raw(handle) });
    }
}

/// Free a string returned by [`wasmsh_runtime_handle_json`].
#[no_mangle]
pub extern "C" fn wasmsh_runtime_free_string(ptr: *mut c_char) {
    if !ptr.is_null() {
        drop(unsafe { CString::from_raw(ptr) });
    }
}

fn alloc_cstring(s: &str) -> *mut c_char {
    CString::new(s)
        .unwrap_or_else(|_| CString::new("[]").unwrap())
        .into_raw()
}
