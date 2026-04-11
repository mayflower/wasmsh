//! Component export layer: `wit-bindgen`-generated bindings plus the glue
//! that forwards every WIT call into a [`crate::SessionCore`].
//!
//! This module is compiled only when the `component-export` feature is
//! enabled, which in practice means only on `wasm32-wasip2`. The generated
//! bindings pull in a small amount of `wit-bindgen` runtime support that we
//! do not want to link into host-native unit tests.
//!
//! Session state lives per-resource: every call to the constructor creates
//! a fresh [`crate::SessionCore`] and the `GuestSession` wrapper holds it
//! in a `RefCell`. There is no process-wide singleton and no `static mut`.

use std::cell::RefCell;

use crate::core::{self, RuntimeConfig, SessionCore};
use crate::event::{ComponentDiagnosticLevel, ComponentEvent};

wit_bindgen::generate!({
    world: "wasmsh",
    path: "wit",
});

use exports::wasmsh::component::sandbox::{
    DiagnosticLevel as WitDiagnosticLevel, Event as WitEvent, Guest, GuestSession,
    RuntimeConfig as WitRuntimeConfig,
};

struct Component;

export!(Component);

impl Guest for Component {
    type Session = ComponentSession;

    fn run_once(config: WitRuntimeConfig, input: String) -> Vec<WitEvent> {
        let events = core::run_once(config_from_wit(config), input);
        events.into_iter().map(event_to_wit).collect()
    }
}

/// Adapter between the WIT `session` resource and the host-testable
/// [`SessionCore`]. Each WIT resource instance owns exactly one of these.
pub(crate) struct ComponentSession {
    // `RefCell` because WIT resource methods take `&Self`; the runtime
    // underneath needs `&mut self`. No threads are involved — the component
    // model guarantees single-threaded access per resource instance.
    inner: RefCell<SessionCore>,
}

impl GuestSession for ComponentSession {
    fn new(config: WitRuntimeConfig) -> Self {
        // `SessionCore` captures `HostCommand::Init` events internally and
        // replays them on the first `run` call, matching the WIT contract
        // documented on the `session` constructor.
        Self {
            inner: RefCell::new(SessionCore::new(config_from_wit(config))),
        }
    }

    fn run(&self, input: String) -> Vec<WitEvent> {
        let events = self.inner.borrow_mut().run(input);
        events.into_iter().map(event_to_wit).collect()
    }

    fn read_file(&self, path: String) -> Vec<WitEvent> {
        let events = self.inner.borrow_mut().read_file(path);
        events.into_iter().map(event_to_wit).collect()
    }

    fn write_file(&self, path: String, data: Vec<u8>) -> Vec<WitEvent> {
        let events = self.inner.borrow_mut().write_file(path, data);
        events.into_iter().map(event_to_wit).collect()
    }

    fn list_dir(&self, path: String) -> Vec<WitEvent> {
        let events = self.inner.borrow_mut().list_dir(path);
        events.into_iter().map(event_to_wit).collect()
    }

    fn mount(&self, path: String) -> Vec<WitEvent> {
        let events = self.inner.borrow_mut().mount(path);
        events.into_iter().map(event_to_wit).collect()
    }
}

fn config_from_wit(config: WitRuntimeConfig) -> RuntimeConfig {
    RuntimeConfig {
        step_budget: config.step_budget,
        allowed_hosts: config.allowed_hosts,
    }
}

fn event_to_wit(event: ComponentEvent) -> WitEvent {
    match event {
        ComponentEvent::Stdout(data) => WitEvent::Stdout(data),
        ComponentEvent::Stderr(data) => WitEvent::Stderr(data),
        ComponentEvent::Exit(status) => WitEvent::Exit(status),
        ComponentEvent::Yielded => WitEvent::Yielded,
        ComponentEvent::Diagnostic(level, message) => {
            WitEvent::Diagnostic((diagnostic_to_wit(level), message))
        }
        ComponentEvent::FsChanged(path) => WitEvent::FsChanged(path),
        ComponentEvent::Version(version) => WitEvent::Version(version),
    }
}

fn diagnostic_to_wit(level: ComponentDiagnosticLevel) -> WitDiagnosticLevel {
    match level {
        ComponentDiagnosticLevel::Info => WitDiagnosticLevel::Info,
        ComponentDiagnosticLevel::Warning => WitDiagnosticLevel::Warning,
        ComponentDiagnosticLevel::Error => WitDiagnosticLevel::Error,
        ComponentDiagnosticLevel::Trace => WitDiagnosticLevel::Trace,
    }
}
