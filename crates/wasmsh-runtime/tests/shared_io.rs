mod common;

use common::{get_exit, get_stderr, get_stdout};
use wasmsh_protocol::HostCommand;
use wasmsh_runtime::{ExternalCommandResult, WorkerRuntime};

fn install_hostcat(rt: &mut WorkerRuntime) {
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
}

#[test]
fn builtin_path_uses_same_io_redirection_model() {
    let mut rt = WorkerRuntime::new();
    rt.handle_command(HostCommand::Init {
        step_budget: 0,
        allowed_hosts: vec![],
    });

    let events = rt.handle_command(HostCommand::Run {
        input: "printf hello > /out.txt; cat /out.txt".into(),
    });

    assert_eq!(get_stdout(&events), "hello");
}

#[test]
fn utility_path_uses_same_io_redirection_model() {
    let mut rt = WorkerRuntime::new();
    rt.handle_command(HostCommand::Init {
        step_budget: 0,
        allowed_hosts: vec![],
    });
    rt.handle_command(HostCommand::WriteFile {
        path: "/in.txt".into(),
        data: b"input\n".to_vec(),
    });

    let events = rt.handle_command(HostCommand::Run {
        input: "cat < /in.txt".into(),
    });

    assert_eq!(get_stdout(&events), "input\n");
}

#[test]
fn external_path_uses_same_io_redirection_model() {
    let mut rt = WorkerRuntime::new();
    install_hostcat(&mut rt);
    rt.handle_command(HostCommand::Init {
        step_budget: 0,
        allowed_hosts: vec![],
    });
    rt.handle_command(HostCommand::WriteFile {
        path: "/in.txt".into(),
        data: b"input\n".to_vec(),
    });

    let events = rt.handle_command(HostCommand::Run {
        input: "hostcat < /in.txt".into(),
    });

    assert_eq!(get_stdout(&events), "input\n");
}

#[test]
fn function_path_uses_same_io_redirection_model() {
    let mut rt = WorkerRuntime::new();
    rt.handle_command(HostCommand::Init {
        step_budget: 0,
        allowed_hosts: vec![],
    });
    rt.handle_command(HostCommand::WriteFile {
        path: "/in.txt".into(),
        data: b"input\n".to_vec(),
    });

    let events = rt.handle_command(HostCommand::Run {
        input: "f(){ cat; }\nf < /in.txt > /out.txt; cat /out.txt".into(),
    });

    assert_eq!(get_stdout(&events), "input\n");
}

#[test]
fn mixed_pipeline_external_and_file_redirection_share_io_model() {
    let mut rt = WorkerRuntime::new();
    install_hostcat(&mut rt);
    rt.handle_command(HostCommand::Init {
        step_budget: 0,
        allowed_hosts: vec![],
    });

    let events = rt.handle_command(HostCommand::Run {
        input: "printf hi | hostcat > /out.txt; cat /out.txt".into(),
    });

    assert_eq!(get_stdout(&events), "hi");
}

#[test]
fn function_shadowing_utility_takes_precedence() {
    let mut rt = WorkerRuntime::new();
    rt.handle_command(HostCommand::Init {
        step_budget: 0,
        allowed_hosts: vec![],
    });
    rt.handle_command(HostCommand::WriteFile {
        path: "/in.txt".into(),
        data: b"utility\n".to_vec(),
    });

    let events = rt.handle_command(HostCommand::Run {
        input: "cat(){ printf function; }\ncat < /in.txt > /out.txt; cat /out.txt".into(),
    });

    assert_eq!(get_stdout(&events), "function");
}

#[test]
fn builtin_keyword_bypasses_function_shadowing() {
    let mut rt = WorkerRuntime::new();
    rt.handle_command(HostCommand::Init {
        step_budget: 0,
        allowed_hosts: vec![],
    });

    let events = rt.handle_command(HostCommand::Run {
        input: "printf(){ echo function; }\nprintf > /fn.txt; builtin printf builtin > /builtin.txt; cat /fn.txt; cat /builtin.txt".into(),
    });

    assert_eq!(get_stdout(&events), "function\nbuiltin");
}

#[test]
fn nounset_builtin_expansion_surfaces_error_through_vm_subset_path() {
    let mut rt = WorkerRuntime::new();
    rt.handle_command(HostCommand::Init {
        step_budget: 0,
        allowed_hosts: vec![],
    });

    let events = rt.handle_command(HostCommand::Run {
        input: "set -u; echo $UNSET_VAR".into(),
    });

    assert_eq!(get_stdout(&events), "");
    assert!(
        get_stderr(&events).contains("UNSET_VAR: unbound variable"),
        "stderr = {:?}",
        get_stderr(&events)
    );
    assert_eq!(get_exit(&events), 1);
}

#[test]
fn nounset_assignment_expansion_surfaces_error_through_vm_subset_path() {
    let mut rt = WorkerRuntime::new();
    rt.handle_command(HostCommand::Init {
        step_budget: 0,
        allowed_hosts: vec![],
    });

    let events = rt.handle_command(HostCommand::Run {
        input: "set -u; FOO=$UNSET_VAR".into(),
    });

    assert!(
        get_stderr(&events).contains("UNSET_VAR: unbound variable"),
        "stderr = {:?}",
        get_stderr(&events)
    );
    assert_eq!(get_exit(&events), 1);
}

#[test]
fn source_uses_redirected_io_and_preserves_shell_state() {
    let mut rt = WorkerRuntime::new();
    rt.handle_command(HostCommand::Init {
        step_budget: 0,
        allowed_hosts: vec![],
    });
    rt.handle_command(HostCommand::WriteFile {
        path: "/lib.sh".into(),
        data: b"echo sourced\nX=loaded\n".to_vec(),
    });

    let events = rt.handle_command(HostCommand::Run {
        input: "source /lib.sh > /out.txt; cat /out.txt; echo $X".into(),
    });

    assert_eq!(get_stdout(&events), "sourced\nloaded\n");
}
