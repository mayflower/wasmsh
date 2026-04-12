//! Integration tests for the extracted runtime protocol.
//!
//! These tests verify that `WorkerRuntime` from the shared runtime crate
//! behaves identically to the original browser crate implementation:
//! Init returns Version, Run returns stdout/exit, WriteFile/ReadFile/ListDir
//! work end-to-end.

mod common;

use common::{get_exit, get_stderr, get_stdout};
use wasmsh_ast::WordPart;
use wasmsh_hir::HirCommand;
use wasmsh_protocol::{DiagnosticLevel, HostCommand, WorkerEvent, PROTOCOL_VERSION};
use wasmsh_runtime::{ExecutionPoll, ExternalCommandResult, WorkerRuntime};

fn new_runtime(step_budget: u32) -> WorkerRuntime {
    let mut rt = WorkerRuntime::new();
    rt.handle_command(HostCommand::Init {
        step_budget: step_budget.into(),
        allowed_hosts: vec![],
    });
    rt
}

fn install_hosterr(rt: &mut WorkerRuntime) {
    rt.set_external_handler(Box::new(|name, _argv, _stdin| {
        if name != "hosterr" {
            return None;
        }
        Some(ExternalCommandResult {
            stdout: Vec::new(),
            stderr: b"ERR\n".to_vec(),
            status: 0,
        })
    }));
}

fn collect_execution_events(rt: &mut WorkerRuntime) -> Vec<WorkerEvent> {
    let mut events = Vec::new();
    while let Some(poll) = rt.poll_active_run() {
        match poll {
            ExecutionPoll::Yield(mut batch) => events.append(&mut batch),
            ExecutionPoll::Done(mut batch) => {
                events.append(&mut batch);
                break;
            }
        }
    }
    events
}

fn has_yielded(events: &[WorkerEvent]) -> bool {
    events
        .iter()
        .any(|event| matches!(event, WorkerEvent::Yielded))
}

fn run_with_vm_subset(input: &str, enabled: bool) -> Vec<WorkerEvent> {
    let mut rt = new_runtime(0);
    rt.set_vm_subset_enabled(enabled);
    rt.handle_command(HostCommand::Run {
        input: input.into(),
    })
}

fn run_one_shot(input: &str, step_budget: u32) -> Vec<WorkerEvent> {
    let mut rt = new_runtime(step_budget);
    rt.handle_command(HostCommand::Run {
        input: input.into(),
    })
}

fn run_progressive(input: &str, step_budget: u32) -> Vec<WorkerEvent> {
    let mut rt = new_runtime(step_budget);
    let start = rt.handle_command(HostCommand::StartRun {
        input: input.into(),
    });
    assert_eq!(start, vec![WorkerEvent::Yielded]);

    let mut events = Vec::new();
    loop {
        let batch = rt.handle_command(HostCommand::PollRun);
        let finished = get_exit(&batch) >= 0;
        events.extend(
            batch
                .into_iter()
                .filter(|event| !matches!(event, WorkerEvent::Yielded)),
        );
        if finished {
            break;
        }
    }
    events
}

fn assert_one_shot_matches_progressive(input: &str, step_budget: u32) {
    let direct = run_one_shot(input, step_budget);
    let progressive = run_progressive(input, step_budget);
    assert_eq!(direct, progressive, "input: {input}");
}

fn assert_vm_subset_matches_fallback(input: &str) {
    let vm_events = run_with_vm_subset(input, true);
    let fallback_events = run_with_vm_subset(input, false);
    assert_eq!(vm_events, fallback_events, "input: {input}");
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
fn simple_command_runs_with_single_step_budget() {
    let mut rt = WorkerRuntime::new();
    rt.handle_command(HostCommand::Init {
        step_budget: 1,
        allowed_hosts: vec![],
    });
    let events = rt.handle_command(HostCommand::Run {
        input: "echo hello".into(),
    });
    assert_eq!(get_stdout(&events), "hello\n");
    assert_eq!(get_exit(&events), 0);
}

#[test]
fn parser_lowers_compound_array_assignment_as_assignment_command() {
    let ast = wasmsh_parse::parse("arr=(one two three)").expect("parse succeeds");
    let hir = wasmsh_hir::lower(&ast);
    let HirCommand::Assign(assign) = &hir.items[0].list[0].first.commands[0] else {
        panic!("expected assignment command");
    };
    let value = assign.assignments[0]
        .value
        .as_ref()
        .expect("compound value");
    assert_eq!(value.parts.len(), 1);
    assert_eq!(value.parts[0], WordPart::Literal("(one two three)".into()));
}

#[test]
fn parser_preserves_brace_words_as_literals() {
    let ast = wasmsh_parse::parse("echo {1..3}").expect("parse succeeds");
    let hir = wasmsh_hir::lower(&ast);
    let HirCommand::Exec(exec) = &hir.items[0].list[0].first.commands[0] else {
        panic!("expected exec command");
    };
    assert_eq!(exec.argv.len(), 2);
    assert_eq!(exec.argv[1].parts.len(), 1);
    assert_eq!(exec.argv[1].parts[0], WordPart::Literal("{1..3}".into()));
}

#[test]
fn compound_array_assignment_preserves_indexed_array_state() {
    let mut rt = new_runtime(0);
    let events = rt.handle_command(HostCommand::Run {
        input: "arr=(one two three)\necho ${arr[0]}\necho ${#arr[@]}\necho \"${arr[@]}\"".into(),
    });
    assert_eq!(get_stdout(&events), "one\n3\none two three\n");
    assert_eq!(get_exit(&events), 0);
}

#[test]
fn single_stage_literal_commands_apply_brace_and_glob_expansion() {
    let mut rt = new_runtime(0);
    rt.handle_command(HostCommand::WriteFile {
        path: "/ab".into(),
        data: Vec::new(),
    });
    rt.handle_command(HostCommand::WriteFile {
        path: "/ac".into(),
        data: Vec::new(),
    });

    let events = rt.handle_command(HostCommand::Run {
        input: "shopt -s nullglob\necho {1..3}\necho /a?\necho *.missing".into(),
    });
    assert_eq!(get_stdout(&events), "1 2 3\n/ab /ac\n\n");
    assert_eq!(get_exit(&events), 0);
}

#[test]
fn set_x_traces_single_stage_commands() {
    let mut rt = new_runtime(0);
    let events = rt.handle_command(HostCommand::Run {
        input: "set -x\necho hello\nset +x".into(),
    });
    assert_eq!(get_stdout(&events), "hello\n");
    assert!(get_stderr(&events).contains("echo hello"));
    assert_eq!(get_exit(&events), 0);
}

#[test]
fn read_delimiter_consumes_pipeline_stdin_inside_group() {
    let mut rt = new_runtime(0);
    let events = rt.handle_command(HostCommand::Run {
        input: "echo -n \"a:b:c\" | { read -d ':' first; echo \"$first\"; }".into(),
    });
    assert_eq!(get_stdout(&events), "a\n");
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
fn vm_subset_matches_fallback_for_echo_hello() {
    let vm_events = run_with_vm_subset("echo hello", true);
    let fallback_events = run_with_vm_subset("echo hello", false);
    assert_eq!(vm_events, fallback_events);
}

#[test]
fn vm_subset_matches_fallback_for_assignment_then_echo() {
    let input = "FOO=bar; echo $FOO";
    let vm_events = run_with_vm_subset(input, true);
    let fallback_events = run_with_vm_subset(input, false);
    assert_eq!(vm_events, fallback_events);
}

#[test]
fn vm_subset_matches_fallback_for_short_circuit_lists() {
    for input in ["true && echo ok", "false || echo ok"] {
        let vm_events = run_with_vm_subset(input, true);
        let fallback_events = run_with_vm_subset(input, false);
        assert_eq!(vm_events, fallback_events, "input: {input}");
    }
}

#[test]
fn vm_subset_matches_fallback_for_builtin_stdout_redirection() {
    let input = "echo hello > /out.txt; cat /out.txt";
    let vm_events = run_with_vm_subset(input, true);
    let fallback_events = run_with_vm_subset(input, false);
    assert_eq!(vm_events, fallback_events);
}

#[test]
fn invariant_yield_resume_equivalence_matrix() {
    for input in [
        "echo hello",
        "echo hello | cat",
        "echo hello > /out.txt; cat /out.txt",
        "FOO=bar; echo $FOO",
        "true && echo ok",
    ] {
        assert_one_shot_matches_progressive(input, 1);
    }
}

#[test]
fn invariant_cancellation_isolation_after_pipeline_yield() {
    let mut rt = new_runtime(1);

    rt.start_execution("echo first | cat; echo second".into())
        .expect("start execution");
    match rt.poll_active_run().expect("first poll") {
        ExecutionPoll::Yield(events) => assert_eq!(get_stdout(&events), "first\n"),
        ExecutionPoll::Done(events) => panic!("expected yield, got done: {events:?}"),
    }

    let cancel = rt.handle_command(HostCommand::Cancel);
    assert!(cancel.iter().any(
        |event| matches!(event, WorkerEvent::Diagnostic(DiagnosticLevel::Info, msg) if msg == "cancel received")
    ));
    match rt.poll_active_run().expect("cancelled poll") {
        ExecutionPoll::Done(events) => assert_eq!(get_exit(&events), 130),
        ExecutionPoll::Yield(events) => panic!("expected completion, got yield: {events:?}"),
    }

    let recovered = rt.handle_command(HostCommand::Run {
        input: "echo recovered | cat".into(),
    });
    let clean = run_one_shot("echo recovered | cat", 1);
    assert_eq!(recovered, clean);
}

#[test]
fn invariant_vm_subset_fallback_equivalence_matrix() {
    for input in [
        "echo hello",
        "FOO=bar; echo $FOO",
        "true && echo ok",
        "false || echo ok",
        "echo hello > /out.txt; cat /out.txt",
    ] {
        assert_vm_subset_matches_fallback(input);
    }
}

#[test]
fn cancel_returns_diagnostic() {
    let mut rt = new_runtime(0);
    let events = rt.handle_command(HostCommand::Cancel);
    assert!(events
        .iter()
        .any(|e| matches!(e, WorkerEvent::Diagnostic(DiagnosticLevel::Info, _))));
}

#[test]
fn signal_runs_registered_trap_without_exiting_idle_shell() {
    let mut rt = new_runtime(0);
    let setup = rt.handle_command(HostCommand::Run {
        input: "trap 'echo term' TERM".into(),
    });
    assert_eq!(get_exit(&setup), 0);

    let events = rt.handle_command(HostCommand::Signal {
        signal: "SIGTERM".into(),
    });
    assert_eq!(get_stdout(&events), "term\n");
    assert_eq!(get_exit(&events), -1);

    let recovered = rt.handle_command(HostCommand::Run {
        input: "echo recovered".into(),
    });
    assert_eq!(get_stdout(&recovered), "recovered\n");
    assert_eq!(get_exit(&recovered), 0);
}

#[test]
fn terminating_signal_runs_exit_trap_and_reports_bash_status() {
    let mut rt = new_runtime(0);
    let setup = rt.handle_command(HostCommand::Run {
        input: "trap 'echo cleanup' EXIT".into(),
    });
    assert_eq!(get_exit(&setup), 0);

    let events = rt.handle_command(HostCommand::Signal {
        signal: "15".into(),
    });
    assert_eq!(get_stdout(&events), "cleanup\n");
    assert_eq!(get_exit(&events), 143);

    let recovered = rt.handle_command(HostCommand::Run {
        input: "echo recovered".into(),
    });
    assert_eq!(get_stdout(&recovered), "recovered\n");
    assert_eq!(get_exit(&recovered), 0);
}

#[test]
fn trapped_signal_can_interrupt_progressive_run_without_forcing_exit() {
    let mut rt = new_runtime(1);
    let setup = rt.handle_command(HostCommand::Run {
        input: "trap 'seen=trapped' TERM".into(),
    });
    assert_eq!(get_exit(&setup), 0);

    let start = rt.handle_command(HostCommand::StartRun {
        input: "echo first; echo second".into(),
    });
    assert_eq!(start, vec![WorkerEvent::Yielded]);

    let first = rt.handle_command(HostCommand::PollRun);
    assert_eq!(get_stdout(&first), "first\n");
    assert!(has_yielded(&first));

    let signal = rt.handle_command(HostCommand::Signal {
        signal: "TERM".into(),
    });
    assert_eq!(get_stdout(&signal), "");
    assert_eq!(get_exit(&signal), -1);

    let second = rt.handle_command(HostCommand::PollRun);
    assert_eq!(get_stdout(&second), "second\n");
    assert_eq!(get_exit(&second), 0);

    let seen = rt.handle_command(HostCommand::Run {
        input: "echo ${seen-unset}".into(),
    });
    assert_eq!(get_stdout(&seen), "trapped\n");
}

#[test]
fn terminating_signal_stops_progressive_run_with_signal_exit_code() {
    let mut rt = new_runtime(1);

    let start = rt.handle_command(HostCommand::StartRun {
        input: "echo first; echo second".into(),
    });
    assert_eq!(start, vec![WorkerEvent::Yielded]);

    let first = rt.handle_command(HostCommand::PollRun);
    assert_eq!(get_stdout(&first), "first\n");
    assert!(has_yielded(&first));

    let signal = rt.handle_command(HostCommand::Signal {
        signal: "TERM".into(),
    });
    assert!(signal.iter().any(|event| {
        matches!(
            event,
            WorkerEvent::Diagnostic(DiagnosticLevel::Info, message)
                if message == "signal TERM received"
        )
    }));

    let done = rt.handle_command(HostCommand::PollRun);
    assert_eq!(get_stdout(&done), "");
    assert_eq!(get_exit(&done), 143);
}

#[test]
fn stop_like_signals_report_job_control_gap() {
    let mut rt = new_runtime(0);
    let events = rt.handle_command(HostCommand::Signal {
        signal: "TSTP".into(),
    });
    assert!(events.iter().any(|event| {
        matches!(
            event,
            WorkerEvent::Diagnostic(DiagnosticLevel::Warning, message)
                if message.contains("job-control stop semantics")
        )
    }));
}

#[test]
fn trap_inheritance_respects_errtrace_and_functrace_flags() {
    let mut rt = new_runtime(0);
    let debug_off = rt.handle_command(HostCommand::Run {
        input: "trap 'echo debug' DEBUG\nf(){ echo body; }\nf".into(),
    });
    assert_eq!(get_stdout(&debug_off), "debug\nbody\n");

    let mut rt = new_runtime(0);
    let debug_on = rt.handle_command(HostCommand::Run {
        input: "set -T\ntrap 'echo debug' DEBUG\nf(){ echo body; }\nf".into(),
    });
    assert_eq!(get_stdout(&debug_on), "debug\ndebug\ndebug\nbody\n");

    let mut rt = new_runtime(0);
    let return_off = rt.handle_command(HostCommand::Run {
        input: "trap 'echo return' RETURN\nf(){ echo body; }\nf".into(),
    });
    assert_eq!(get_stdout(&return_off), "body\n");

    let mut rt = new_runtime(0);
    let return_on = rt.handle_command(HostCommand::Run {
        input: "set -T\ntrap 'echo return' RETURN\nf(){ echo body; }\nf".into(),
    });
    assert_eq!(get_stdout(&return_on), "body\nreturn\n");

    let mut rt = new_runtime(0);
    let err_off = rt.handle_command(HostCommand::Run {
        input: "trap 'echo err' ERR\nf(){ false; }\nf\necho status:$?".into(),
    });
    assert_eq!(get_stdout(&err_off), "err\nstatus:1\n");

    let mut rt = new_runtime(0);
    let err_on = rt.handle_command(HostCommand::Run {
        input: "set -E\ntrap 'echo err' ERR\nf(){ false; }\nf\necho status:$?".into(),
    });
    assert_eq!(get_stdout(&err_on), "err\nerr\nstatus:1\n");
}

#[test]
fn active_execution_yields_and_resumes_with_small_budget() {
    let mut rt = new_runtime(1);

    rt.start_execution("echo one; echo two".into()).unwrap();

    let first = rt.poll_active_run().expect("first poll");
    match first {
        ExecutionPoll::Yield(events) => {
            assert_eq!(get_stdout(&events), "one\n");
            assert_eq!(get_exit(&events), -1);
        }
        ExecutionPoll::Done(events) => panic!("expected yield, got done: {events:?}"),
    }

    let second = rt.poll_active_run().expect("second poll");
    match second {
        ExecutionPoll::Done(events) => {
            assert_eq!(get_stdout(&events), "two\n");
            assert_eq!(get_exit(&events), 0);
        }
        ExecutionPoll::Yield(events) => panic!("expected completion, got yield: {events:?}"),
    }
}

#[test]
fn active_execution_matches_run_wrapper_output() {
    let mut direct = new_runtime(1);
    let mut resumable = new_runtime(1);

    let input = "echo left; hosterr; echo right";
    install_hosterr(&mut direct);
    install_hosterr(&mut resumable);

    let direct_events = direct.handle_command(HostCommand::Run {
        input: input.into(),
    });
    resumable.start_execution(input.into()).unwrap();
    let resumed_events = collect_execution_events(&mut resumable);

    assert_eq!(direct_events, resumed_events);
}

#[test]
fn cancelling_active_execution_returns_clean_exit_and_next_run_recovers() {
    let mut rt = new_runtime(1);

    rt.start_execution("echo first; echo second".into())
        .unwrap();
    match rt.poll_active_run().expect("first poll") {
        ExecutionPoll::Yield(events) => assert_eq!(get_stdout(&events), "first\n"),
        ExecutionPoll::Done(events) => panic!("expected yield, got done: {events:?}"),
    }

    let cancel = rt.handle_command(HostCommand::Cancel);
    assert!(cancel.iter().any(
        |event| matches!(event, WorkerEvent::Diagnostic(DiagnosticLevel::Info, msg) if msg == "cancel received")
    ));

    let cancelled = rt.poll_active_run().expect("cancelled poll");
    match cancelled {
        ExecutionPoll::Done(events) => {
            assert_eq!(get_stdout(&events), "");
            assert_eq!(get_exit(&events), 130);
        }
        ExecutionPoll::Yield(events) => panic!("expected clean cancel, got yield: {events:?}"),
    }

    let recovered = rt.handle_command(HostCommand::Run {
        input: "echo recovered".into(),
    });
    assert_eq!(get_stdout(&recovered), "recovered\n");
    assert_eq!(get_exit(&recovered), 0);
}

#[test]
fn yielded_execution_does_not_leak_unreached_state_into_next_run() {
    let mut rt = new_runtime(1);

    rt.start_execution("echo ready; foo=after-yield".into())
        .unwrap();
    match rt.poll_active_run().expect("first poll") {
        ExecutionPoll::Yield(events) => assert_eq!(get_stdout(&events), "ready\n"),
        ExecutionPoll::Done(events) => panic!("expected yield, got done: {events:?}"),
    }

    rt.handle_command(HostCommand::Cancel);
    match rt.poll_active_run().expect("cancelled poll") {
        ExecutionPoll::Done(events) => assert_eq!(get_exit(&events), 130),
        ExecutionPoll::Yield(events) => panic!("expected completion, got yield: {events:?}"),
    }

    let next = rt.handle_command(HostCommand::Run {
        input: "echo ${foo-unset}".into(),
    });
    assert_eq!(get_stdout(&next), "unset\n");
    assert_eq!(get_exit(&next), 0);
}

#[test]
fn start_run_acknowledges_active_execution_without_finishing() {
    let mut rt = new_runtime(1);

    let start = rt.handle_command(HostCommand::StartRun {
        input: "echo one; echo two".into(),
    });
    assert_eq!(start, vec![WorkerEvent::Yielded]);

    let first_poll = rt.handle_command(HostCommand::PollRun);
    assert_eq!(get_stdout(&first_poll), "one\n");
    assert_eq!(get_exit(&first_poll), -1);
    assert!(has_yielded(&first_poll));
}

#[test]
fn poll_run_drains_incremental_output_until_exit() {
    let mut rt = new_runtime(1);

    rt.handle_command(HostCommand::StartRun {
        input: "echo one; echo two; echo three".into(),
    });

    let first = rt.handle_command(HostCommand::PollRun);
    assert_eq!(get_stdout(&first), "one\n");
    assert!(has_yielded(&first));

    let second = rt.handle_command(HostCommand::PollRun);
    assert_eq!(get_stdout(&second), "two\n");
    assert!(has_yielded(&second));

    let third = rt.handle_command(HostCommand::PollRun);
    assert_eq!(get_stdout(&third), "three\n");
    assert_eq!(get_exit(&third), 0);
    assert!(!has_yielded(&third));
}

#[test]
fn progressive_cancel_and_restart_work_normally() {
    let mut rt = new_runtime(1);

    rt.handle_command(HostCommand::StartRun {
        input: "echo first; echo second".into(),
    });
    let first = rt.handle_command(HostCommand::PollRun);
    assert_eq!(get_stdout(&first), "first\n");
    assert!(has_yielded(&first));

    let cancel = rt.handle_command(HostCommand::Cancel);
    assert!(cancel.iter().any(
        |event| matches!(event, WorkerEvent::Diagnostic(DiagnosticLevel::Info, msg) if msg == "cancel received")
    ));

    let cancelled = rt.handle_command(HostCommand::PollRun);
    assert_eq!(get_exit(&cancelled), 130);
    assert!(!has_yielded(&cancelled));

    let restart = rt.handle_command(HostCommand::StartRun {
        input: "echo restarted".into(),
    });
    assert_eq!(restart, vec![WorkerEvent::Yielded]);
    let restarted = rt.handle_command(HostCommand::PollRun);
    assert_eq!(get_stdout(&restarted), "restarted\n");
    assert_eq!(get_exit(&restarted), 0);
}

#[test]
fn run_remains_compatible_with_progressive_protocol_added() {
    let mut one_shot = new_runtime(1);
    let mut progressive = new_runtime(1);

    install_hosterr(&mut one_shot);
    install_hosterr(&mut progressive);
    let input = "echo left; hosterr; echo right";

    let run_events = one_shot.handle_command(HostCommand::Run {
        input: input.into(),
    });
    let start = progressive.handle_command(HostCommand::StartRun {
        input: input.into(),
    });
    assert_eq!(start, vec![WorkerEvent::Yielded]);

    let mut progressive_events = Vec::new();
    loop {
        let batch = progressive.handle_command(HostCommand::PollRun);
        progressive_events.extend(batch.clone());
        if get_exit(&batch) >= 0 {
            break;
        }
    }
    progressive_events.retain(|event| !matches!(event, WorkerEvent::Yielded));

    assert_eq!(run_events, progressive_events);
}

#[test]
fn cat_reads_vfs_file() {
    let mut rt = new_runtime(0);
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

#[test]
fn input_redirection_reads_vfs_file_lazily() {
    let mut rt = new_runtime(0);
    rt.handle_command(HostCommand::WriteFile {
        path: "/input.txt".into(),
        data: b"redirected".to_vec(),
    });
    let events = rt.handle_command(HostCommand::Run {
        input: "cat < /input.txt".into(),
    });
    assert_eq!(get_stdout(&events), "redirected");
    assert_eq!(get_exit(&events), 0);
}

#[test]
fn run_recovers_after_exit_builtin() {
    let mut rt = new_runtime(0);

    // `exit 42` should return exit code 42
    let events = rt.handle_command(HostCommand::Run {
        input: "exit 42".into(),
    });
    assert_eq!(get_exit(&events), 42);

    // Subsequent commands must still work — exit_requested must not persist
    let events = rt.handle_command(HostCommand::Run {
        input: "echo recovered".into(),
    });
    assert_eq!(get_stdout(&events), "recovered\n");
    assert_eq!(get_exit(&events), 0);
}

#[test]
fn single_quoted_glob_not_expanded() {
    let mut rt = WorkerRuntime::new();
    rt.handle_command(HostCommand::Init {
        step_budget: 0,
        allowed_hosts: vec![],
    });
    // Create a file whose name matches [a-z]*
    rt.handle_command(HostCommand::WriteFile {
        path: "/testfile".into(),
        data: vec![],
    });
    // Single-quoted string must NOT be glob-expanded
    let events = rt.handle_command(HostCommand::Run {
        input: "echo '[a-z]*'".into(),
    });
    assert_eq!(get_stdout(&events), "[a-z]*\n");
    assert_eq!(get_exit(&events), 0);
}

#[test]
fn external_handler_receives_pipeline_stdin() {
    let mut rt = WorkerRuntime::new();
    rt.set_external_handler(Box::new(|name, _argv, stdin| {
        if name != "hostcat" {
            return None;
        }
        let stdout = stdin
            .map(|mut stdin| {
                let mut out = Vec::new();
                let mut buffer = [0u8; 4096];
                loop {
                    match stdin.read_chunk(&mut buffer) {
                        Ok(0) => break,
                        Ok(read) => out.extend_from_slice(&buffer[..read]),
                        Err(_) => return Vec::new(),
                    }
                }
                out
            })
            .unwrap_or_default();
        Some(ExternalCommandResult {
            stdout,
            stderr: Vec::new(),
            status: 0,
        })
    }));
    rt.handle_command(HostCommand::Init {
        step_budget: 0,
        allowed_hosts: vec![],
    });

    let events = rt.handle_command(HostCommand::Run {
        input: "printf hi | hostcat".into(),
    });
    assert_eq!(get_stdout(&events), "hi");
    assert_eq!(get_exit(&events), 0);
}

#[test]
fn external_handler_2dup1_then_stdout_redirect_keeps_stderr_visible() {
    let mut rt = WorkerRuntime::new();
    install_hosterr(&mut rt);
    rt.handle_command(HostCommand::Init {
        step_budget: 0,
        allowed_hosts: vec![],
    });

    let events = rt.handle_command(HostCommand::Run {
        input: "hosterr 2>&1 > /out.txt; cat /out.txt".into(),
    });

    assert_eq!(get_stdout(&events), "ERR\n");
    assert_eq!(get_stderr(&events), "");

    let file = rt.handle_command(HostCommand::ReadFile {
        path: "/out.txt".into(),
    });
    assert_eq!(get_stdout(&file), "");
}

#[test]
fn external_handler_stdout_redirect_then_2dup1_captures_stderr_in_file() {
    let mut rt = WorkerRuntime::new();
    install_hosterr(&mut rt);
    rt.handle_command(HostCommand::Init {
        step_budget: 0,
        allowed_hosts: vec![],
    });

    let events = rt.handle_command(HostCommand::Run {
        input: "hosterr > /out.txt 2>&1; cat /out.txt".into(),
    });

    assert_eq!(get_stdout(&events), "ERR\n");
    assert_eq!(get_stderr(&events), "");

    let file = rt.handle_command(HostCommand::ReadFile {
        path: "/out.txt".into(),
    });
    assert_eq!(get_stdout(&file), "ERR\n");
}

#[test]
fn external_handler_2dup1_into_pipeline_sends_stderr_to_stdout_pipe() {
    let mut rt = WorkerRuntime::new();
    install_hosterr(&mut rt);
    rt.handle_command(HostCommand::Init {
        step_budget: 0,
        allowed_hosts: vec![],
    });

    let events = rt.handle_command(HostCommand::Run {
        input: "hosterr 2>&1 | cat".into(),
    });

    assert_eq!(get_stdout(&events), "ERR\n");
    assert_eq!(get_stderr(&events), "");
}
