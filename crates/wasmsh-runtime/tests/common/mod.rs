//! Shared helpers for runtime integration tests.
//!
//! Cargo's integration test runner compiles each `.rs` file under `tests/` as
//! its own crate, so helpers live under `tests/common/mod.rs` and are brought
//! in with `mod common;` from each test file. Individual tests may use a
//! subset of these helpers, which is why the module allows `dead_code`.

#![allow(dead_code)]

use wasmsh_protocol::WorkerEvent;

pub(crate) fn get_stdout(events: &[WorkerEvent]) -> String {
    let mut out = Vec::new();
    for event in events {
        if let WorkerEvent::Stdout(data) = event {
            out.extend_from_slice(data);
        }
    }
    String::from_utf8(out).unwrap_or_default()
}

pub(crate) fn get_stderr(events: &[WorkerEvent]) -> String {
    let mut out = Vec::new();
    for event in events {
        if let WorkerEvent::Stderr(data) = event {
            out.extend_from_slice(data);
        }
    }
    String::from_utf8(out).unwrap_or_default()
}

pub(crate) fn get_exit(events: &[WorkerEvent]) -> i32 {
    events
        .iter()
        .find_map(|event| match event {
            WorkerEvent::Exit(status) => Some(*status),
            _ => None,
        })
        .unwrap_or(-1)
}
