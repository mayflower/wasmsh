#![allow(unsafe_code, reason = "embedded CPython initialization uses the C API")]

use std::sync::OnceLock;
use std::ffi::CStr;

use wasmsh_json_bridge::{JsonRuntimeConfig, JsonRuntimeHandle};
use wasmsh_runtime::ExternalCommandResult;

mod python_cmd;

unsafe extern "C" {
    fn wasmsh_python_initialize() -> std::os::raw::c_int;
    fn wasmsh_python_initialize_error() -> *const std::os::raw::c_char;
}

static PYTHON_INIT: OnceLock<Result<(), String>> = OnceLock::new();

pub(crate) fn new_runtime_handle() -> JsonRuntimeHandle {
    JsonRuntimeHandle::with_config(JsonRuntimeConfig {
        external_handler: Some(Box::new(|cmd_name, argv, stdin| {
            if cmd_name != "python" && cmd_name != "python3" {
                return None;
            }

            match ensure_python_initialized() {
                Ok(()) => python_cmd::handle_python_command(cmd_name, argv, stdin),
                Err(error) => Some(ExternalCommandResult {
                    stdout: Vec::new(),
                    stderr: format!("wasmsh: python: {error}\n").into_bytes(),
                    status: 1,
                }),
            }
        })),
        network_backend_factory: None,
    })
}

fn ensure_python_initialized() -> Result<(), String> {
    PYTHON_INIT.get_or_init(initialize_python).clone()
}

fn initialize_python() -> Result<(), String> {
    unsafe {
        if wasmsh_python_initialize() != 0 {
            let error_ptr = wasmsh_python_initialize_error();
            let message = if error_ptr.is_null() {
                "failed to initialize embedded CPython".to_string()
            } else {
                CStr::from_ptr(error_ptr).to_string_lossy().into_owned()
            };
            return Err(message);
        }
    }

    Ok(())
}
