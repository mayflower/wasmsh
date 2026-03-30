//! Integration tests for the extracted runtime protocol.
//!
//! These tests verify that `WorkerRuntime` from the shared runtime crate
//! behaves identically to the original browser crate implementation:
//! Init returns Version, Run returns stdout/exit, WriteFile/ReadFile/ListDir
//! work end-to-end.

use wasmsh_protocol::{DiagnosticLevel, HostCommand, WorkerEvent, PROTOCOL_VERSION};
use wasmsh_runtime::WorkerRuntime;

fn get_stdout(events: &[WorkerEvent]) -> String {
    let mut out = Vec::new();
    for e in events {
        if let WorkerEvent::Stdout(data) = e {
            out.extend_from_slice(data);
        }
    }
    String::from_utf8(out).unwrap_or_default()
}

fn get_exit(events: &[WorkerEvent]) -> i32 {
    events
        .iter()
        .find_map(|e| {
            if let WorkerEvent::Exit(s) = e {
                Some(*s)
            } else {
                None
            }
        })
        .unwrap_or(-1)
}

#[test]
fn init_returns_protocol_version() {
    let mut rt = WorkerRuntime::new();
    let events = rt.handle_command(HostCommand::Init {
        step_budget: 0,
        allowed_hosts: vec![],
    });
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0],
        WorkerEvent::Version(PROTOCOL_VERSION.to_string())
    );
}

#[test]
fn run_echo_hello() {
    let mut rt = WorkerRuntime::new();
    rt.handle_command(HostCommand::Init {
        step_budget: 0,
        allowed_hosts: vec![],
    });
    let events = rt.handle_command(HostCommand::Run {
        input: "echo hello".into(),
    });
    assert_eq!(get_stdout(&events), "hello\n");
    assert_eq!(get_exit(&events), 0);
}

#[test]
fn write_file_then_read_file() {
    let mut rt = WorkerRuntime::new();
    rt.handle_command(HostCommand::Init {
        step_budget: 0,
        allowed_hosts: vec![],
    });

    let write_events = rt.handle_command(HostCommand::WriteFile {
        path: "/test.txt".into(),
        data: b"content".to_vec(),
    });
    assert!(write_events
        .iter()
        .any(|e| matches!(e, WorkerEvent::FsChanged(_))));

    let read_events = rt.handle_command(HostCommand::ReadFile {
        path: "/test.txt".into(),
    });
    assert_eq!(read_events, vec![WorkerEvent::Stdout(b"content".to_vec())]);
}

#[test]
fn list_dir_shows_written_files() {
    let mut rt = WorkerRuntime::new();
    rt.handle_command(HostCommand::Init {
        step_budget: 0,
        allowed_hosts: vec![],
    });

    rt.handle_command(HostCommand::WriteFile {
        path: "/a.txt".into(),
        data: vec![],
    });
    rt.handle_command(HostCommand::WriteFile {
        path: "/b.txt".into(),
        data: vec![],
    });

    let events = rt.handle_command(HostCommand::ListDir { path: "/".into() });
    let stdout = get_stdout(&events);
    assert!(stdout.contains("a.txt"), "got: {stdout}");
    assert!(stdout.contains("b.txt"), "got: {stdout}");
}

#[test]
fn run_not_initialized_returns_error() {
    let mut rt = WorkerRuntime::new();
    let events = rt.handle_command(HostCommand::Run {
        input: "echo hello".into(),
    });
    assert!(events
        .iter()
        .any(|e| matches!(e, WorkerEvent::Diagnostic(DiagnosticLevel::Error, _))));
}

#[test]
fn cancel_returns_diagnostic() {
    let mut rt = WorkerRuntime::new();
    rt.handle_command(HostCommand::Init {
        step_budget: 0,
        allowed_hosts: vec![],
    });
    let events = rt.handle_command(HostCommand::Cancel);
    assert!(events
        .iter()
        .any(|e| matches!(e, WorkerEvent::Diagnostic(DiagnosticLevel::Info, _))));
}

#[test]
fn cat_reads_vfs_file() {
    let mut rt = WorkerRuntime::new();
    rt.handle_command(HostCommand::Init {
        step_budget: 0,
        allowed_hosts: vec![],
    });
    rt.handle_command(HostCommand::WriteFile {
        path: "/hello.txt".into(),
        data: b"world".to_vec(),
    });
    let events = rt.handle_command(HostCommand::Run {
        input: "cat /hello.txt".into(),
    });
    assert_eq!(get_stdout(&events), "world");
    assert_eq!(get_exit(&events), 0);
}
