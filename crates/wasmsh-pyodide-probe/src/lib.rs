//! Minimal probe crate for `wasm32-unknown-emscripten`.
//!
//! Exposes C ABI functions for testing same-module integration with
//! Pyodide.  This crate is **not** part of the cargo workspace
//! (`[workspace.exclude]`) so that developers without `emcc` installed
//! are unaffected.
//!
//! ⚠️ The three probe functions are also defined in
//! `crates/wasmsh-pyodide/src/probe.rs`, which is the copy that actually
//! ends up linked into the production Pyodide wasm.  This standalone
//! crate exists solely for the `e2e/build-contract` test, which builds
//! it in isolation to prove that any wasmsh crate compiles cleanly for
//! the emscripten target.  Any semantic change here MUST also be applied
//! to the in-tree copy, otherwise the test and the shipped wasm silently
//! diverge.
//!
//! All functions use the Emscripten POSIX filesystem (libc), which is
//! the same filesystem Python sees — proving shared FS access.

use std::ffi::CStr;
use std::os::raw::c_char;
use wasmsh_json_bridge::{probe_file_equals, probe_version_cstr, probe_write_text};

/// Returns the wasmsh protocol version as a null-terminated C string.
///
/// The returned pointer is valid for the lifetime of the program.
#[no_mangle]
pub extern "C" fn wasmsh_probe_version() -> *const c_char {
    probe_version_cstr().as_ptr()
}

/// Write `text` to the file at `path` using POSIX libc.
///
/// Returns 0 on success, -1 on failure.
///
/// This function is exposed as a safe Rust `extern "C" fn` because only
/// C / JS callers can reach it — Rust callers would need to dereference
/// `*const c_char` themselves.  Null pointers are checked explicitly and
/// return `-1`; any other invariant violation (non-null-terminated or
/// dangling pointer) is unchecked and is the C caller's responsibility.
#[no_mangle]
pub extern "C" fn wasmsh_probe_write_text(path: *const c_char, text: *const c_char) -> i32 {
    if path.is_null() || text.is_null() {
        return -1;
    }
    probe_write_text(unsafe { CStr::from_ptr(path) }, unsafe {
        CStr::from_ptr(text)
    })
}

/// Check whether the file at `path` has exactly the content `expected`.
///
/// Returns 1 if equal, 0 if not equal, the file cannot be opened, or the
/// file is larger than the internal 64 KiB read buffer.  **Note:** the
/// "not equal" and "error" paths both return 0 — this is intentional for
/// the test helper, but callers should not use this to distinguish
/// missing files from content mismatches.
///
/// See [`wasmsh_probe_write_text`] for the null-pointer / lifetime
/// contract; this function applies the same conventions.
#[no_mangle]
pub extern "C" fn wasmsh_probe_file_equals(path: *const c_char, expected: *const c_char) -> i32 {
    if path.is_null() || expected.is_null() {
        return 0;
    }
    i32::from(probe_file_equals(
        unsafe { CStr::from_ptr(path) },
        unsafe { CStr::from_ptr(expected) },
    ))
}
