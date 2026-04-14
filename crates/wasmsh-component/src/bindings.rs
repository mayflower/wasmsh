//! Component export layer: `wit-bindgen`-generated bindings plus the glue
//! that forwards every WIT call into the shared JSON bridge and probe helpers.

use std::cell::RefCell;
use std::ffi::CString;

use wasmsh_json_bridge::{probe_file_equals, probe_version, probe_write_text, JsonRuntimeHandle};

wit_bindgen::generate!({
    world: "wasmsh",
    path: "wit",
});

use exports::wasmsh::component::runtime::{Guest, GuestHandle};

struct Component;

export!(Component);

impl Guest for Component {
    type Handle = ComponentHandle;

    fn probe_version() -> String {
        probe_version().to_string()
    }

    fn probe_write_text(path: String, text: String) -> i32 {
        match (CString::new(path), CString::new(text)) {
            (Ok(path), Ok(text)) => probe_write_text(path.as_c_str(), text.as_c_str()),
            _ => -1,
        }
    }

    fn probe_file_equals(path: String, expected: String) -> bool {
        match (CString::new(path), CString::new(expected)) {
            (Ok(path), Ok(expected)) => probe_file_equals(path.as_c_str(), expected.as_c_str()),
            _ => false,
        }
    }
}

/// Adapter between the WIT `handle` resource and the shared JSON runtime.
pub(crate) struct ComponentHandle {
    inner: RefCell<JsonRuntimeHandle>,
}

impl GuestHandle for ComponentHandle {
    fn new() -> Self {
        Self {
            inner: RefCell::new(crate::python::new_runtime_handle()),
        }
    }

    fn handle_json(&self, input: String) -> String {
        self.inner.borrow_mut().handle_json(&input)
    }
}
