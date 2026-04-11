//! Component-facing mirror of [`wasmsh_protocol::WorkerEvent`].
//!
//! The WIT-generated types from `wit/world.wit` live behind the
//! `component-export` feature and only exist on `wasm32-wasip2`. Tests on the
//! host target need a total, value-based conversion that does not depend on
//! the generated bindings, so this module owns a plain-Rust mirror of the
//! `event` variant and the `diagnostic-level` enum plus a pair of `From`
//! conversions from the canonical protocol types.

use wasmsh_protocol::{DiagnosticLevel, WorkerEvent};

/// Component-facing diagnostic severity, mirroring
/// [`wasmsh_protocol::DiagnosticLevel`] as a plain `enum` so host-native tests
/// do not need the wit-bindgen runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentDiagnosticLevel {
    Info,
    Warning,
    Error,
    Trace,
}

impl From<DiagnosticLevel> for ComponentDiagnosticLevel {
    fn from(level: DiagnosticLevel) -> Self {
        // `DiagnosticLevel` is `#[non_exhaustive]`, so the wildcard arm is
        // mandatory. We map unknown future variants to `Info` (the safest
        // user-visible level); when a new variant is added upstream this
        // function should be updated to name it explicitly.
        match level {
            DiagnosticLevel::Warning => Self::Warning,
            DiagnosticLevel::Error => Self::Error,
            DiagnosticLevel::Trace => Self::Trace,
            DiagnosticLevel::Info | _ => Self::Info,
        }
    }
}

/// Component-facing event mirror of [`wasmsh_protocol::WorkerEvent`].
///
/// The variant set matches the WIT `event` variant in `wit/world.wit` one-for-one.
/// Converting a `WorkerEvent` into a `ComponentEvent` is total.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComponentEvent {
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
    Exit(i32),
    Yielded,
    Diagnostic(ComponentDiagnosticLevel, String),
    FsChanged(String),
    Version(String),
}

impl From<WorkerEvent> for ComponentEvent {
    fn from(event: WorkerEvent) -> Self {
        match event {
            WorkerEvent::Stdout(data) => Self::Stdout(data),
            WorkerEvent::Stderr(data) => Self::Stderr(data),
            WorkerEvent::Exit(status) => Self::Exit(status),
            WorkerEvent::Yielded => Self::Yielded,
            WorkerEvent::Diagnostic(level, message) => Self::Diagnostic(level.into(), message),
            WorkerEvent::FsChanged(path) => Self::FsChanged(path),
            WorkerEvent::Version(version) => Self::Version(version),
            // `WorkerEvent` is `#[non_exhaustive]`; map future variants to a
            // trace-level diagnostic until a matching WIT variant is added.
            _ => Self::Diagnostic(
                ComponentDiagnosticLevel::Trace,
                "unknown WorkerEvent variant".to_string(),
            ),
        }
    }
}

/// Map an owned slice of protocol events to their component-facing mirrors.
pub fn events_from_worker(events: Vec<WorkerEvent>) -> Vec<ComponentEvent> {
    events.into_iter().map(ComponentEvent::from).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_level_mapping_is_total() {
        assert_eq!(
            ComponentDiagnosticLevel::from(DiagnosticLevel::Info),
            ComponentDiagnosticLevel::Info
        );
        assert_eq!(
            ComponentDiagnosticLevel::from(DiagnosticLevel::Warning),
            ComponentDiagnosticLevel::Warning
        );
        assert_eq!(
            ComponentDiagnosticLevel::from(DiagnosticLevel::Error),
            ComponentDiagnosticLevel::Error
        );
        assert_eq!(
            ComponentDiagnosticLevel::from(DiagnosticLevel::Trace),
            ComponentDiagnosticLevel::Trace
        );
    }

    #[test]
    fn worker_event_to_component_event_roundtrips() {
        let cases = vec![
            WorkerEvent::Stdout(b"hello".to_vec()),
            WorkerEvent::Stderr(b"err".to_vec()),
            WorkerEvent::Exit(0),
            WorkerEvent::Yielded,
            WorkerEvent::Diagnostic(DiagnosticLevel::Warning, "warn".to_string()),
            WorkerEvent::FsChanged("/tmp/a".to_string()),
            WorkerEvent::Version("0.1.0".to_string()),
        ];
        let mapped = events_from_worker(cases);
        assert_eq!(
            mapped,
            vec![
                ComponentEvent::Stdout(b"hello".to_vec()),
                ComponentEvent::Stderr(b"err".to_vec()),
                ComponentEvent::Exit(0),
                ComponentEvent::Yielded,
                ComponentEvent::Diagnostic(ComponentDiagnosticLevel::Warning, "warn".to_string()),
                ComponentEvent::FsChanged("/tmp/a".to_string()),
                ComponentEvent::Version("0.1.0".to_string()),
            ]
        );
    }
}
