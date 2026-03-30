//! Network backend for the Pyodide (Emscripten) target.
//!
//! Calls into JavaScript via `extern "C"` FFI for synchronous HTTP.
//! The JS host (browser-worker.js or node-host.mjs) provides the
//! `wasmsh_js_http_fetch` function implementation.

use std::ffi::{CStr, CString};
use std::os::raw::c_char;

use wasmsh_utils::net_types::{
    HostAllowlist, HttpRequest, HttpResponse, NetworkBackend, NetworkError,
};

extern "C" {
    /// Synchronous HTTP fetch provided by the JS host.
    ///
    /// Takes request parameters as C strings (JSON-encoded where needed).
    /// Returns a JSON string `{"status":200,"headers":[],"body_base64":"..."}`.
    /// The caller must free the returned string with `libc::free`.
    fn wasmsh_js_http_fetch(
        url: *const c_char,
        method: *const c_char,
        headers_json: *const c_char,
        body: *const u8,
        body_len: u32,
        follow_redirects: i32,
    ) -> *mut c_char;
}

/// Network backend for the Pyodide Emscripten target.
pub struct PyodideNetworkBackend {
    allowlist: HostAllowlist,
}

impl PyodideNetworkBackend {
    pub fn new(allowed_hosts: Vec<String>) -> Self {
        Self {
            allowlist: HostAllowlist::new(allowed_hosts),
        }
    }
}

/// JSON shape returned by the JS fetch function.
#[derive(serde::Deserialize)]
struct JsFetchResponse {
    status: u16,
    #[serde(default)]
    headers: Vec<(String, String)>,
    #[serde(default)]
    body_base64: String,
    #[serde(default)]
    error: Option<String>,
}

fn decode_base64(input: &str) -> Vec<u8> {
    // Minimal base64 decoder — no external dep needed.
    let table: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut buf: u32 = 0;
    let mut bits: u32 = 0;
    for &b in input.as_bytes() {
        let val = if b == b'=' {
            continue;
        } else if let Some(pos) = table.iter().position(|&t| t == b) {
            pos as u32
        } else {
            continue; // skip whitespace/invalid
        };
        buf = (buf << 6) | val;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    out
}

impl NetworkBackend for PyodideNetworkBackend {
    fn fetch(&self, request: &HttpRequest) -> Result<HttpResponse, NetworkError> {
        self.allowlist.check(&request.url)?;

        let url_c =
            CString::new(request.url.as_str()).map_err(|e| NetworkError::Other(e.to_string()))?;
        let method_c = CString::new(request.method.as_str())
            .map_err(|e| NetworkError::Other(e.to_string()))?;
        let headers_json = serde_json::to_string(&request.headers).unwrap_or_else(|_| "[]".into());
        let headers_c =
            CString::new(headers_json.as_str()).map_err(|e| NetworkError::Other(e.to_string()))?;

        let (body_ptr, body_len) = match &request.body {
            Some(b) => (b.as_ptr(), b.len() as u32),
            None => (std::ptr::null(), 0),
        };

        let result_ptr = unsafe {
            wasmsh_js_http_fetch(
                url_c.as_ptr(),
                method_c.as_ptr(),
                headers_c.as_ptr(),
                body_ptr,
                body_len,
                i32::from(request.follow_redirects),
            )
        };

        if result_ptr.is_null() {
            return Err(NetworkError::Other("fetch returned null".into()));
        }

        let result_str = unsafe { CStr::from_ptr(result_ptr) }
            .to_str()
            .unwrap_or("{}");
        let parsed: JsFetchResponse = serde_json::from_str(result_str)
            .map_err(|e| NetworkError::Other(format!("invalid fetch response: {e}")))?;

        // Free the JS-allocated string.
        unsafe {
            libc::free(result_ptr.cast());
        }

        if let Some(err) = parsed.error {
            return Err(NetworkError::ConnectionFailed(err));
        }

        let body = decode_base64(&parsed.body_base64);

        Ok(HttpResponse {
            status: parsed.status,
            headers: parsed.headers,
            body,
        })
    }
}
