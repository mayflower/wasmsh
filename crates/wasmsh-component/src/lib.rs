//! WASI P2 Component Model adapter for the `wasmsh` runtime.
//!
//! This crate is the third transport for the canonical `wasmsh-protocol`
//! command/event contract, alongside the native Rust calls consumed by
//! `wasmsh-browser` and the JSON-over-C-ABI path consumed by `wasmsh-pyodide`.
//! It is intended as the first wasmCloud-facing seam — a Component Model
//! component that exports a custom `wasmsh:component/sandbox` interface with
//! a stateful `session` resource. Host-side wasmCloud plugin wiring, the
//! `DeepAgents` adapter, and multi-tenant session registries are deliberately
//! out of scope; see `docs/adr/adr-0030-wasmcloud-component-transport.md`.
//!
//! The crate is split into:
//!
//! - [`core`] — host-testable `SessionCore` that owns one initialised
//!   [`wasmsh_runtime::WorkerRuntime`]. Target-agnostic, unit-tested, with no
//!   dependency on wit-bindgen. Every WIT method in `wit/world.wit`
//!   corresponds to exactly one `SessionCore` method.
//! - [`event`] — value-based mirror of `wasmsh_protocol::WorkerEvent` and
//!   `DiagnosticLevel`, plus the total `From` conversions used by the
//!   component export layer. Also target-agnostic.
//! - `bindings` — wit-bindgen-generated Rust bindings. Only built when the
//!   `component-export` feature is enabled (i.e. when building for
//!   `wasm32-wasip2`).
//!
//! The WIT interface itself lives in `wit/world.wit`. It mirrors the shape
//! of `HostCommand` / `WorkerEvent` but exposes the session-scoped subset
//! that the first wasmCloud-facing cut actually needs.

#![warn(missing_docs)]
#![allow(
    missing_docs,
    reason = "module-level docs above cover the public surface"
)]

pub mod core;
pub mod event;

pub use crate::core::{run_once, RuntimeConfig, SessionCore, DEFAULT_STEP_BUDGET};
pub use crate::event::{events_from_worker, ComponentDiagnosticLevel, ComponentEvent};

// ── Component export layer ──────────────────────────────────────────────
//
// The wit-bindgen-generated bindings and the component export glue are only
// compiled when the `component-export` feature is enabled AND the crate is
// being built for a wasm32 target. Gating on both lets `cargo clippy
// --all-features` run on the native host without trying to emit wasm ABI
// symbols (`#[export_name]`, Component Model `cabi_realloc`, etc.) that
// native rustc rejects under the workspace's `unsafe_code = "deny"` policy.
//
// The `wit-bindgen` macro itself expands into generated code that uses
// `unsafe` blocks and raw `#[export_name]` symbols to satisfy the Component
// Model ABI. The workspace denies `unsafe_code` globally; we allow it only
// in this module, which contains no hand-written `unsafe` beyond the
// generated bindings and the thin resource glue that forwards into
// `SessionCore`.
#[cfg(all(feature = "component-export", target_arch = "wasm32"))]
#[allow(
    unsafe_code,
    clippy::missing_safety_doc,
    clippy::same_length_and_capacity,
    reason = "wit-bindgen generates unsafe Component Model ABI glue"
)]
mod bindings;
