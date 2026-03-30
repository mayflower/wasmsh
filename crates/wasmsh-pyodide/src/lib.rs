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

use wasmsh_protocol::HostCommand;
use wasmsh_runtime::WorkerRuntime;

mod network;
mod python_cmd;

/// Opaque handle wrapping a `WorkerRuntime`.
struct RuntimeHandle {
    runtime: WorkerRuntime,
}

/// Create a new wasmsh runtime instance with python/python3 command support.
///
/// Returns an opaque pointer. Must be freed with [`wasmsh_runtime_free`].
#[no_mangle]
pub extern "C" fn wasmsh_runtime_new() -> *mut RuntimeHandle {
    let mut runtime = WorkerRuntime::new();
    runtime.set_external_handler(Box::new(python_cmd::handle_python_command));
    Box::into_raw(Box::new(RuntimeHandle { runtime }))
}

/// Process a JSON-encoded `HostCommand` and return a JSON-encoded event array.
#[no_mangle]
pub extern "C" fn wasmsh_runtime_handle_json(
    handle: *mut RuntimeHandle,
    json_ptr: *const c_char,
) -> *mut c_char {
    if handle.is_null() || json_ptr.is_null() {
        return alloc_cstring("[]");
    }

    let json_str = unsafe { CStr::from_ptr(json_ptr) }
        .to_str()
        .unwrap_or("");

    let cmd: HostCommand = match serde_json::from_str(json_str) {
        Ok(c) => c,
        Err(_) => return alloc_cstring("[]"),
    };

    let rt = unsafe { &mut (*handle).runtime };

    // On Init with allowed_hosts, configure the network backend.
    if let HostCommand::Init {
        ref allowed_hosts, ..
    } = cmd
    {
        if !allowed_hosts.is_empty() {
            let backend = network::PyodideNetworkBackend::new(allowed_hosts.clone());
            rt.set_network_backend(Box::new(backend));
        }
    }

    let events = rt.handle_command(cmd);

    let result = serde_json::to_string(&events).unwrap_or_else(|_| "[]".to_string());
    alloc_cstring(&result)
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
