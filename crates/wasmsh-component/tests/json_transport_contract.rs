use wasmsh_component::JsonRuntimeHandle;
use wasmsh_protocol::{DiagnosticLevel, HostCommand, WorkerEvent, PROTOCOL_VERSION};

fn decode_events(payload: &str) -> Vec<WorkerEvent> {
    serde_json::from_str(payload).expect("bridge should return WorkerEvent JSON")
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
fn run_echo_hello_matches_protocol_json_shape() {
    let mut handle = JsonRuntimeHandle::new();
    let _ = handle.handle_json(
        &serde_json::to_string(&HostCommand::Init {
            step_budget: 100_000,
            allowed_hosts: Vec::new(),
        })
        .unwrap(),
    );
    let payload = handle.handle_json(
        &serde_json::to_string(&HostCommand::Run {
            input: "echo hello".to_string(),
        })
        .unwrap(),
    );
    let events = decode_events(&payload);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, WorkerEvent::Stdout(data) if data == b"hello\n")),
        "expected stdout hello event, got {events:?}"
    );
    assert!(
        matches!(events.last(), Some(WorkerEvent::Exit(0))),
        "expected trailing Exit(0), got {events:?}"
    );
}

#[test]
fn invalid_json_returns_serialized_error_diagnostic() {
    let mut handle = JsonRuntimeHandle::new();
    let payload = handle.handle_json("{ definitely not valid json");
    let events = decode_events(&payload);
    assert!(
        matches!(
            events.as_slice(),
            [WorkerEvent::Diagnostic(DiagnosticLevel::Error, message)]
                if message.contains("invalid JSON command")
        ),
        "expected invalid JSON diagnostic, got {events:?}"
    );
}

#[test]
fn write_then_read_roundtrips_through_json_commands() {
    let mut handle = JsonRuntimeHandle::new();
    let _ = handle.handle_json(
        &serde_json::to_string(&HostCommand::Init {
            step_budget: 100_000,
            allowed_hosts: Vec::new(),
        })
        .unwrap(),
    );
    let write = handle.handle_json(
        &serde_json::to_string(&HostCommand::WriteFile {
            path: "/workspace/hello.txt".to_string(),
            data: b"hello from json bridge".to_vec(),
        })
        .unwrap(),
    );
    assert_eq!(
        decode_events(&write),
        vec![WorkerEvent::FsChanged("/workspace/hello.txt".to_string())]
    );

    let read = handle.handle_json(
        &serde_json::to_string(&HostCommand::ReadFile {
            path: "/workspace/hello.txt".to_string(),
        })
        .unwrap(),
    );
    assert_eq!(
        decode_events(&read),
        vec![WorkerEvent::Stdout(b"hello from json bridge".to_vec())]
    );
}

#[test]
fn list_dir_sees_written_file() {
    let mut handle = JsonRuntimeHandle::new();
    let _ = handle.handle_json(
        &serde_json::to_string(&HostCommand::Init {
            step_budget: 100_000,
            allowed_hosts: Vec::new(),
        })
        .unwrap(),
    );
    let _ = handle.handle_json(
        &serde_json::to_string(&HostCommand::WriteFile {
            path: "/workspace/seen.txt".to_string(),
            data: b"x".to_vec(),
        })
        .unwrap(),
    );

    let list = handle.handle_json(
        &serde_json::to_string(&HostCommand::ListDir {
            path: "/workspace".to_string(),
        })
        .unwrap(),
    );
    let events = decode_events(&list);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, WorkerEvent::Stdout(data) if String::from_utf8_lossy(data).contains("seen.txt"))),
        "expected directory listing containing seen.txt, got {events:?}"
    );
}

#[test]
fn cancel_returns_informational_diagnostic_payload() {
    let mut handle = JsonRuntimeHandle::new();
    let _ = handle.handle_json(
        &serde_json::to_string(&HostCommand::Init {
            step_budget: 100_000,
            allowed_hosts: Vec::new(),
        })
        .unwrap(),
    );
    let cancel = handle.handle_json(&serde_json::to_string(&HostCommand::Cancel).unwrap());
    assert_eq!(
        decode_events(&cancel),
        vec![WorkerEvent::Diagnostic(
            DiagnosticLevel::Info,
            "cancel received".to_string(),
        )]
    );
}
