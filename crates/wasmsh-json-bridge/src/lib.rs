//! Shared JSON transport and libc-backed probe helpers for wasmsh embedders.
//!
//! The canonical host transport for embedded runtimes is the serde JSON form
//! of [`wasmsh_protocol::HostCommand`] in and [`wasmsh_protocol::WorkerEvent`]
//! out. This crate owns that bridge so Pyodide, the WASI P2 component target,
//! and probe helpers cannot drift.

#![warn(missing_docs)]

use std::ffi::{CStr, CString};
use std::sync::OnceLock;

use wasmsh_protocol::{DiagnosticLevel, HostCommand, WorkerEvent, PROTOCOL_VERSION};
use wasmsh_runtime::{ExternalCommandHandler, WorkerRuntime};
use wasmsh_utils::net_types::{HostAllowlist, HttpRequest, HttpResponse, NetworkBackend, NetworkError};

/// Create a network backend for the allowed-host configuration from `Init`.
pub type NetworkBackendFactory = Box<dyn Fn(Vec<String>) -> Box<dyn NetworkBackend>>;

/// Configuration for a shared JSON runtime handle.
#[allow(
    missing_debug_implementations,
    reason = "callback trait objects do not implement Debug"
)]
#[derive(Default)]
pub struct JsonRuntimeConfig {
    /// Optional handler for external commands such as `python` / `python3`.
    pub external_handler: Option<ExternalCommandHandler>,
    /// Factory used to install a deterministic network backend on every
    /// `Init`. When unset, a deny-all backend is used.
    pub network_backend_factory: Option<NetworkBackendFactory>,
}

/// Shared JSON runtime handle used by both Pyodide and the WASI P2 component.
///
/// One handle owns one [`WorkerRuntime`]. `handle_json` parses a JSON
/// `HostCommand`, dispatches it, and serializes the resulting event array back
/// to JSON.
#[allow(
    missing_debug_implementations,
    reason = "WorkerRuntime and callback trait objects do not implement Debug"
)]
pub struct JsonRuntimeHandle {
    runtime: WorkerRuntime,
    network_backend_factory: NetworkBackendFactory,
}

impl JsonRuntimeHandle {
    /// Construct a handle with the default deny-all network backend.
    #[must_use]
    pub fn new() -> Self {
        Self::with_config(JsonRuntimeConfig::default())
    }

    /// Construct a handle with transport-specific callbacks and backend hooks.
    #[must_use]
    pub fn with_config(config: JsonRuntimeConfig) -> Self {
        let mut runtime = WorkerRuntime::new();
        if let Some(handler) = config.external_handler {
            runtime.set_external_handler(handler);
        }
        Self {
            runtime,
            network_backend_factory: config
                .network_backend_factory
                .unwrap_or_else(|| Box::new(|allowed_hosts| Box::new(DenyingNetworkBackend::new(allowed_hosts)))),
        }
    }

    /// Parse a JSON `HostCommand`, dispatch it, and serialize the resulting
    /// `Vec<WorkerEvent>` back to JSON.
    #[must_use]
    pub fn handle_json(&mut self, input: &str) -> String {
        let cmd: HostCommand = match serde_json::from_str(input) {
            Ok(command) => command,
            Err(error) => {
                return serialize_events(&[WorkerEvent::Diagnostic(
                    DiagnosticLevel::Error,
                    format!("invalid JSON command: {error}"),
                )]);
            }
        };

        if let HostCommand::Init {
            ref allowed_hosts, ..
        } = cmd
        {
            self.runtime
                .set_network_backend((self.network_backend_factory)(allowed_hosts.clone()));
        }

        let events = self.runtime.handle_command(cmd);
        serialize_events(&events)
    }
}

impl Default for JsonRuntimeHandle {
    fn default() -> Self {
        Self::new()
    }
}

fn serialize_events(events: &[WorkerEvent]) -> String {
    serde_json::to_string(&events).unwrap_or_else(|_| "[]".to_string())
}

#[derive(Debug)]
struct DenyingNetworkBackend {
    allowlist: HostAllowlist,
}

impl DenyingNetworkBackend {
    fn new(allowed_hosts: Vec<String>) -> Self {
        Self {
            allowlist: HostAllowlist::new(allowed_hosts),
        }
    }
}

impl NetworkBackend for DenyingNetworkBackend {
    fn fetch(&self, request: &HttpRequest) -> Result<HttpResponse, NetworkError> {
        self.allowlist.check(&request.url)?;
        Err(NetworkError::Other(
            "transport does not provide a network backend".to_string(),
        ))
    }
}

/// Return the shared protocol version string.
#[must_use]
pub fn probe_version() -> &'static str {
    PROTOCOL_VERSION
}

/// Return the shared protocol version as a stable C string.
#[must_use]
pub fn probe_version_cstr() -> &'static CStr {
    static VERSION: OnceLock<CString> = OnceLock::new();
    VERSION
        .get_or_init(|| CString::new(PROTOCOL_VERSION).expect("PROTOCOL_VERSION contains no null bytes"))
        .as_c_str()
}

#[allow(unsafe_code, reason = "libc probe helpers require raw FFI calls")]
mod libc_probe {
    use std::ffi::CStr;

    pub(super) fn write_text(path: &CStr, text: &CStr) -> i32 {
        let mode = c"w";
        let fp = unsafe { libc::fopen(path.as_ptr(), mode.as_ptr()) };
        if fp.is_null() {
            return -1;
        }
        let bytes = text.to_bytes();
        let written = unsafe { libc::fwrite(bytes.as_ptr().cast(), 1, bytes.len(), fp) };
        unsafe { libc::fclose(fp) };
        if written == bytes.len() {
            0
        } else {
            -1
        }
    }

    pub(super) fn file_equals(path: &CStr, expected: &CStr) -> bool {
        let mode = c"r";
        let fp = unsafe { libc::fopen(path.as_ptr(), mode.as_ptr()) };
        if fp.is_null() {
            return false;
        }

        let expected_bytes = expected.to_bytes();
        let mut buf = vec![0u8; 65_536];
        let n = unsafe { libc::fread(buf.as_mut_ptr().cast(), 1, buf.len(), fp) };
        unsafe { libc::fclose(fp) };

        n == expected_bytes.len() && &buf[..n] == expected_bytes
    }
}

/// Write `text` to `path` using libc file I/O.
///
/// Returns `0` on success and `-1` on failure.
pub fn probe_write_text(path: &CStr, text: &CStr) -> i32 {
    libc_probe::write_text(path, text)
}

/// Check whether `path` contains exactly `expected`.
///
/// Returns `true` if equal and `false` otherwise.
#[must_use]
pub fn probe_file_equals(path: &CStr, expected: &CStr) -> bool {
    libc_probe::file_equals(path, expected)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_events(payload: &str) -> Vec<WorkerEvent> {
        serde_json::from_str(payload).expect("valid WorkerEvent payload")
    }

    #[test]
    fn init_returns_version_event() {
        let mut handle = JsonRuntimeHandle::new();
        let payload = handle.handle_json(
            &serde_json::to_string(&HostCommand::Init {
                step_budget: 100_000,
                allowed_hosts: Vec::new(),
            })
            .unwrap(),
        );
        assert_eq!(
            decode_events(&payload),
            vec![WorkerEvent::Version(PROTOCOL_VERSION.to_string())]
        );
    }

    #[test]
    fn invalid_json_returns_error_diagnostic() {
        let mut handle = JsonRuntimeHandle::new();
        let payload = handle.handle_json("{not valid json");
        assert!(
            matches!(
                decode_events(&payload).as_slice(),
                [WorkerEvent::Diagnostic(DiagnosticLevel::Error, message)]
                    if message.contains("invalid JSON command")
            ),
            "expected invalid JSON diagnostic"
        );
    }
}
