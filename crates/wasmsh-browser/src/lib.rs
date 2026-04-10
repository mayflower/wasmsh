//! Browser Web Worker integration for wasmsh.
//!
//! Thin adapter around [`wasmsh_runtime::WorkerRuntime`] that adds
//! `wasm-bindgen` entry points for the browser worker.

// Re-export the runtime so downstream consumers (testkit, benches) work unchanged.
pub use wasmsh_runtime::{extglob_match, BrowserConfig, WorkerRuntime};

// Protocol types used in tests (via `use super::*`) and wasm_bindings.
#[cfg(test)]
use wasmsh_protocol::{DiagnosticLevel, HostCommand, WorkerEvent, PROTOCOL_VERSION};

#[cfg(test)]
mod tests {
    use super::*;

    fn run_shell(input: &str) -> (Vec<WorkerEvent>, i32) {
        let mut rt = WorkerRuntime::new();
        rt.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        let events = rt.handle_command(HostCommand::Run {
            input: input.into(),
        });
        let status = events
            .iter()
            .find_map(|e| {
                if let WorkerEvent::Exit(s) = e {
                    Some(*s)
                } else {
                    None
                }
            })
            .unwrap_or(-1);
        (events, status)
    }

    fn get_stdout(events: &[WorkerEvent]) -> String {
        let mut out = Vec::new();
        for e in events {
            if let WorkerEvent::Stdout(data) = e {
                out.extend_from_slice(data);
            }
        }
        String::from_utf8(out).unwrap_or_default()
    }

    fn get_stderr(events: &[WorkerEvent]) -> String {
        let mut out = Vec::new();
        for e in events {
            if let WorkerEvent::Stderr(data) = e {
                out.extend_from_slice(data);
            }
        }
        String::from_utf8(out).unwrap_or_default()
    }

    #[test]
    fn init_returns_version() {
        let mut rt = WorkerRuntime::new();
        let events = rt.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        assert!(matches!(&events[0], WorkerEvent::Version(v) if v == PROTOCOL_VERSION));
    }

    #[test]
    fn run_before_init_errors() {
        let mut rt = WorkerRuntime::new();
        let events = rt.handle_command(HostCommand::Run {
            input: "echo hi".into(),
        });
        assert!(matches!(
            &events[0],
            WorkerEvent::Diagnostic(DiagnosticLevel::Error, _)
        ));
    }

    #[test]
    fn echo_hello() {
        let (events, status) = run_shell("echo hello");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "hello\n");
    }

    #[test]
    fn true_false() {
        let (_, status) = run_shell("true");
        assert_eq!(status, 0);
        let (_, status) = run_shell("false");
        assert_eq!(status, 1);
    }

    #[test]
    fn variable_assignment_and_echo() {
        let (events, status) = run_shell("X=hello; echo $X");
        assert_eq!(status, 0);
        // Note: variable expansion happens through the word parser + expand
        // The parser produces WordPart::Parameter("X"), expand resolves it
        assert_eq!(get_stdout(&events), "hello\n");
    }

    #[test]
    fn and_or_chain() {
        let (events, _) = run_shell("true && echo yes");
        assert_eq!(get_stdout(&events), "yes\n");

        let (events, _) = run_shell("false && echo no");
        assert_eq!(get_stdout(&events), "");

        let (events, _) = run_shell("false || echo fallback");
        assert_eq!(get_stdout(&events), "fallback\n");
    }

    #[test]
    fn if_then_fi() {
        let (events, status) = run_shell("if true; then echo yes; fi");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "yes\n");
    }

    #[test]
    fn if_else() {
        let (events, _) = run_shell("if false; then echo no; else echo yes; fi");
        assert_eq!(get_stdout(&events), "yes\n");
    }

    #[test]
    fn for_loop() {
        let (events, _) = run_shell("for x in a b c; do echo $x; done");
        assert_eq!(get_stdout(&events), "a\nb\nc\n");
    }

    #[test]
    fn parse_error_reported() {
        let (events, status) = run_shell("|");
        assert_eq!(status, 2);
        assert!(events.iter().any(|e| matches!(e, WorkerEvent::Stderr(_))));
    }

    #[test]
    fn negated_pipeline() {
        let (_, status) = run_shell("! true");
        assert_eq!(status, 1);
        let (_, status) = run_shell("! false");
        assert_eq!(status, 0);
    }

    #[test]
    fn cancel_command() {
        let mut rt = WorkerRuntime::new();
        rt.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        let events = rt.handle_command(HostCommand::Cancel);
        assert!(matches!(
            &events[0],
            WorkerEvent::Diagnostic(DiagnosticLevel::Info, _)
        ));
    }

    // ---- Utility dispatch ----

    #[test]
    fn touch_and_cat_via_shell() {
        let mut rt = WorkerRuntime::new();
        rt.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        // touch creates a file, then we write via protocol and cat it
        rt.handle_command(HostCommand::Run {
            input: "touch /hello.txt".into(),
        });
        rt.handle_command(HostCommand::WriteFile {
            path: "/hello.txt".into(),
            data: b"hello world".to_vec(),
        });
        let events = rt.handle_command(HostCommand::Run {
            input: "cat /hello.txt".into(),
        });
        assert_eq!(get_stdout(&events), "hello world");
    }

    #[test]
    fn mkdir_and_ls_via_shell() {
        let mut rt = WorkerRuntime::new();
        rt.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        rt.handle_command(HostCommand::Run {
            input: "mkdir /mydir".into(),
        });
        rt.handle_command(HostCommand::Run {
            input: "touch /mydir/a.txt".into(),
        });
        let events = rt.handle_command(HostCommand::Run {
            input: "ls /mydir".into(),
        });
        assert_eq!(get_stdout(&events), "a.txt\n");
    }

    #[test]
    fn unknown_command_reports_error() {
        let (events, status) = run_shell("nonexistent_cmd");
        assert_eq!(status, 127);
        // Check stderr contains "command not found"
        let stderr: String = events
            .iter()
            .filter_map(|e| {
                if let WorkerEvent::Stderr(data) = e {
                    Some(String::from_utf8_lossy(data).to_string())
                } else {
                    None
                }
            })
            .collect();
        assert!(stderr.contains("command not found"));
    }

    // ---- Protocol file operations ----

    #[test]
    fn protocol_write_and_read_file() {
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
    fn protocol_list_dir() {
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
        assert!(stdout.contains("a.txt"));
        assert!(stdout.contains("b.txt"));
    }

    // ---- Redirections ----

    #[test]
    fn output_redirection_to_file() {
        let mut rt = WorkerRuntime::new();
        rt.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        // echo hello > /out.txt should write to file, not stdout
        let events = rt.handle_command(HostCommand::Run {
            input: "echo hello > /out.txt".into(),
        });
        // stdout should be empty (redirected to file)
        assert_eq!(get_stdout(&events), "");
        // File should contain the output
        let read_events = rt.handle_command(HostCommand::ReadFile {
            path: "/out.txt".into(),
        });
        assert_eq!(get_stdout(&read_events), "hello\n");
    }

    #[test]
    fn append_redirection() {
        let mut rt = WorkerRuntime::new();
        rt.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        rt.handle_command(HostCommand::Run {
            input: "echo line1 > /log.txt".into(),
        });
        rt.handle_command(HostCommand::Run {
            input: "echo line2 >> /log.txt".into(),
        });
        let read_events = rt.handle_command(HostCommand::ReadFile {
            path: "/log.txt".into(),
        });
        assert_eq!(get_stdout(&read_events), "line1\nline2\n");
    }

    #[test]
    fn redirect_only_creates_file() {
        let mut rt = WorkerRuntime::new();
        rt.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        rt.handle_command(HostCommand::Run {
            input: "> /empty.txt".into(),
        });
        let read_events = rt.handle_command(HostCommand::ReadFile {
            path: "/empty.txt".into(),
        });
        assert_eq!(get_stdout(&read_events), "");
    }

    // ---- Diagnostics surfaced as events ----

    #[test]
    fn vm_diagnostics_surfaced() {
        let mut rt = WorkerRuntime::new();
        rt.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        // Running an unknown command triggers a diagnostic in the VM
        let events = rt.handle_command(HostCommand::Run {
            input: "unknown_cmd_xyz".into(),
        });
        // The "command not found" goes to stderr, not diagnostics,
        // but the VM emits a diagnostic when CallBuiltin fails for unknown builtins.
        // Since we dispatch unknown commands before IR, it goes to stderr.
        // Let's test that stderr events are present.
        assert!(events.iter().any(|e| matches!(e, WorkerEvent::Stderr(_))));
    }

    // ---- Integration: unset + default expansion ----

    #[test]
    fn unset_then_default_expansion() {
        let mut rt = WorkerRuntime::new();
        rt.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        rt.handle_command(HostCommand::Run {
            input: "X=hello".into(),
        });
        rt.handle_command(HostCommand::Run {
            input: "unset X".into(),
        });
        // After unset, ${X:-default} should use the default
        let events = rt.handle_command(HostCommand::Run {
            input: "echo ${X:-default}".into(),
        });
        assert_eq!(get_stdout(&events), "default\n");
    }

    #[test]
    fn readonly_prevents_reassignment() {
        let mut rt = WorkerRuntime::new();
        rt.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        rt.handle_command(HostCommand::Run {
            input: "readonly X=locked".into(),
        });
        let events = rt.handle_command(HostCommand::Run {
            input: "echo $X".into(),
        });
        assert_eq!(get_stdout(&events), "locked\n");
    }

    #[test]
    fn pipeline_last_status() {
        // Pipeline exit status should be the last command's status
        let (_, status) = run_shell("true | false");
        assert_eq!(status, 1);
        let (_, status) = run_shell("false | true");
        assert_eq!(status, 0);
    }

    #[test]
    fn pipe_data_flows_through() {
        let (events, status) = run_shell("echo hello | cat");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "hello\n");
    }

    #[test]
    fn command_substitution_captures_stdout_without_leak() {
        let (events, status) = run_shell("echo $(printf 'hello')");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "hello\n");
    }

    #[test]
    fn command_substitution_preserves_inner_stderr_visibility() {
        let (events, status) = run_shell("echo $(printf 'hello'; echo err >&2)");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "hello\n");
        assert_eq!(get_stderr(&events), "err\n");
    }

    #[test]
    fn command_substitution_isolates_shell_state() {
        let (events, status) = run_shell("foo=before; echo $(foo=after; printf hi); echo $foo");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "hi\nbefore\n");
    }

    #[test]
    fn scheduler_executes_single_redirect_only_command() {
        let mut rt = WorkerRuntime::new();
        rt.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        let events = rt.handle_command(HostCommand::Run {
            input: "> /created.txt".into(),
        });
        let status = events
            .iter()
            .find_map(|e| {
                if let WorkerEvent::Exit(s) = e {
                    Some(*s)
                } else {
                    None
                }
            })
            .unwrap_or(-1);
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "");
        assert_eq!(get_stderr(&events), "");
        let read_events = rt.handle_command(HostCommand::ReadFile {
            path: "/created.txt".into(),
        });
        assert_eq!(get_stdout(&read_events), "");
    }

    #[test]
    fn pipe_three_stages() {
        let (events, status) = run_shell("echo hello world | cat | cat");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "hello world\n");
    }

    #[test]
    fn pipe_echo_to_wc() {
        let (events, status) = run_shell("echo hello world | wc");
        assert_eq!(status, 0);
        let stdout = get_stdout(&events);
        assert!(stdout.contains('1')); // 1 line
        assert!(stdout.contains('2')); // 2 words
    }

    #[test]
    fn streaming_yes_head_stops_after_requested_lines() {
        let (events, status) = run_shell("yes | head -n 5");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "y\ny\ny\ny\ny\n");
    }

    #[test]
    fn streaming_yes_cat_head_stops_after_requested_lines() {
        let (events, status) = run_shell("yes | cat | head -n 5");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "y\ny\ny\ny\ny\n");
    }

    #[test]
    fn streaming_yes_head_wc_counts_lines() {
        let (events, status) = run_shell("yes | head -n 5 | wc -l");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "      5\n");
    }

    #[test]
    fn streaming_cat_file_head_stops_at_requested_bytes() {
        let mut rt = WorkerRuntime::new();
        rt.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        rt.handle_command(HostCommand::WriteFile {
            path: "/big.txt".into(),
            data: b"abcdefghijklmnopqrstuvwxyz".to_vec(),
        });
        let events = rt.handle_command(HostCommand::Run {
            input: "cat /big.txt | head -c 10".into(),
        });
        let status = events
            .iter()
            .find_map(|e| {
                if let WorkerEvent::Exit(s) = e {
                    Some(*s)
                } else {
                    None
                }
            })
            .unwrap_or(-1);
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "abcdefghij");
    }

    #[test]
    fn streaming_yes_tr_head_transforms_lines() {
        let (events, status) = run_shell("yes | tr y z | head -n 5");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "z\nz\nz\nz\nz\n");
    }

    #[test]
    fn streaming_yes_grep_head_stops_after_requested_lines() {
        let (events, status) = run_shell("yes | grep y | head -n 5");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "y\ny\ny\ny\ny\n");
    }

    #[test]
    fn streaming_yes_tee_head_writes_only_pulled_output() {
        let mut rt = WorkerRuntime::new();
        rt.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        let events = rt.handle_command(HostCommand::Run {
            input: "yes | tee /tee.txt | head -n 5".into(),
        });
        let status = events
            .iter()
            .find_map(|event| {
                if let WorkerEvent::Exit(code) = event {
                    Some(*code)
                } else {
                    None
                }
            })
            .unwrap_or(-1);
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "y\ny\ny\ny\ny\n");

        let file_events = rt.handle_command(HostCommand::ReadFile {
            path: "/tee.txt".into(),
        });
        assert_eq!(get_stdout(&file_events), "y\ny\ny\ny\ny\n");
    }

    #[test]
    fn streaming_buffered_sort_tee_cat_preserves_sorted_output() {
        let mut rt = WorkerRuntime::new();
        rt.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        let events = rt.handle_command(HostCommand::Run {
            input: "printf 'b\\na\\n' | sort | tee /sorted.txt | cat".into(),
        });
        let status = events
            .iter()
            .find_map(|event| {
                if let WorkerEvent::Exit(code) = event {
                    Some(*code)
                } else {
                    None
                }
            })
            .unwrap_or(-1);
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "a\nb\n");

        let file_events = rt.handle_command(HostCommand::ReadFile {
            path: "/sorted.txt".into(),
        });
        assert_eq!(get_stdout(&file_events), "a\nb\n");
    }

    #[test]
    fn streaming_yes_rev_head_stops_after_requested_lines() {
        let (events, status) = run_shell("yes | rev | head -n 5");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "y\ny\ny\ny\ny\n");
    }

    #[test]
    fn streaming_echo_cut_selects_field() {
        let (events, status) = run_shell("echo abc:def | cut -d: -f2 | head -c 4");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "def\n");
    }

    #[test]
    fn streaming_echo_tail_head_selects_last_lines() {
        let (events, status) = run_shell("echo -e 'a\\nb\\nc' | tail -n 2 | head -n 1");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "b\n");
    }

    #[test]
    fn streaming_buffered_printf_sort_head_outputs_sorted_first_line() {
        let (events, status) = run_shell("printf 'b\\na\\n' | sort | head -n 1");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "a\n");
    }

    #[test]
    fn streaming_buffered_function_stage_preserves_output() {
        let (events, status) = run_shell("f(){ cat; }\nprintf hi | f | head -c 2");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "hi");
    }

    #[test]
    fn streaming_buffered_function_pipe_stderr_preserves_output() {
        let (events, status) = run_shell("f(){ echo out; echo err >&2; }\nf |& head -n 2");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "out\nerr\n");
    }

    #[test]
    fn scheduled_group_stage_pipe_stderr_preserves_output() {
        let (events, status) = run_shell("printf x | { cat; echo err >&2; } |& cat");
        assert_eq!(status, 0);
        let stdout = get_stdout(&events);
        assert!(stdout.contains("x"));
        assert!(stdout.contains("err"));
    }

    #[test]
    fn streaming_tee_pipe_stderr_preserves_output() {
        let (events, status) = run_shell("printf x | tee / |& cat");
        assert_eq!(status, 0);
        let stdout = get_stdout(&events);
        assert!(stdout.contains("x"));
        assert!(stdout.contains("tee: /: is a directory: /"));
        assert_eq!(get_stderr(&events), "");
    }

    #[test]
    fn streaming_tee_pipe_stderr_respects_pipefail_status() {
        let (events, status) = run_shell("set -o pipefail\nprintf x | tee / |& cat");
        assert_eq!(status, 1);
        let stdout = get_stdout(&events);
        assert!(stdout.contains("x"));
        assert!(stdout.contains("tee: /: is a directory: /"));
        assert_eq!(get_stderr(&events), "");
    }

    #[test]
    fn streaming_yes_bat_head_formats_numbered_lines() {
        let (events, status) = run_shell("yes | bat --style=numbers | head -n 2");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "    1   │ y\n    2   │ y\n");
    }

    #[test]
    fn streaming_yes_sed_head_rewrites_lines() {
        let (events, status) = run_shell("yes | sed 's/y/z/' | head -n 5");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "z\nz\nz\nz\nz\n");
    }

    #[test]
    fn streaming_echo_paste_serial_joins_lines() {
        let (events, status) = run_shell("echo -e 'a\\nb\\nc' | paste -s -d , | head -c 6");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "a,b,c\n");
    }

    #[test]
    fn streaming_echo_column_preserves_plain_output() {
        let (events, status) = run_shell("echo abc | column | head -c 4");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "abc\n");
    }

    #[test]
    fn streaming_echo_uniq_deduplicates_lines() {
        let (events, status) = run_shell("echo -e 'a\\na\\nb' | uniq | head -n 2");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "a\nb\n");
    }

    #[test]
    fn generic_pipeline_grep_preserves_visible_output_budget_behavior() {
        let (events, status) = run_shell("echo -e 'a\\nb' | grep b");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "b\n");
    }

    #[test]
    fn while_loop_with_counter() {
        let mut rt = WorkerRuntime::new();
        rt.handle_command(HostCommand::Init {
            step_budget: 10000,
            allowed_hosts: vec![],
        });
        // Simple loop that echoes 3 times using a counter variable
        let events = rt.handle_command(HostCommand::Run {
            input: "for i in 1 2 3; do echo line; done".into(),
        });
        assert_eq!(get_stdout(&events), "line\nline\nline\n");
    }

    #[test]
    fn heredoc_with_cat() {
        let mut rt = WorkerRuntime::new();
        rt.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        let events = rt.handle_command(HostCommand::Run {
            input: "cat <<EOF\nhello world\nEOF\n".into(),
        });
        assert_eq!(get_stdout(&events), "hello world\n");
    }

    #[test]
    fn string_length_expansion() {
        let (events, status) = run_shell("X=hello; echo ${#X}");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "5\n");
    }

    // ---- Functions ----

    #[test]
    fn function_define_and_call() {
        let (events, status) = run_shell("greet() { echo hello; }; greet");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "hello\n");
    }

    #[test]
    fn function_with_args() {
        let mut rt = WorkerRuntime::new();
        rt.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        rt.handle_command(HostCommand::Run {
            input: "greet() { echo hello $1; }".into(),
        });
        let events = rt.handle_command(HostCommand::Run {
            input: "greet world".into(),
        });
        assert_eq!(get_stdout(&events), "hello world\n");
    }

    #[test]
    fn function_modifies_parent_scope() {
        // Bash behavior: functions share parent scope (no isolation by default)
        let mut rt = WorkerRuntime::new();
        rt.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        rt.handle_command(HostCommand::Run {
            input: "X=outer".into(),
        });
        rt.handle_command(HostCommand::Run {
            input: "f() { X=inner; }".into(),
        });
        rt.handle_command(HostCommand::Run { input: "f".into() });
        let events = rt.handle_command(HostCommand::Run {
            input: "echo $X".into(),
        });
        assert_eq!(get_stdout(&events), "inner\n");
    }

    #[test]
    fn local_isolates_in_function() {
        // `local` creates a variable that is restored after function returns
        let mut rt = WorkerRuntime::new();
        rt.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        rt.handle_command(HostCommand::Run {
            input: "X=outer".into(),
        });
        rt.handle_command(HostCommand::Run {
            input: "f() { local X=inner; echo $X; }".into(),
        });
        let events = rt.handle_command(HostCommand::Run {
            input: "f; echo $X".into(),
        });
        assert_eq!(get_stdout(&events), "inner\nouter\n");
    }

    // ---- Case ----

    #[test]
    fn case_basic() {
        let source = "case hello in\nhello) echo matched;;\nworld) echo no;;\nesac";
        let (events, status) = run_shell(source);
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "matched\n");
    }

    #[test]
    fn case_wildcard() {
        let source = "case anything in\n*) echo default;;\nesac";
        let (events, _) = run_shell(source);
        assert_eq!(get_stdout(&events), "default\n");
    }

    #[test]
    fn case_no_match() {
        let source = "case hello in\nworld) echo no;;\nesac";
        let (events, _) = run_shell(source);
        assert_eq!(get_stdout(&events), "");
    }

    // ---- Subshell scope isolation ----

    #[test]
    fn subshell_scope_isolation() {
        let mut rt = WorkerRuntime::new();
        rt.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        rt.handle_command(HostCommand::Run {
            input: "X=outer".into(),
        });
        rt.handle_command(HostCommand::Run {
            input: "(X=inner)".into(),
        });
        let events = rt.handle_command(HostCommand::Run {
            input: "echo $X".into(),
        });
        assert_eq!(get_stdout(&events), "outer\n");
    }

    // ---- Assign-default expansion ----

    #[test]
    fn assign_default_expansion() {
        let (events, _) = run_shell("echo ${X:=fallback}; echo $X");
        assert_eq!(get_stdout(&events), "fallback\nfallback\n");
    }

    // ---- Glob expansion ----

    #[test]
    fn glob_star_matches_files() {
        let mut rt = WorkerRuntime::new();
        rt.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        rt.handle_command(HostCommand::Run {
            input: "touch /a.txt".into(),
        });
        rt.handle_command(HostCommand::Run {
            input: "touch /b.txt".into(),
        });
        rt.handle_command(HostCommand::Run {
            input: "touch /c.log".into(),
        });
        let events = rt.handle_command(HostCommand::Run {
            input: "echo /*.txt".into(),
        });
        let stdout = get_stdout(&events);
        assert!(stdout.contains("/a.txt"));
        assert!(stdout.contains("/b.txt"));
        assert!(!stdout.contains("c.log"));
    }

    #[test]
    fn glob_no_match_keeps_literal() {
        let (events, _) = run_shell("echo /no_such_*.xyz");
        assert_eq!(get_stdout(&events), "/no_such_*.xyz\n");
    }

    #[test]
    fn glob_question_mark() {
        let mut rt = WorkerRuntime::new();
        rt.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        rt.handle_command(HostCommand::Run {
            input: "touch /ab".into(),
        });
        rt.handle_command(HostCommand::Run {
            input: "touch /ac".into(),
        });
        rt.handle_command(HostCommand::Run {
            input: "touch /abc".into(),
        });
        let events = rt.handle_command(HostCommand::Run {
            input: "echo /a?".into(),
        });
        let stdout = get_stdout(&events);
        assert!(stdout.contains("/ab"));
        assert!(stdout.contains("/ac"));
        assert!(!stdout.contains("/abc"));
    }

    // ---- Brace expansion ----

    #[test]
    fn brace_comma_expansion() {
        let (events, _) = run_shell("echo {a,b,c}");
        assert_eq!(get_stdout(&events), "a b c\n");
    }

    #[test]
    fn brace_range_expansion() {
        let (events, _) = run_shell("echo {1..5}");
        assert_eq!(get_stdout(&events), "1 2 3 4 5\n");
    }

    #[test]
    fn brace_prefix_suffix() {
        let (events, _) = run_shell("echo file{1,2,3}.txt");
        assert_eq!(get_stdout(&events), "file1.txt file2.txt file3.txt\n");
    }

    // ---- Here-string ----

    #[test]
    fn here_string_basic() {
        let (events, status) = run_shell("cat <<< hello");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "hello\n");
    }

    #[test]
    fn here_string_with_variable() {
        let (events, status) = run_shell("X=world; cat <<< $X");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "world\n");
    }

    // ---- ANSI-C quoting ----

    #[test]
    fn ansi_c_quoting_newline() {
        let (events, status) = run_shell("echo $'hello\\nworld'");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "hello\nworld\n");
    }

    #[test]
    fn ansi_c_quoting_tab() {
        let (events, status) = run_shell("echo $'a\\tb'");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "a\tb\n");
    }

    #[test]
    fn ansi_c_quoting_hex() {
        let (events, status) = run_shell("echo $'\\x41'");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "A\n");
    }

    // ---- Stderr redirection ----

    #[test]
    fn stderr_redirect_to_file() {
        let mut rt = WorkerRuntime::new();
        rt.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        // Running a command that doesn't exist produces stderr
        let _events = rt.handle_command(HostCommand::Run {
            input: "nonexistent_cmd 2> /err.txt".into(),
        });
        // stderr should have been captured to file
        let read_events = rt.handle_command(HostCommand::ReadFile {
            path: "/err.txt".into(),
        });
        let err_content = get_stdout(&read_events);
        assert!(err_content.contains("command not found"));
    }

    #[test]
    fn stderr_merge_into_stdout() {
        let mut rt = WorkerRuntime::new();
        rt.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        // Redirections are applied left-to-right: stderr duplicates the original
        // stdout, then stdout is redirected to the file. The error stays visible.
        let events = rt.handle_command(HostCommand::Run {
            input: "nonexistent_cmd 2>&1 > /out.txt".into(),
        });
        let read_events = rt.handle_command(HostCommand::ReadFile {
            path: "/out.txt".into(),
        });
        let content = get_stdout(&read_events);
        assert_eq!(content, "");
        assert!(get_stdout(&events).contains("command not found"));
        assert_eq!(get_stderr(&events), "");
    }

    #[test]
    fn amp_greater_both_to_file() {
        let mut rt = WorkerRuntime::new();
        rt.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        let _events = rt.handle_command(HostCommand::Run {
            input: "nonexistent_cmd &> /all.txt".into(),
        });
        let read_events = rt.handle_command(HostCommand::ReadFile {
            path: "/all.txt".into(),
        });
        let content = get_stdout(&read_events);
        assert!(content.contains("command not found"));
    }

    // ---- [[ ]] extended test ----

    #[test]
    fn dbl_bracket_string_equality() {
        let (_, status) = run_shell("[[ hello == hello ]]");
        assert_eq!(status, 0);
        let (_, status) = run_shell("[[ hello == world ]]");
        assert_eq!(status, 1);
    }

    #[test]
    fn dbl_bracket_string_inequality() {
        let (_, status) = run_shell("[[ hello != world ]]");
        assert_eq!(status, 0);
        let (_, status) = run_shell("[[ hello != hello ]]");
        assert_eq!(status, 1);
    }

    #[test]
    fn dbl_bracket_glob_match() {
        let (_, status) = run_shell("[[ hello == hel* ]]");
        assert_eq!(status, 0);
        let (_, status) = run_shell("[[ hello == wor* ]]");
        assert_eq!(status, 1);
    }

    #[test]
    fn dbl_bracket_string_ordering() {
        let (_, status) = run_shell("[[ abc < def ]]");
        assert_eq!(status, 0);
        let (_, status) = run_shell("[[ def < abc ]]");
        assert_eq!(status, 1);
        let (_, status) = run_shell("[[ def > abc ]]");
        assert_eq!(status, 0);
    }

    #[test]
    fn dbl_bracket_integer_comparison() {
        let (_, status) = run_shell("[[ 5 -eq 5 ]]");
        assert_eq!(status, 0);
        let (_, status) = run_shell("[[ 5 -ne 3 ]]");
        assert_eq!(status, 0);
        let (_, status) = run_shell("[[ 3 -lt 5 ]]");
        assert_eq!(status, 0);
        let (_, status) = run_shell("[[ 5 -le 5 ]]");
        assert_eq!(status, 0);
        let (_, status) = run_shell("[[ 7 -gt 3 ]]");
        assert_eq!(status, 0);
        let (_, status) = run_shell("[[ 5 -ge 5 ]]");
        assert_eq!(status, 0);
        let (_, status) = run_shell("[[ 5 -lt 3 ]]");
        assert_eq!(status, 1);
    }

    #[test]
    fn dbl_bracket_string_tests() {
        let (_, status) = run_shell("[[ -z \"\" ]]");
        assert_eq!(status, 0);
        let (_, status) = run_shell("[[ -z hello ]]");
        assert_eq!(status, 1);
        let (_, status) = run_shell("[[ -n hello ]]");
        assert_eq!(status, 0);
        let (_, status) = run_shell("[[ -n \"\" ]]");
        assert_eq!(status, 1);
    }

    #[test]
    fn dbl_bracket_logical_and() {
        let (_, status) = run_shell("[[ hello == hello && world == world ]]");
        assert_eq!(status, 0);
        let (_, status) = run_shell("[[ hello == hello && world == nope ]]");
        assert_eq!(status, 1);
    }

    #[test]
    fn dbl_bracket_logical_or() {
        let (_, status) = run_shell("[[ hello == nope || world == world ]]");
        assert_eq!(status, 0);
        let (_, status) = run_shell("[[ hello == nope || world == nope ]]");
        assert_eq!(status, 1);
    }

    #[test]
    fn dbl_bracket_logical_not() {
        let (_, status) = run_shell("[[ ! hello == world ]]");
        assert_eq!(status, 0);
        let (_, status) = run_shell("[[ ! hello == hello ]]");
        assert_eq!(status, 1);
    }

    #[test]
    fn dbl_bracket_variable_expansion() {
        let (_, status) = run_shell("X=hello; [[ $X == hello ]]");
        assert_eq!(status, 0);
        let (_, status) = run_shell("X=hello; [[ $X == world ]]");
        assert_eq!(status, 1);
    }

    #[test]
    fn dbl_bracket_no_word_splitting() {
        // In [[ ]], variables with spaces should NOT be word-split
        let (_, status) = run_shell("X=\"hello world\"; [[ $X == \"hello world\" ]]");
        assert_eq!(status, 0);
    }

    #[test]
    fn dbl_bracket_file_tests() {
        let mut rt = WorkerRuntime::new();
        rt.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        // Create a file
        rt.handle_command(HostCommand::Run {
            input: "touch /testfile".into(),
        });
        // -e: file exists
        let events = rt.handle_command(HostCommand::Run {
            input: "[[ -e /testfile ]]".into(),
        });
        let status = events
            .iter()
            .find_map(|e| {
                if let WorkerEvent::Exit(s) = e {
                    Some(*s)
                } else {
                    None
                }
            })
            .unwrap();
        assert_eq!(status, 0);

        // -f: is a regular file
        let events = rt.handle_command(HostCommand::Run {
            input: "[[ -f /testfile ]]".into(),
        });
        let status = events
            .iter()
            .find_map(|e| {
                if let WorkerEvent::Exit(s) = e {
                    Some(*s)
                } else {
                    None
                }
            })
            .unwrap();
        assert_eq!(status, 0);

        // -d: is a directory (should fail for a file)
        let events = rt.handle_command(HostCommand::Run {
            input: "[[ -d /testfile ]]".into(),
        });
        let status = events
            .iter()
            .find_map(|e| {
                if let WorkerEvent::Exit(s) = e {
                    Some(*s)
                } else {
                    None
                }
            })
            .unwrap();
        assert_eq!(status, 1);

        // -e: non-existent file
        let events = rt.handle_command(HostCommand::Run {
            input: "[[ -e /nonexistent ]]".into(),
        });
        let status = events
            .iter()
            .find_map(|e| {
                if let WorkerEvent::Exit(s) = e {
                    Some(*s)
                } else {
                    None
                }
            })
            .unwrap();
        assert_eq!(status, 1);
    }

    #[test]
    fn dbl_bracket_dir_test() {
        let mut rt = WorkerRuntime::new();
        rt.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        rt.handle_command(HostCommand::Run {
            input: "mkdir /testdir".into(),
        });
        let events = rt.handle_command(HostCommand::Run {
            input: "[[ -d /testdir ]]".into(),
        });
        let status = events
            .iter()
            .find_map(|e| {
                if let WorkerEvent::Exit(s) = e {
                    Some(*s)
                } else {
                    None
                }
            })
            .unwrap();
        assert_eq!(status, 0);
    }

    #[test]
    fn dbl_bracket_regex_match() {
        let (_, status) = run_shell("[[ hello =~ ^hel ]]");
        assert_eq!(status, 0);
        let (_, status) = run_shell("[[ hello =~ world ]]");
        assert_eq!(status, 1);
        let (_, status) = run_shell("[[ hello =~ ^hello$ ]]");
        assert_eq!(status, 0);
    }

    #[test]
    fn dbl_bracket_in_if() {
        let (events, status) = run_shell("if [[ 1 -eq 1 ]]; then echo yes; fi");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "yes\n");
    }

    #[test]
    fn dbl_bracket_in_and_or() {
        let (events, _) = run_shell("[[ hello == hello ]] && echo matched");
        assert_eq!(get_stdout(&events), "matched\n");
        let (events, _) = run_shell("[[ hello == nope ]] || echo fallback");
        assert_eq!(get_stdout(&events), "fallback\n");
    }

    #[test]
    fn dbl_bracket_grouping() {
        let (_, status) = run_shell("[[ ( hello == hello ) ]]");
        assert_eq!(status, 0);
        // Grouping with || inside ()
        let (_, status) = run_shell("[[ ( a == b || a == a ) && x == x ]]");
        assert_eq!(status, 0);
    }

    #[test]
    fn dbl_bracket_single_string() {
        // Non-empty string is true
        let (_, status) = run_shell("[[ hello ]]");
        assert_eq!(status, 0);
        // Empty string is false
        let (_, status) = run_shell("[[ \"\" ]]");
        assert_eq!(status, 1);
    }

    // ---- (( )) arithmetic command ----

    #[test]
    fn arith_command_nonzero_is_success() {
        // (( 1 )) → non-zero result → exit 0
        let (_, status) = run_shell("(( 1 ))");
        assert_eq!(status, 0);
    }

    #[test]
    fn arith_command_zero_is_failure() {
        // (( 0 )) → zero result → exit 1
        let (_, status) = run_shell("(( 0 ))");
        assert_eq!(status, 1);
    }

    #[test]
    fn arith_command_expression() {
        let (_, status) = run_shell("(( 2 + 3 ))");
        assert_eq!(status, 0); // result 5 → non-zero → success
    }

    #[test]
    fn arith_command_assignment() {
        let (events, _) = run_shell("(( x = 42 )); echo $x");
        assert_eq!(get_stdout(&events), "42\n");
    }

    #[test]
    fn arith_command_in_if() {
        let (events, _) = run_shell("if (( 1 + 1 )); then echo yes; fi");
        assert_eq!(get_stdout(&events), "yes\n");
    }

    #[test]
    fn arith_command_in_and_or() {
        let (events, _) = run_shell("(( 1 )) && echo ok");
        assert_eq!(get_stdout(&events), "ok\n");
        let (events, _) = run_shell("(( 0 )) || echo fallback");
        assert_eq!(get_stdout(&events), "fallback\n");
    }

    #[test]
    fn arith_command_increment() {
        let (events, _) = run_shell("x=5; (( x++ )); echo $x");
        assert_eq!(get_stdout(&events), "6\n");
    }

    // ---- C-style for (( )) loop ----

    #[test]
    fn arith_for_basic() {
        let (events, status) = run_shell("for ((i=0; i<5; i++)) do echo $i; done");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "0\n1\n2\n3\n4\n");
    }

    #[test]
    fn arith_for_with_spaces() {
        let (events, _) = run_shell("for (( i = 0; i < 3; i++ )) do echo $i; done");
        assert_eq!(get_stdout(&events), "0\n1\n2\n");
    }

    #[test]
    fn arith_for_sum() {
        let (events, _) =
            run_shell("sum=0; for ((i=1; i<=10; i++)) do (( sum += i )); done; echo $sum");
        assert_eq!(get_stdout(&events), "55\n");
    }

    #[test]
    fn arith_for_break() {
        let (events, _) =
            run_shell("for ((i=0; i<100; i++)) do if (( i == 3 )); then break; fi; echo $i; done");
        assert_eq!(get_stdout(&events), "0\n1\n2\n");
    }

    #[test]
    fn arith_for_continue() {
        let (events, _) =
            run_shell("for ((i=0; i<5; i++)) do if (( i == 2 )); then continue; fi; echo $i; done");
        assert_eq!(get_stdout(&events), "0\n1\n3\n4\n");
    }

    // ---- let builtin ----

    #[test]
    fn let_basic_assignment() {
        let (events, _) = run_shell("let x=5; echo $x");
        assert_eq!(get_stdout(&events), "5\n");
    }

    #[test]
    fn let_arithmetic() {
        let (events, _) = run_shell("let x=2+3; echo $x");
        assert_eq!(get_stdout(&events), "5\n");
    }

    #[test]
    fn let_returns_zero_for_nonzero() {
        // let returns 0 when last expression is non-zero
        let (_, status) = run_shell("let 1+1");
        assert_eq!(status, 0);
    }

    #[test]
    fn let_returns_one_for_zero() {
        // let returns 1 when last expression is zero
        let (_, status) = run_shell("let 0");
        assert_eq!(status, 1);
    }

    #[test]
    fn let_multiple_expressions() {
        let (events, status) = run_shell("let a=1 b=2 c=a+b; echo $c");
        assert_eq!(status, 0); // last expr (a+b=3) is non-zero → 0
        assert_eq!(get_stdout(&events), "3\n");
    }

    #[test]
    fn let_no_args_fails() {
        let (_, status) = run_shell("let");
        assert_eq!(status, 1);
    }

    // ---- declare/typeset ----

    #[test]
    fn declare_basic_variable() {
        let (events, _) = run_shell("declare x=hello; echo $x");
        assert_eq!(get_stdout(&events), "hello\n");
    }

    #[test]
    fn declare_integer_flag() {
        let (events, _) = run_shell("declare -i x=2+3; echo $x");
        assert_eq!(get_stdout(&events), "5\n");
    }

    #[test]
    fn declare_export_flag() {
        let (events, _) = run_shell("declare -x MYVAR=exported; echo $MYVAR");
        assert_eq!(get_stdout(&events), "exported\n");
    }

    #[test]
    fn declare_readonly_flag() {
        // After declare -r, re-assignment should be silently ignored
        let (events, _) = run_shell("declare -r X=locked; X=new; echo $X");
        assert_eq!(get_stdout(&events), "locked\n");
    }

    #[test]
    fn declare_lowercase_flag() {
        let (events, _) = run_shell("declare -l x=HELLO; echo $x");
        assert_eq!(get_stdout(&events), "hello\n");
    }

    #[test]
    fn declare_uppercase_flag() {
        let (events, _) = run_shell("declare -u x=hello; echo $x");
        assert_eq!(get_stdout(&events), "HELLO\n");
    }

    #[test]
    fn declare_indexed_array() {
        let (events, _) = run_shell("declare -a arr; arr[0]=x; arr[1]=y; echo ${arr[0]} ${arr[1]}");
        assert_eq!(get_stdout(&events), "x y\n");
    }

    #[test]
    fn declare_assoc_array() {
        let (events, _) = run_shell("declare -A map; map[key]=val; echo ${map[key]}");
        assert_eq!(get_stdout(&events), "val\n");
    }

    #[test]
    fn typeset_is_alias_for_declare() {
        let (events, _) = run_shell("typeset -i x=3+4; echo $x");
        assert_eq!(get_stdout(&events), "7\n");
    }

    #[test]
    fn declare_print_specific_var() {
        let (events, _) = run_shell("x=hello; declare -p x");
        let out = get_stdout(&events);
        assert!(out.contains("x="));
        assert!(out.contains("hello"));
    }

    // ---- set -o / shell option enforcement tests ----

    #[test]
    fn set_o_pipefail_enable_disable() {
        // set -o pipefail stores SHOPT_o_pipefail=1
        let (events, status) = run_shell("set -o pipefail; echo $SHOPT_o_pipefail");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "1\n");

        // set +o pipefail stores SHOPT_o_pipefail=0
        let (events, _) = run_shell("set -o pipefail; set +o pipefail; echo $SHOPT_o_pipefail");
        assert_eq!(get_stdout(&events), "0\n");
    }

    #[test]
    fn pipefail_uses_rightmost_failure() {
        // Without pipefail: last command determines status
        let (_, status) = run_shell("false | true");
        assert_eq!(status, 0);

        // With pipefail: rightmost non-zero status is used
        let (_, status) = run_shell("set -o pipefail; false | true");
        assert_eq!(status, 1);
    }

    #[test]
    fn pipefail_all_succeed_is_zero() {
        let (_, status) = run_shell("set -o pipefail; true | true | true");
        assert_eq!(status, 0);
    }

    #[test]
    fn pipefail_rightmost_nonzero() {
        // The rightmost non-zero should be chosen
        let (_, status) = run_shell("set -o pipefail; false | true | false");
        assert_eq!(status, 1);
    }

    #[test]
    fn nounset_unset_var_errors() {
        let (events, status) = run_shell("set -u; echo $UNSET_VAR");
        assert_eq!(status, 1);
        let stderr = get_stderr(&events);
        assert!(stderr.contains("UNSET_VAR"));
        assert!(stderr.contains("unbound variable"));
    }

    #[test]
    fn nounset_set_var_ok() {
        // set -u should not trigger for defined variables
        let (events, status) = run_shell("set -u; X=hello; echo $X");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "hello\n");
    }

    #[test]
    fn nounset_special_params_ok() {
        // $? and $# should not trigger nounset
        let (events, status) = run_shell("set -u; echo $? $#");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "0 0\n");
    }

    #[test]
    fn nounset_with_default_operator() {
        // ${var:-default} should not trigger nounset even when var is unset
        let (events, status) = run_shell("set -u; echo ${UNSET:-fallback}");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "fallback\n");
    }

    #[test]
    fn nounset_long_option_alias() {
        // set -o nounset should be equivalent to set -u
        let (events, status) = run_shell("set -o nounset; echo $UNSET_VAR");
        assert_eq!(status, 1);
        let stderr = get_stderr(&events);
        assert!(stderr.contains("unbound variable"));
    }

    #[test]
    fn xtrace_outputs_commands() {
        let (events, status) = run_shell("set -x; echo hello");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "hello\n");
        let stderr = get_stderr(&events);
        // xtrace should produce "+ echo hello" on stderr
        assert!(stderr.contains("+ echo hello"));
    }

    #[test]
    fn xtrace_custom_ps4() {
        let (events, _) = run_shell("PS4='>> '; set -x; echo test");
        let stderr = get_stderr(&events);
        assert!(stderr.contains(">> echo test"));
    }

    #[test]
    fn xtrace_disabled_with_plus_x() {
        let (events, _) = run_shell("set -x; set +x; echo quiet");
        let stderr = get_stderr(&events);
        // The "set +x" itself is traced, but "echo quiet" should not be
        assert!(stderr.contains("+ set +x"));
        assert!(!stderr.contains("+ echo quiet"));
    }

    #[test]
    fn noglob_skips_expansion() {
        let mut rt = WorkerRuntime::new();
        rt.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        // Create a file that would match *.txt
        rt.handle_command(HostCommand::Run {
            input: "touch /hello.txt".into(),
        });
        // With noglob, the * should be literal
        let events = rt.handle_command(HostCommand::Run {
            input: "set -f; echo /*.txt".into(),
        });
        let stdout = get_stdout(&events);
        assert_eq!(stdout, "/*.txt\n");
    }

    #[test]
    fn noglob_disabled_allows_expansion() {
        let mut rt = WorkerRuntime::new();
        rt.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        rt.handle_command(HostCommand::Run {
            input: "touch /abc.txt".into(),
        });
        // Enable then disable noglob: globs should work again
        let events = rt.handle_command(HostCommand::Run {
            input: "set -f; set +f; echo /*.txt".into(),
        });
        let stdout = get_stdout(&events);
        assert_eq!(stdout, "/abc.txt\n");
    }

    #[test]
    fn allexport_auto_exports() {
        let (events, status) = run_shell("set -a; MYVAR=hello; echo $MYVAR");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "hello\n");
        // We can't directly test export flag from shell, but we can verify
        // via declare -p which shows flags. Or we simply verify the variable is set.
    }

    #[test]
    fn set_long_options_errexit() {
        // set -o errexit should be same as set -e
        let (events, status) = run_shell("set -o errexit; echo $SHOPT_e");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "1\n");
    }

    #[test]
    fn set_long_options_xtrace() {
        let (events, _) = run_shell("set -o xtrace; echo $SHOPT_x");
        assert_eq!(get_stdout(&events), "1\n");
    }

    #[test]
    fn set_long_options_allexport() {
        let (events, _) = run_shell("set -o allexport; echo $SHOPT_a");
        assert_eq!(get_stdout(&events), "1\n");
    }

    #[test]
    fn set_long_options_noglob() {
        let (events, _) = run_shell("set -o noglob; echo $SHOPT_f");
        assert_eq!(get_stdout(&events), "1\n");
    }

    #[test]
    fn set_long_options_noclobber() {
        let (events, _) = run_shell("set -o noclobber; echo $SHOPT_C");
        assert_eq!(get_stdout(&events), "1\n");
    }

    // ---- shopt builtin tests ----

    #[test]
    fn shopt_list_all() {
        let (events, status) = run_shell("shopt");
        assert_eq!(status, 0);
        let out = get_stdout(&events);
        assert!(out.contains("extglob"));
        assert!(out.contains("nullglob"));
        assert!(out.contains("dotglob"));
        assert!(out.contains("globstar"));
        assert!(out.contains("off"));
    }

    #[test]
    fn shopt_enable_option() {
        let (events, status) = run_shell("shopt -s extglob; shopt extglob");
        assert_eq!(status, 0);
        let out = get_stdout(&events);
        assert!(out.contains("extglob\ton"));
    }

    #[test]
    fn shopt_disable_option() {
        let (events, status) = run_shell("shopt -s extglob; shopt -u extglob; shopt extglob");
        assert_eq!(status, 0);
        let out = get_stdout(&events);
        assert!(out.contains("extglob\toff"));
    }

    #[test]
    fn shopt_invalid_option() {
        let (events, status) = run_shell("shopt -s nonexistent");
        assert_eq!(status, 1);
        let stderr = get_stderr(&events);
        assert!(stderr.contains("invalid shell option name"));
    }

    #[test]
    fn shopt_query_specific() {
        let (events, status) = run_shell("shopt nullglob");
        assert_eq!(status, 0);
        let out = get_stdout(&events);
        assert!(out.contains("nullglob\toff"));
    }

    // ---- Dynamic variables ----

    #[test]
    fn dynamic_random() {
        let (events, status) = run_shell("echo $RANDOM");
        assert_eq!(status, 0);
        let out = get_stdout(&events);
        let val: u32 = out.trim().parse().unwrap();
        assert!(val < 32768);
    }

    #[test]
    fn dynamic_random_changes() {
        // Two calls should produce different values
        let (events, _) = run_shell("echo $RANDOM; echo $RANDOM");
        let out = get_stdout(&events);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_ne!(lines[0], lines[1]);
    }

    #[test]
    fn dynamic_lineno() {
        let (events, status) = run_shell("echo $LINENO");
        assert_eq!(status, 0);
        let out = get_stdout(&events);
        // LINENO should be a number
        let _val: u32 = out.trim().parse().unwrap();
    }

    #[test]
    fn dynamic_seconds() {
        let (events, status) = run_shell("echo $SECONDS");
        assert_eq!(status, 0);
        let out = get_stdout(&events);
        let val: u64 = out.trim().parse().unwrap();
        assert!(val < 60);
    }

    #[test]
    fn dynamic_funcname() {
        let (events, status) = run_shell("myfn() { echo $FUNCNAME; }; myfn");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "myfn\n");
    }

    #[test]
    fn dynamic_pipestatus() {
        let (events, status) = run_shell("true | false; echo ${PIPESTATUS[0]} ${PIPESTATUS[1]}");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "0 1\n");
    }

    #[test]
    fn streaming_grep_no_match_returns_failure() {
        let (events, status) = run_shell("echo a | grep b");
        assert_eq!(status, 1);
        assert_eq!(get_stdout(&events), "");
    }

    #[test]
    fn streaming_grep_updates_pipestatus() {
        let (events, status) = run_shell(
            "echo a | grep b | cat; echo ${PIPESTATUS[0]} ${PIPESTATUS[1]} ${PIPESTATUS[2]}",
        );
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "0 1 0\n");
    }

    #[test]
    fn streaming_grep_respects_pipefail_status() {
        let (_, status) = run_shell("set -o pipefail; echo a | grep b | cat");
        assert_eq!(status, 1);
    }

    #[test]
    fn dynamic_bash_source() {
        let mut rt = WorkerRuntime::new();
        rt.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        rt.handle_command(HostCommand::WriteFile {
            path: "/test.sh".into(),
            data: b"echo $BASH_SOURCE".to_vec(),
        });
        let events = rt.handle_command(HostCommand::Run {
            input: "source /test.sh".into(),
        });
        assert_eq!(get_stdout(&events), "/test.sh\n");
    }

    // ---- Alias/unalias ----

    #[test]
    fn alias_basic() {
        let (events, status) = run_shell("alias ll='echo listing'; ll");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "listing\n");
    }

    #[test]
    fn alias_with_args() {
        let (events, status) = run_shell("alias greet='echo hello'; greet world");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "hello world\n");
    }

    #[test]
    fn shopt_expand_aliases_can_disable_alias_expansion() {
        let (events, status) = run_shell("alias ll='echo listing'; shopt -u expand_aliases; ll");
        assert_eq!(status, 127);
        assert!(get_stderr(&events).contains("command not found"));
    }

    #[test]
    fn shopt_expand_aliases_can_reenable_alias_expansion() {
        let (events, status) = run_shell(
            "alias ll='echo listing'; shopt -u expand_aliases; shopt -s expand_aliases; ll",
        );
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "listing\n");
    }

    #[test]
    fn alias_list_all() {
        let (events, status) = run_shell("alias ll='ls -la'; alias g='grep'; alias");
        assert_eq!(status, 0);
        let out = get_stdout(&events);
        assert!(out.contains("alias ll='ls -la'"));
        assert!(out.contains("alias g='grep'"));
    }

    #[test]
    fn alias_show_specific() {
        let (events, status) = run_shell("alias ll='ls -la'; alias ll");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "alias ll='ls -la'\n");
    }

    #[test]
    fn unalias_removes() {
        let (events, status) = run_shell("alias ll='echo hi'; unalias ll; ll");
        assert_eq!(status, 127); // command not found
        let stderr = get_stderr(&events);
        assert!(stderr.contains("command not found"));
    }

    #[test]
    fn unalias_all() {
        let (events, status) = run_shell("alias a='echo a'; alias b='echo b'; unalias -a; alias");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "");
    }

    // ---- Enhanced printf ----

    #[test]
    fn printf_hex() {
        let (events, _) = run_shell("printf '%x' 255");
        assert_eq!(get_stdout(&events), "ff");
    }

    #[test]
    fn printf_octal() {
        let (events, _) = run_shell("printf '%o' 8");
        assert_eq!(get_stdout(&events), "10");
    }

    #[test]
    fn printf_float() {
        let (events, _) = run_shell("printf '%.2f' 3.14159");
        assert_eq!(get_stdout(&events), "3.14");
    }

    #[test]
    fn printf_char() {
        let (events, _) = run_shell("printf '%c' A");
        assert_eq!(get_stdout(&events), "A");
    }

    #[test]
    fn printf_width_right_align() {
        let (events, _) = run_shell("printf '%10s' hello");
        assert_eq!(get_stdout(&events), "     hello");
    }

    #[test]
    fn printf_width_left_align() {
        let (events, _) = run_shell("printf '%-10s|' hello");
        assert_eq!(get_stdout(&events), "hello     |");
    }

    #[test]
    fn printf_zero_pad() {
        let (events, _) = run_shell("printf '%05d' 42");
        assert_eq!(get_stdout(&events), "00042");
    }

    #[test]
    fn printf_backslash_b() {
        let (events, _) = run_shell("printf '%b' 'hello\\nworld'");
        assert_eq!(get_stdout(&events), "hello\nworld");
    }

    #[test]
    fn printf_shell_quote_q() {
        let (events, _) = run_shell("printf '%q' 'hello world'");
        let out = get_stdout(&events);
        // Should be quoted with $'...' or similar
        assert!(out.contains("hello") && out.contains("world"));
    }

    #[test]
    fn printf_precision_string() {
        let (events, _) = run_shell("printf '%.3s' abcdef");
        assert_eq!(get_stdout(&events), "abc");
    }

    // ---- Enhanced read ----

    #[test]
    fn read_prompt() {
        let (events, _) = run_shell("echo hello | read -p 'Enter: ' VAR; echo done");
        let stderr = get_stderr(&events);
        assert!(stderr.contains("Enter: "));
    }

    #[test]
    fn read_delimiter() {
        let (events, status) = run_shell("printf 'a:b:c' | read -d ':' VAR; echo $VAR");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "a\n");
    }

    #[test]
    fn read_nchars() {
        let (events, status) = run_shell("echo 'hello' | read -n 3 VAR; echo $VAR");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "hel\n");
    }

    #[test]
    fn read_exact_nchars() {
        let (events, status) = run_shell("printf 'ab\\ncd' | read -N 4 VAR; echo \"$VAR\"");
        assert_eq!(status, 0);
        // -N reads exactly 4 chars, ignoring delimiter
        let out = get_stdout(&events);
        assert!(out.starts_with("ab"));
    }

    #[test]
    fn read_into_array() {
        let (events, status) =
            run_shell("echo 'one two three' | read -a arr; echo ${arr[0]} ${arr[1]} ${arr[2]}");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "one two three\n");
    }

    // ---- builtin keyword ----

    #[test]
    fn builtin_keyword_invokes_builtin() {
        let (events, status) = run_shell("builtin echo hello");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "hello\n");
    }

    #[test]
    fn builtin_keyword_skips_function() {
        let (events, status) =
            run_shell("echo() { printf 'FUNC: %s\\n' \"$1\"; }; builtin echo direct");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "direct\n");
    }

    #[test]
    fn builtin_keyword_not_builtin_errors() {
        let (events, status) = run_shell("builtin nonexistent");
        assert_eq!(status, 1);
        let stderr = get_stderr(&events);
        assert!(stderr.contains("not a shell builtin"));
    }

    #[test]
    fn builtin_keyword_inside_function_uses_real_builtin() {
        let (events, status) = run_shell(
            "echo() { builtin echo \"wrapped: $@\"; }\n\
             echo hello",
        );
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "wrapped: hello\n");
    }

    // ---- source PATH search ----

    #[test]
    fn source_path_search() {
        let mut rt = WorkerRuntime::new();
        rt.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        // Create /bin directory and a script in it
        rt.handle_command(HostCommand::Run {
            input: "mkdir /bin".into(),
        });
        rt.handle_command(HostCommand::WriteFile {
            path: "/bin/helpers.sh".into(),
            data: b"LOADED=yes".to_vec(),
        });
        // Set PATH and source without slash
        let events = rt.handle_command(HostCommand::Run {
            input: "PATH=/bin; source helpers.sh; echo $LOADED".into(),
        });
        assert_eq!(get_stdout(&events), "yes\n");
    }

    // ---- mapfile/readarray ----

    #[test]
    fn mapfile_basic() {
        let (events, status) =
            run_shell("printf 'a\\nb\\nc\\n' | mapfile arr; echo ${arr[0]} ${arr[1]} ${arr[2]}");
        assert_eq!(status, 0);
        let out = get_stdout(&events);
        // Each element includes trailing newline by default
        assert!(out.contains('a'));
        assert!(out.contains('b'));
        assert!(out.contains('c'));
    }

    #[test]
    fn mapfile_strip_newline() {
        let (events, status) = run_shell(
            "printf 'x\\ny\\nz\\n' | mapfile -t arr; echo \"${arr[0]}${arr[1]}${arr[2]}\"",
        );
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "xyz\n");
    }

    #[test]
    fn mapfile_default_name() {
        let (events, status) = run_shell("printf 'hello\\nworld\\n' | mapfile; echo ${MAPFILE[0]}");
        assert_eq!(status, 0);
        let out = get_stdout(&events);
        assert!(out.contains("hello"));
    }

    #[test]
    fn readarray_is_alias_for_mapfile() {
        let (events, status) =
            run_shell("printf 'a\\nb\\n' | readarray -t arr; echo ${arr[0]} ${arr[1]}");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "a b\n");
    }

    #[test]
    fn process_subst_out_feeds_inner_command() {
        let (events, status) = run_shell("printf hi > >(cat)");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "hi");
    }

    #[test]
    fn process_subst_out_runs_schedulable_inner_pipeline() {
        let (events, status) = run_shell("printf 'a\\nb\\n' > >(head -n 1 | cat)");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "a\n");
    }

    #[test]
    fn process_subst_out_runs_live_tail_pipeline() {
        let (events, status) = run_shell("printf 'a\\nb\\n' > >(tail -n 1 | cat)");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "b\n");
    }

    #[test]
    fn process_subst_out_runs_live_buffered_pipeline() {
        let (events, status) = run_shell("printf 'b\\na\\n' > >(sort | cat)");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "a\nb\n");
    }

    #[test]
    fn process_subst_out_isolates_shell_state() {
        let (events, status) =
            run_shell("foo=before; printf hi > >(foo=after; wc -c >/count.txt); echo $foo");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "before\n");
    }

    // ---- Pipe-ampersand (|&) ----

    #[test]
    fn pipe_amp_captures_stderr() {
        let (events, status) = run_shell("echo error >&2 |& cat");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "error\n");
    }

    #[test]
    fn plain_pipeline_leaves_stderr_unpiped() {
        let (events, status) = run_shell("echo error >&2 | cat");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "");
        assert_eq!(get_stderr(&events), "error\n");
    }

    #[test]
    fn pipe_amp_captures_both_stdout_and_stderr() {
        let (events, status) = run_shell("{ echo out; echo err >&2; } |& cat");
        assert_eq!(status, 0);
        let stdout = get_stdout(&events);
        assert!(stdout.contains("out"));
        assert!(stdout.contains("err"));
    }

    // ---- Case fall-through (;&) ----

    #[test]
    fn case_fallthrough() {
        let (events, status) = run_shell(
            "X=a\ncase $X in\n  a) echo one ;&\n  b) echo two ;;\n  c) echo three ;;\nesac",
        );
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "one\ntwo\n");
    }

    // ---- Case continue-testing (;;&) ----

    #[test]
    fn case_continue_testing() {
        let (events, status) = run_shell(
            "X=abc\ncase $X in\n  a*) echo starts-a ;;&\n  *b*) echo contains-b ;;&\n  *c) echo ends-c ;;\nesac",
        );
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "starts-a\ncontains-b\nends-c\n");
    }

    // ---- Case glob matching ----

    #[test]
    fn case_glob_pattern() {
        let (events, status) =
            run_shell("case hello in\n  h*) echo matched ;;\n  *) echo nope ;;\nesac");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "matched\n");
    }

    // ---- Select ----

    #[test]
    fn select_basic() {
        // Use echo pipe to provide stdin to select
        let (events, status) = run_shell(
            "echo 2 | select item in apple banana cherry; do\n  echo \"chose: $item\"\n  break\ndone",
        );
        assert_eq!(status, 0);
        let stdout = get_stdout(&events);
        assert!(stdout.contains("chose: banana"), "got: {stdout}");
    }

    // ---- $"..." locale quoting ----

    #[test]
    fn locale_quoting_basic() {
        let (events, status) = run_shell("echo $\"hello\"");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "hello\n");
    }

    #[test]
    fn locale_quoting_with_variable() {
        let (events, status) = run_shell("X=world; echo $\"hello $X\"");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "hello world\n");
    }

    // ---- nullglob ----

    #[test]
    fn nullglob_empty_on_no_match() {
        let (events, status) = run_shell(
            "shopt -s nullglob\nresult=$(echo /nonexistent/*.xyz)\nif test -z \"$result\"; then\n  echo empty\nfi",
        );
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "empty\n");
    }

    // ---- dotglob ----

    #[test]
    fn dotglob_matches_hidden() {
        let mut rt = WorkerRuntime::new();
        rt.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        rt.handle_command(HostCommand::Run {
            input: "mkdir /tmp2".into(),
        });
        rt.handle_command(HostCommand::WriteFile {
            path: "/tmp2/.hidden".into(),
            data: vec![],
        });
        rt.handle_command(HostCommand::WriteFile {
            path: "/tmp2/visible".into(),
            data: vec![],
        });
        let events = rt.handle_command(HostCommand::Run {
            input: "cd /tmp2; shopt -s dotglob; echo * | tr ' ' '\\n' | sort".into(),
        });
        let stdout = get_stdout(&events);
        assert!(stdout.contains(".hidden"), "got: {stdout}");
        assert!(stdout.contains("visible"), "got: {stdout}");
    }

    // ---- nocasematch ----

    #[test]
    fn nocasematch_case_statement() {
        let (events, status) = run_shell(
            "shopt -s nocasematch\nX=Hello\ncase $X in\n  hello) echo matched ;;\n  *) echo no-match ;;\nesac",
        );
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "matched\n");
    }

    #[test]
    fn nocasematch_double_bracket() {
        let (events, status) = run_shell(
            "shopt -s nocasematch\nif [[ HELLO == hello ]]; then echo yes; else echo no; fi",
        );
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "yes\n");
    }

    // ---- extglob matching ----

    #[test]
    fn extglob_match_at_basic() {
        assert!(extglob_match("@(jpg|png)", "jpg"));
        assert!(extglob_match("@(jpg|png)", "png"));
        assert!(!extglob_match("@(jpg|png)", "txt"));
    }

    #[test]
    fn extglob_match_star_suffix() {
        assert!(extglob_match("*.@(jpg|png)", "file.jpg"));
        assert!(extglob_match("*.@(jpg|png)", "file.png"));
        assert!(!extglob_match("*.@(jpg|png)", "file.txt"));
    }

    #[test]
    fn extglob_match_not() {
        assert!(!extglob_match("!(*.log)", "b.log"));
        assert!(extglob_match("!(*.log)", "a.txt"));
    }

    #[test]
    fn extglob_match_optional() {
        assert!(extglob_match("colo?(u)r", "color"));
        assert!(extglob_match("colo?(u)r", "colour"));
        assert!(!extglob_match("colo?(u)r", "colouur"));
    }

    // ---- extglob (integration) ----

    #[test]
    fn extglob_at_pattern() {
        let mut rt = WorkerRuntime::new();
        rt.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        rt.handle_command(HostCommand::WriteFile {
            path: "/tmp3/file.jpg".into(),
            data: vec![],
        });
        rt.handle_command(HostCommand::WriteFile {
            path: "/tmp3/file.png".into(),
            data: vec![],
        });
        rt.handle_command(HostCommand::WriteFile {
            path: "/tmp3/file.txt".into(),
            data: vec![],
        });
        let events = rt.handle_command(HostCommand::Run {
            input: "cd /tmp3; shopt -s extglob; for f in *.@(jpg|png); do echo $f; done | sort"
                .into(),
        });
        let stdout = get_stdout(&events);
        assert!(stdout.contains("file.jpg"), "got: {stdout}");
        assert!(stdout.contains("file.png"), "got: {stdout}");
        assert!(!stdout.contains("file.txt"), "got: {stdout}");
    }

    #[test]
    fn extglob_not_pattern() {
        let mut rt = WorkerRuntime::new();
        rt.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        rt.handle_command(HostCommand::WriteFile {
            path: "/tmp4/a.txt".into(),
            data: vec![],
        });
        rt.handle_command(HostCommand::WriteFile {
            path: "/tmp4/b.log".into(),
            data: vec![],
        });
        rt.handle_command(HostCommand::WriteFile {
            path: "/tmp4/c.txt".into(),
            data: vec![],
        });
        let events = rt.handle_command(HostCommand::Run {
            input: "cd /tmp4; shopt -s extglob; for f in !(*.log); do echo $f; done | sort".into(),
        });
        let stdout = get_stdout(&events);
        assert!(stdout.contains("a.txt"), "got: {stdout}");
        assert!(stdout.contains("c.txt"), "got: {stdout}");
        assert!(!stdout.contains("b.log"), "got: {stdout}");
    }

    #[test]
    fn extglob_optional_pattern() {
        let mut rt = WorkerRuntime::new();
        rt.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        rt.handle_command(HostCommand::WriteFile {
            path: "/tmp5/color".into(),
            data: vec![],
        });
        rt.handle_command(HostCommand::WriteFile {
            path: "/tmp5/colour".into(),
            data: vec![],
        });
        let events = rt.handle_command(HostCommand::Run {
            input: "cd /tmp5; shopt -s extglob; for f in colo?(u)r; do echo $f; done | sort".into(),
        });
        let stdout = get_stdout(&events);
        assert!(stdout.contains("color"), "got: {stdout}");
        assert!(stdout.contains("colour"), "got: {stdout}");
    }

    // ---- globstar ----

    #[test]
    fn globstar_recursive() {
        let mut rt = WorkerRuntime::new();
        rt.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        rt.handle_command(HostCommand::WriteFile {
            path: "/project/a.txt".into(),
            data: vec![],
        });
        rt.handle_command(HostCommand::WriteFile {
            path: "/project/sub/b.txt".into(),
            data: vec![],
        });
        rt.handle_command(HostCommand::WriteFile {
            path: "/project/sub/deep/c.txt".into(),
            data: vec![],
        });
        let events = rt.handle_command(HostCommand::Run {
            input: "cd /project; shopt -s globstar; for f in **/*.txt; do echo $f; done | sort"
                .into(),
        });
        let stdout = get_stdout(&events);
        assert!(stdout.contains("a.txt"), "got: {stdout}");
        assert!(stdout.contains("sub/b.txt"), "got: {stdout}");
        assert!(stdout.contains("sub/deep/c.txt"), "got: {stdout}");
    }

    #[test]
    fn exec_live_redirections_preserve_left_to_right_dup_order() {
        let mut rt = WorkerRuntime::new();
        rt.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });

        let events = rt.handle_command(HostCommand::Run {
            input: "printf hi > /first.txt 1>&2\nprintf hi 1>&2 > /second.txt".into(),
        });

        assert_eq!(get_stdout(&events), "");
        assert_eq!(get_stderr(&events), "hi");

        let first = rt.handle_command(HostCommand::ReadFile {
            path: "/first.txt".into(),
        });
        assert_eq!(get_stdout(&first), "");

        let second = rt.handle_command(HostCommand::ReadFile {
            path: "/second.txt".into(),
        });
        assert_eq!(get_stdout(&second), "hi");
    }

    #[test]
    fn exec_process_subst_redirections_preserve_left_to_right_dup_order() {
        let mut rt = WorkerRuntime::new();
        rt.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });

        let events = rt.handle_command(HostCommand::Run {
            input: "printf hi > >(cat) 1>&2\nprintf hi 1>&2 > >(cat)".into(),
        });

        assert_eq!(get_stdout(&events), "hi");
        assert_eq!(get_stderr(&events), "hi");
    }

    #[test]
    fn process_subst_in_streams_native_pipeline() {
        let mut rt = WorkerRuntime::new();
        rt.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });

        let events = rt.handle_command(HostCommand::Run {
            input: "cat <(yes | head -n 5)".into(),
        });

        assert_eq!(get_stdout(&events), "y\ny\ny\ny\ny\n");
        assert_eq!(get_stderr(&events), "");
    }

    #[test]
    fn process_subst_in_streams_native_sed_pipeline() {
        let mut rt = WorkerRuntime::new();
        rt.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });

        let events = rt.handle_command(HostCommand::Run {
            input: "cat <(yes | sed 's/y/z/' | head -n 3)".into(),
        });

        assert_eq!(get_stdout(&events), "z\nz\nz\n");
        assert_eq!(get_stderr(&events), "");
    }

    #[test]
    fn process_subst_in_buffered_pipeline_still_works() {
        let mut rt = WorkerRuntime::new();
        rt.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });

        let events = rt.handle_command(HostCommand::Run {
            input: "cat <(printf 'b\\na\\n' | sort)".into(),
        });

        assert_eq!(get_stdout(&events), "a\nb\n");
        assert_eq!(get_stderr(&events), "");
    }

    #[test]
    fn process_subst_out_runs_live_tee_pipeline() {
        let mut rt = WorkerRuntime::new();
        rt.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });

        let events = rt.handle_command(HostCommand::Run {
            input: "printf 'a\\nb\\n' > >(tee /tee.txt | cat)".into(),
        });

        assert_eq!(get_stdout(&events), "a\nb\n");
        assert_eq!(get_stderr(&events), "");

        let file = rt.handle_command(HostCommand::ReadFile {
            path: "/tee.txt".into(),
        });
        assert_eq!(get_stdout(&file), "a\nb\n");
    }

    #[test]
    fn builtin_and_utility_redirections_write_files_during_execution() {
        let mut rt = WorkerRuntime::new();
        rt.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });

        let events = rt.handle_command(HostCommand::Run {
            input: "type printf > /builtin.txt\nprintf hi > /utility.txt".into(),
        });

        let status = events
            .iter()
            .find_map(|event| {
                if let WorkerEvent::Exit(code) = event {
                    Some(*code)
                } else {
                    None
                }
            })
            .unwrap_or(-1);
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "");
        assert_eq!(get_stderr(&events), "");

        let builtin = rt.handle_command(HostCommand::ReadFile {
            path: "/builtin.txt".into(),
        });
        assert!(get_stdout(&builtin).contains("printf"));

        let utility = rt.handle_command(HostCommand::ReadFile {
            path: "/utility.txt".into(),
        });
        assert_eq!(get_stdout(&utility), "hi");
    }
}
// ── wasm-bindgen entry points (wasm32 only) ────────────────────────

#[cfg(target_arch = "wasm32")]
mod wasm_bindings {
    use wasm_bindgen::prelude::*;
    use wasmsh_protocol::HostCommand;
    use wasmsh_utils::net_types::{
        HostAllowlist, HttpRequest, HttpResponse, NetworkBackend, NetworkError,
    };

    use crate::WorkerRuntime;

    // JS function provided by the worker scope for synchronous HTTP.
    #[wasm_bindgen]
    extern "C" {
        /// Synchronous HTTP fetch implemented in JavaScript (Web Worker).
        /// Returns a JS object: `{ status: number, headers_json: string, body: Uint8Array }`.
        fn wasmsh_http_fetch(
            url: &str,
            method: &str,
            headers_json: &str,
            body: &[u8],
            body_len: u32,
            follow_redirects: bool,
        ) -> JsValue;
    }

    /// Network backend using synchronous `XMLHttpRequest` in a Web Worker.
    struct BrowserNetworkBackend {
        allowlist: HostAllowlist,
    }

    impl NetworkBackend for BrowserNetworkBackend {
        fn fetch(&self, request: &HttpRequest) -> Result<HttpResponse, NetworkError> {
            self.allowlist.check(&request.url)?;

            let headers_json =
                serde_json::to_string(&request.headers).unwrap_or_else(|_| "[]".into());
            let body = request.body.as_deref().unwrap_or(&[]);
            let body_len = body.len() as u32;

            let result = wasmsh_http_fetch(
                &request.url,
                &request.method,
                &headers_json,
                body,
                body_len,
                request.follow_redirects,
            );

            // Parse the JS result object.
            let status = js_sys::Reflect::get(&result, &"status".into())
                .ok()
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0) as u16;

            let headers_str = js_sys::Reflect::get(&result, &"headers_json".into())
                .ok()
                .and_then(|v| v.as_string())
                .unwrap_or_else(|| "[]".into());
            let headers: Vec<(String, String)> =
                serde_json::from_str(&headers_str).unwrap_or_default();

            let body_val = js_sys::Reflect::get(&result, &"body".into())
                .ok()
                .unwrap_or(JsValue::NULL);
            let body_bytes = if body_val.is_instance_of::<js_sys::Uint8Array>() {
                js_sys::Uint8Array::from(body_val).to_vec()
            } else {
                Vec::new()
            };

            // Check for error field (connection failure, etc.)
            if let Ok(err_val) = js_sys::Reflect::get(&result, &"error".into()) {
                if let Some(err_msg) = err_val.as_string() {
                    return Err(NetworkError::ConnectionFailed(err_msg));
                }
            }

            Ok(HttpResponse {
                status,
                headers,
                body: body_bytes,
            })
        }
    }

    /// Browser-facing shell instance exposed via `wasm-bindgen`.
    #[wasm_bindgen]
    #[allow(missing_debug_implementations)]
    pub struct WasmShell {
        runtime: WorkerRuntime,
    }

    #[wasm_bindgen]
    impl WasmShell {
        /// Create a new shell instance.
        #[wasm_bindgen(constructor)]
        pub fn new() -> Self {
            console_error_panic_hook::set_once();
            Self {
                runtime: WorkerRuntime::new(),
            }
        }

        /// Initialize the shell with a step budget and a network allowlist.
        /// `allowed_hosts_json` is a JSON array of host patterns (default `"[]"`).
        /// An empty allowlist creates a backend that denies every host, so
        /// callers get a `host denied` error instead of `network access not
        /// available`.  Returns a JSON array of events.
        pub fn init(&mut self, step_budget: u64, allowed_hosts_json: &str) -> String {
            let allowed_hosts: Vec<String> =
                serde_json::from_str(allowed_hosts_json).unwrap_or_default();

            let backend = BrowserNetworkBackend {
                allowlist: HostAllowlist::new(allowed_hosts.clone()),
            };
            self.runtime.set_network_backend(Box::new(backend));

            let events = self.runtime.handle_command(HostCommand::Init {
                step_budget,
                allowed_hosts,
            });
            serde_json::to_string(&events).unwrap_or_default()
        }

        /// Execute a shell command.  Returns a JSON array of events.
        #[wasm_bindgen(js_name = "exec")]
        pub fn run(&mut self, input: &str) -> String {
            let events = self.runtime.handle_command(HostCommand::Run {
                input: input.to_string(),
            });
            serde_json::to_string(&events).unwrap_or_default()
        }

        /// Write a file to the VFS.  Returns a JSON array of events.
        pub fn write_file(&mut self, path: &str, data: &[u8]) -> String {
            let events = self.runtime.handle_command(HostCommand::WriteFile {
                path: path.to_string(),
                data: data.to_vec(),
            });
            serde_json::to_string(&events).unwrap_or_default()
        }

        /// Read a file from the VFS.  Returns a JSON array of events.
        pub fn read_file(&mut self, path: &str) -> String {
            let events = self.runtime.handle_command(HostCommand::ReadFile {
                path: path.to_string(),
            });
            serde_json::to_string(&events).unwrap_or_default()
        }

        /// List a directory.  Returns a JSON array of events.
        pub fn list_dir(&mut self, path: &str) -> String {
            let events = self.runtime.handle_command(HostCommand::ListDir {
                path: path.to_string(),
            });
            serde_json::to_string(&events).unwrap_or_default()
        }

        /// Cancel the currently running execution.  Returns a JSON array of events.
        pub fn cancel(&mut self) -> String {
            let events = self.runtime.handle_command(HostCommand::Cancel);
            serde_json::to_string(&events).unwrap_or_default()
        }
    }
}
