//! Build-time / E2E probe helpers exposed via the C ABI.
//!
//! These three functions used to live in the standalone `wasmsh-pyodide-probe`
//! crate, which was linked alongside `wasmsh-pyodide` into the Pyodide wasm.
//! With `MAIN_MODULE=1` Emscripten uses `--whole-archive` and the duplicate
//! `wasmsh-protocol` / `serde_core` symbols pulled in by both staticlibs
//! caused link failures. Folding the probe entry points into this crate keeps
//! exactly one copy of each transitive dependency in the final wasm.
//!
//! All three functions go through libc, which routes through Emscripten's
//! POSIX VFS — the same filesystem CPython sees inside Pyodide.

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::sync::OnceLock;

/// Returns the wasmsh protocol version as a null-terminated C string.
///
/// The returned pointer is valid for the lifetime of the program.
#[no_mangle]
pub extern "C" fn wasmsh_probe_version() -> *const c_char {
    static VERSION: OnceLock<CString> = OnceLock::new();
    VERSION
        .get_or_init(|| {
            CString::new(wasmsh_protocol::PROTOCOL_VERSION)
                .expect("PROTOCOL_VERSION contains no null bytes")
        })
        .as_ptr()
}

/// Write `text` to the file at `path` using POSIX libc.
///
/// Returns 0 on success, -1 on failure.
///
/// # Safety
///
/// Caller must ensure both pointers are valid, non-null, null-terminated.
#[no_mangle]
pub extern "C" fn wasmsh_probe_write_text(path: *const c_char, text: *const c_char) -> i32 {
    if path.is_null() || text.is_null() {
        return -1;
    }
    let path_str = unsafe { CStr::from_ptr(path) };
    let text_str = unsafe { CStr::from_ptr(text) };

    let mode = c"w";
    let fp = unsafe { libc::fopen(path_str.as_ptr(), mode.as_ptr()) };
    if fp.is_null() {
        return -1;
    }
    let bytes = text_str.to_bytes();
    let written = unsafe { libc::fwrite(bytes.as_ptr().cast(), 1, bytes.len(), fp) };
    unsafe { libc::fclose(fp) };
    if written == bytes.len() {
        0
    } else {
        -1
    }
}

/// Check whether the file at `path` has exactly the content `expected`.
///
/// Returns 1 if equal, 0 if not equal or on error.
///
/// # Safety
///
/// Caller must ensure both pointers are valid, non-null, null-terminated.
#[no_mangle]
pub extern "C" fn wasmsh_probe_file_equals(path: *const c_char, expected: *const c_char) -> i32 {
    if path.is_null() || expected.is_null() {
        return 0;
    }
    let path_str = unsafe { CStr::from_ptr(path) };
    let expected_bytes = unsafe { CStr::from_ptr(expected) }.to_bytes();

    let mode = c"r";
    let fp = unsafe { libc::fopen(path_str.as_ptr(), mode.as_ptr()) };
    if fp.is_null() {
        return 0;
    }

    let mut buf = vec![0u8; 65536];
    let n = unsafe { libc::fread(buf.as_mut_ptr().cast(), 1, buf.len(), fp) };
    unsafe { libc::fclose(fp) };

    if n == expected_bytes.len() && &buf[..n] == expected_bytes {
        1
    } else {
        0
    }
}
