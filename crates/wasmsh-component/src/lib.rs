//! WASI P2 Component Model adapter for the `wasmsh` runtime.
//!
//! The component target is intentionally thin: it re-exports the same JSON
//! `HostCommand` -> `Vec<WorkerEvent>` bridge used by the Pyodide transport
//! through a Component Model resource handle plus shared probe helpers.
//! This keeps the Pyodide and WASI P2 embeddings on one canonical transport
//! implementation instead of creating a second typed control protocol.

#![warn(missing_docs)]
#![allow(
    missing_docs,
    reason = "module-level docs above cover the public surface"
)]

pub use wasmsh_json_bridge::JsonRuntimeHandle;

// ── Component export layer ──────────────────────────────────────────────
//
// The wit-bindgen-generated bindings and the component export glue are only
// compiled when the `component-export` feature is enabled AND the crate is
// being built for wasm32. This keeps host-native tests and clippy runs away
// from generated component ABI glue while still exporting the thin runtime
// handle on wasm32-wasip2 builds.
#[cfg(all(feature = "component-export", target_arch = "wasm32"))]
#[allow(
    unsafe_code,
    clippy::missing_safety_doc,
    clippy::same_length_and_capacity,
    reason = "wit-bindgen generates unsafe Component Model ABI glue"
)]
mod bindings;
