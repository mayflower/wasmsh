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

extern "C" {
    fn Py_SetProgramName(name: *const std::os::raw::c_int);
    fn Py_SetPythonHome(home: *const std::os::raw::c_int);
    fn Py_SetPath(path: *const std::os::raw::c_int);
    fn Py_Initialize();
    fn Py_IsInitialized() -> std::os::raw::c_int;
    fn PyRun_SimpleString(command: *const std::os::raw::c_char) -> std::os::raw::c_int;
}

/// Helper: call PyRun_SimpleString to add a path to sys.path at runtime.
unsafe fn add_to_sys_path(path: &str) {
    let cmd = format!("import sys; sys.path.insert(0, '{path}')\0");
    PyRun_SimpleString(cmd.as_ptr().cast());
}

/// Boot the Pyodide runtime (Python interpreter initialization).
///
/// Must be called once after wasm instantiation and before
/// [`wasmsh_runtime_new`]. In the JS-launched path, the JS loader handles
/// this implicitly; in the no-JS Wasmtime path, the host calls this export
/// directly.
///
/// Returns 0 on success, non-zero on failure.
#[no_mangle]
pub extern "C" fn wasmsh_pyodide_boot() -> i32 {
    unsafe {
        if Py_IsInitialized() != 0 {
            return 0;
        }
        // Set PYTHONHOME via the C API so getpath computes
        // sys.path = [PYTHONHOME/lib/python3.13, ...].
        // Do NOT call Py_SetPath — it overrides getpath entirely and
        // may not compute stdlib_dir/prefix correctly.
        static PYTHON_HOME: &[i32] = &['/' as i32, 0];
        Py_SetPythonHome(PYTHON_HOME.as_ptr());

        static PROGRAM_NAME: &[i32] = &[
            '/' as i32, 'p' as i32, 'y' as i32, 't' as i32, 'h' as i32,
            'o' as i32, 'n' as i32, '3' as i32, 0,
        ];
        Py_SetProgramName(PROGRAM_NAME.as_ptr());

        Py_Initialize();

        if Py_IsInitialized() == 0 {
            return 1;
        }

    }
    0
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
