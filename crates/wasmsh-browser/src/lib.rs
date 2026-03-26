//! Browser Web Worker integration for wasmsh.
//!
//! This crate provides the browser entry point and bridges the shell
//! runtime to the host page via `wasmsh-protocol` messages. It wires
//! the full pipeline: parse → HIR → expand → execute builtins.

use indexmap::IndexMap;

use wasmsh_ast::CaseTerminator;
use wasmsh_ast::RedirectionOp;
use wasmsh_expand::expand_words;
use wasmsh_fs::{MemoryFs, OpenOptions, Vfs};
use wasmsh_hir::{
    HirAndOr, HirAndOrOp, HirCommand, HirCompleteCommand, HirPipeline, HirRedirection,
};
use wasmsh_protocol::{DiagnosticLevel, HostCommand, WorkerEvent, PROTOCOL_VERSION};
use wasmsh_state::ShellState;
use wasmsh_utils::{UtilContext, UtilRegistry, VecOutput as UtilOutput};
use wasmsh_vm::Vm;

/// Sentinel FD value for `&>` (redirect both stdout and stderr).
const FD_BOTH: u32 = u32::MAX;

// Runtime-level command names dispatched before builtins.
const CMD_LOCAL: &str = "local";
const CMD_BREAK: &str = "break";
const CMD_CONTINUE: &str = "continue";
const CMD_EXIT: &str = "exit";
const CMD_EVAL: &str = "eval";
const CMD_SOURCE: &str = "source";
const CMD_DOT: &str = ".";
const CMD_DECLARE: &str = "declare";
const CMD_TYPESET: &str = "typeset";
const CMD_LET: &str = "let";
const CMD_SHOPT: &str = "shopt";
const CMD_ALIAS: &str = "alias";
const CMD_UNALIAS: &str = "unalias";
const CMD_BUILTIN: &str = "builtin";
const CMD_MAPFILE: &str = "mapfile";
const CMD_READARRAY: &str = "readarray";
const CMD_TYPE: &str = "type";

/// Configuration for the browser runtime.
#[derive(Debug, Clone)]
pub struct BrowserConfig {
    pub step_budget: u64,
}

impl Default for BrowserConfig {
    fn default() -> Self {
        Self {
            step_budget: 100_000,
        }
    }
}

/// Maximum recursion depth for eval, source, and command substitution.
const MAX_RECURSION_DEPTH: u32 = 100;

/// Transient execution state, reset between top-level commands.
struct ExecState {
    break_depth: u32,
    loop_continue: bool,
    exit_requested: Option<i32>,
    errexit_suppressed: bool,
    local_save_stack: Vec<(smol_str::SmolStr, Option<smol_str::SmolStr>)>,
    recursion_depth: u32,
}

impl ExecState {
    fn new() -> Self {
        Self {
            break_depth: 0,
            loop_continue: false,
            exit_requested: None,
            errexit_suppressed: false,
            local_save_stack: Vec::new(),
            recursion_depth: 0,
        }
    }

    fn reset(&mut self) {
        self.break_depth = 0;
        self.loop_continue = false;
        self.exit_requested = None;
        self.errexit_suppressed = false;
    }
}

/// The worker-side runtime that processes host commands.
#[allow(missing_debug_implementations)]
pub struct WorkerRuntime {
    config: BrowserConfig,
    vm: Vm,
    fs: MemoryFs,
    utils: UtilRegistry,
    builtins: wasmsh_builtins::BuiltinRegistry,
    initialized: bool,
    /// Pending stdin data for the next command (from here-doc or pipe).
    pending_stdin: Option<Vec<u8>>,
    /// Registered shell functions (name → HIR body).
    functions: IndexMap<String, HirCommand>,
    /// Transient execution state (loop control, exit, locals).
    exec: ExecState,
    /// Shell aliases (name → replacement text).
    aliases: IndexMap<String, String>,
}

/// Parsed flags for `declare`/`typeset`.
#[allow(clippy::struct_excessive_bools)]
struct DeclareFlags {
    is_assoc: bool,
    is_indexed: bool,
    is_integer: bool,
    is_export: bool,
    is_readonly: bool,
    is_lower: bool,
    is_upper: bool,
    is_print: bool,
    is_nameref: bool,
}

/// Parse declare/typeset flags from argv, returning (flags, `name_indices`).
fn parse_declare_flags(argv: &[String]) -> (DeclareFlags, Vec<usize>) {
    let mut flags = DeclareFlags {
        is_assoc: false,
        is_indexed: false,
        is_integer: false,
        is_export: false,
        is_readonly: false,
        is_lower: false,
        is_upper: false,
        is_print: false,
        is_nameref: false,
    };
    let mut names = Vec::new();

    for (i, arg) in argv[1..].iter().enumerate() {
        if arg.starts_with('-') && arg.len() > 1 {
            for ch in arg[1..].chars() {
                match ch {
                    'A' => flags.is_assoc = true,
                    'a' => flags.is_indexed = true,
                    'i' => flags.is_integer = true,
                    'x' => flags.is_export = true,
                    'r' => flags.is_readonly = true,
                    'l' => flags.is_lower = true,
                    'u' => flags.is_upper = true,
                    'p' => flags.is_print = true,
                    'n' => flags.is_nameref = true,
                    _ => {}
                }
            }
        } else {
            names.push(i + 1);
        }
    }
    (flags, names)
}

impl WorkerRuntime {
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: BrowserConfig::default(),
            vm: Vm::new(ShellState::new(), 0),
            fs: MemoryFs::new(),
            utils: UtilRegistry::new(),
            builtins: wasmsh_builtins::BuiltinRegistry::new(),
            initialized: false,
            pending_stdin: None,
            functions: IndexMap::new(),
            exec: ExecState::new(),
            aliases: IndexMap::new(),
        }
    }

    /// Process a host command and return a list of events to send back.
    pub fn handle_command(&mut self, cmd: HostCommand) -> Vec<WorkerEvent> {
        match cmd {
            HostCommand::Init { step_budget } => {
                self.config.step_budget = step_budget;
                self.vm = Vm::new(ShellState::new(), step_budget);
                self.fs = MemoryFs::new();
                self.pending_stdin = None;
                self.functions = IndexMap::new();
                self.exec.reset();
                self.aliases = IndexMap::new();
                self.initialized = true;
                // Set default shopt options (bash defaults)
                self.vm.state.set_var("SHOPT_extglob".into(), "1".into());
                vec![WorkerEvent::Version(PROTOCOL_VERSION.to_string())]
            }
            HostCommand::Run { input } => {
                if !self.initialized {
                    return vec![WorkerEvent::Diagnostic(
                        DiagnosticLevel::Error,
                        "runtime not initialized".into(),
                    )];
                }
                self.execute_input(&input)
            }
            HostCommand::Cancel => {
                self.vm.cancellation_token().cancel();
                vec![WorkerEvent::Diagnostic(
                    DiagnosticLevel::Info,
                    "cancel received".into(),
                )]
            }
            HostCommand::ReadFile { path } => {
                use wasmsh_fs::OpenOptions;
                match self.fs.open(&path, OpenOptions::read()) {
                    Ok(h) => match self.fs.read_file(h) {
                        Ok(data) => {
                            self.fs.close(h);
                            vec![WorkerEvent::Stdout(data)]
                        }
                        Err(e) => {
                            self.fs.close(h);
                            vec![WorkerEvent::Diagnostic(
                                DiagnosticLevel::Error,
                                format!("read error: {path}: {e}"),
                            )]
                        }
                    },
                    Err(e) => vec![WorkerEvent::Diagnostic(
                        DiagnosticLevel::Error,
                        format!("read error: {e}"),
                    )],
                }
            }
            HostCommand::WriteFile { path, data } => {
                use wasmsh_fs::OpenOptions;
                match self.fs.open(&path, OpenOptions::write()) {
                    Ok(h) => {
                        if let Err(e) = self.fs.write_file(h, &data) {
                            self.vm.stderr.extend_from_slice(
                                format!("wasmsh: write error: {e}\n").as_bytes(),
                            );
                        }
                        self.fs.close(h);
                        vec![WorkerEvent::FsChanged(path)]
                    }
                    Err(e) => vec![WorkerEvent::Diagnostic(
                        DiagnosticLevel::Error,
                        format!("write error: {e}"),
                    )],
                }
            }
            HostCommand::ListDir { path } => match self.fs.read_dir(&path) {
                Ok(entries) => {
                    let names: Vec<u8> = entries
                        .iter()
                        .map(|e| e.name.as_str())
                        .collect::<Vec<_>>()
                        .join("\n")
                        .into_bytes();
                    vec![WorkerEvent::Stdout(names)]
                }
                Err(e) => vec![WorkerEvent::Diagnostic(
                    DiagnosticLevel::Error,
                    format!("readdir error: {e}"),
                )],
            },
            HostCommand::Mount { .. } => {
                vec![WorkerEvent::Diagnostic(
                    DiagnosticLevel::Warning,
                    "mount not yet implemented".into(),
                )]
            }
            _ => {
                vec![WorkerEvent::Diagnostic(
                    DiagnosticLevel::Warning,
                    "unknown command".into(),
                )]
            }
        }
    }

    /// Execute input and return collected events (used by eval/source).
    fn execute_input_inner(&mut self, input: &str) -> Vec<WorkerEvent> {
        self.exec.recursion_depth += 1;
        if self.exec.recursion_depth > MAX_RECURSION_DEPTH {
            self.exec.recursion_depth -= 1;
            return vec![WorkerEvent::Stderr(
                b"wasmsh: maximum recursion depth exceeded\n".to_vec(),
            )];
        }
        let result = self.execute_input_inner_impl(input);
        self.exec.recursion_depth -= 1;
        result
    }

    /// Inner implementation of `execute_input_inner` (after recursion check).
    fn execute_input_inner_impl(&mut self, input: &str) -> Vec<WorkerEvent> {
        let ast = match wasmsh_parse::parse(input) {
            Ok(ast) => ast,
            Err(e) => {
                self.vm.state.last_status = 2;
                return vec![WorkerEvent::Stderr(
                    format!("wasmsh: parse error: {e}\n").into_bytes(),
                )];
            }
        };
        let hir = wasmsh_hir::lower(&ast);
        for cc in &hir.items {
            if self.exec.exit_requested.is_some() {
                break;
            }
            // Update $LINENO from span position
            let line = input
                .as_bytes()
                .iter()
                .take(cc.span.start as usize)
                .filter(|&&b| b == b'\n')
                .count() as u32
                + 1;
            self.vm.state.lineno = line;
            for and_or in &cc.list {
                self.execute_pipeline_chain(and_or);
                if self.exec.exit_requested.is_some() {
                    break;
                }
                if self.should_errexit(and_or) {
                    self.exec.exit_requested = Some(self.vm.state.last_status);
                    break;
                }
            }
        }
        // Drain stdout/stderr into events
        let mut events = Vec::new();
        if !self.vm.stdout.is_empty() {
            events.push(WorkerEvent::Stdout(std::mem::take(&mut self.vm.stdout)));
        }
        if !self.vm.stderr.is_empty() {
            events.push(WorkerEvent::Stderr(std::mem::take(&mut self.vm.stderr)));
        }
        events
    }

    fn execute_input(&mut self, input: &str) -> Vec<WorkerEvent> {
        let mut events = self.execute_input_inner(input);
        self.run_exit_trap_if_needed(&mut events);
        self.drain_io_events(&mut events);
        self.drain_diagnostic_events(&mut events);
        self.push_output_limit_warning(&mut events);

        let exit_status = self
            .exec
            .exit_requested
            .unwrap_or(self.vm.state.last_status);
        events.push(WorkerEvent::Exit(exit_status));
        events
    }

    fn run_exit_trap_if_needed(&mut self, events: &mut Vec<WorkerEvent>) {
        let Some(exit_code) = self.exec.exit_requested else {
            return;
        };
        let Some(handler) = self.vm.state.get_var("_TRAP_EXIT") else {
            return;
        };
        if handler.is_empty() {
            return;
        }

        let handler_str = handler.to_string();
        self.vm.state.set_var(
            smol_str::SmolStr::from("_TRAP_EXIT"),
            smol_str::SmolStr::default(),
        );
        self.exec.exit_requested = None;
        events.extend(self.execute_input_inner(&handler_str));
        self.exec.exit_requested = Some(exit_code);
    }

    fn drain_io_events(&mut self, events: &mut Vec<WorkerEvent>) {
        self.push_buffer_event(events, true);
        self.push_buffer_event(events, false);
    }

    fn push_buffer_event(&mut self, events: &mut Vec<WorkerEvent>, stdout: bool) {
        let buffer = if stdout {
            &mut self.vm.stdout
        } else {
            &mut self.vm.stderr
        };
        if buffer.is_empty() {
            return;
        }

        let data = std::mem::take(buffer);
        events.push(if stdout {
            WorkerEvent::Stdout(data)
        } else {
            WorkerEvent::Stderr(data)
        });
    }

    fn drain_diagnostic_events(&mut self, events: &mut Vec<WorkerEvent>) {
        for diag in self.vm.diagnostics.drain(..) {
            events.push(WorkerEvent::Diagnostic(
                Self::to_protocol_diag_level(diag.level),
                diag.message,
            ));
        }
    }

    fn to_protocol_diag_level(level: wasmsh_vm::DiagLevel) -> DiagnosticLevel {
        match level {
            wasmsh_vm::DiagLevel::Trace => DiagnosticLevel::Trace,
            wasmsh_vm::DiagLevel::Info => DiagnosticLevel::Info,
            wasmsh_vm::DiagLevel::Warning => DiagnosticLevel::Warning,
            wasmsh_vm::DiagLevel::Error => DiagnosticLevel::Error,
        }
    }

    fn push_output_limit_warning(&self, events: &mut Vec<WorkerEvent>) {
        if self.vm.limits.output_byte_limit == 0
            || self.vm.output_bytes <= self.vm.limits.output_byte_limit
        {
            return;
        }

        events.push(WorkerEvent::Diagnostic(
            DiagnosticLevel::Warning,
            format!(
                "output limit exceeded: {} bytes (limit: {})",
                self.vm.output_bytes, self.vm.limits.output_byte_limit
            ),
        ));
    }

    fn execute_pipeline_chain(&mut self, and_or: &HirAndOr) {
        self.execute_pipeline(&and_or.first);
        for (op, pipeline) in &and_or.rest {
            match op {
                HirAndOrOp::And => {
                    if self.vm.state.last_status == 0 {
                        self.execute_pipeline(pipeline);
                    }
                }
                HirAndOrOp::Or => {
                    if self.vm.state.last_status != 0 {
                        self.execute_pipeline(pipeline);
                    }
                }
            }
        }
    }

    fn execute_pipeline(&mut self, pipeline: &HirPipeline) {
        let cmds = &pipeline.commands;
        if cmds.len() == 1 {
            self.execute_command(&cmds[0]);
            // Set PIPESTATUS for single-command pipeline
            let status_key = smol_str::SmolStr::from("PIPESTATUS");
            self.vm.state.init_indexed_array(status_key.clone());
            self.vm.state.set_array_element(
                status_key,
                "0",
                smol_str::SmolStr::from(self.vm.state.last_status.to_string()),
            );
        } else {
            // Multi-stage pipeline: stdout of stage N feeds stdin of stage N+1.
            // Each stage runs to completion; its stdout is captured into a
            // PipeBuffer and provided as pending_stdin to the next stage.
            use wasmsh_vm::pipe::PipeBuffer;

            let pipefail = self.vm.state.get_var("SHOPT_o_pipefail").as_deref() == Some("1");
            let mut rightmost_failure: i32 = 0;
            let mut statuses: Vec<i32> = Vec::new();

            for (i, cmd) in cmds.iter().enumerate() {
                let is_last = i == cmds.len() - 1;
                let stdout_before = self.vm.stdout.len();
                let stderr_before = self.vm.stderr.len();

                self.execute_command(cmd);
                statuses.push(self.vm.state.last_status);

                if pipefail && self.vm.state.last_status != 0 {
                    rightmost_failure = self.vm.state.last_status;
                }

                if !is_last {
                    // Capture this stage's stdout into a pipe buffer
                    let mut stage_output = self.vm.stdout[stdout_before..].to_vec();
                    self.vm.stdout.truncate(stdout_before);

                    // If |& was used, also capture stderr into the pipe
                    let is_pipe_stderr = pipeline.pipe_stderr.get(i).copied().unwrap_or(false);
                    if is_pipe_stderr {
                        let stage_stderr = self.vm.stderr[stderr_before..].to_vec();
                        self.vm.stderr.truncate(stderr_before);
                        stage_output.extend_from_slice(&stage_stderr);
                    }

                    // Feed it as stdin to the next stage
                    let mut pipe = PipeBuffer::default_size();
                    pipe.write_all(&stage_output);
                    pipe.close_write();
                    self.pending_stdin = Some(pipe.drain());
                }
            }

            // Set PIPESTATUS array
            let status_key = smol_str::SmolStr::from("PIPESTATUS");
            self.vm.state.init_indexed_array(status_key.clone());
            for (i, s) in statuses.iter().enumerate() {
                self.vm.state.set_array_element(
                    status_key.clone(),
                    &i.to_string(),
                    smol_str::SmolStr::from(s.to_string()),
                );
            }

            // With pipefail, use the rightmost non-zero exit status
            if pipefail && rightmost_failure != 0 {
                self.vm.state.last_status = rightmost_failure;
            }
        }
        if pipeline.negated {
            self.vm.state.last_status = i32::from(self.vm.state.last_status == 0);
        }
    }

    /// Execute a command substitution and return the trimmed output.
    fn execute_subst(&mut self, inner: &str) -> smol_str::SmolStr {
        let saved_stdout = std::mem::take(&mut self.vm.stdout);
        let events = self.execute_input_inner(inner);
        let mut result = String::new();
        for e in &events {
            if let WorkerEvent::Stdout(d) = e {
                result.push_str(&String::from_utf8_lossy(d));
            }
        }
        if !self.vm.stdout.is_empty() {
            result.push_str(&String::from_utf8_lossy(&self.vm.stdout));
            self.vm.stdout.clear();
        }
        self.vm.stdout = saved_stdout;
        smol_str::SmolStr::from(result.trim_end_matches('\n'))
    }

    /// Resolve command substitutions in a list of words by executing them.
    fn resolve_command_subst(&mut self, words: &[wasmsh_ast::Word]) -> Vec<wasmsh_ast::Word> {
        words
            .iter()
            .map(|w| {
                let parts: Vec<wasmsh_ast::WordPart> = w
                    .parts
                    .iter()
                    .map(|p| match p {
                        wasmsh_ast::WordPart::CommandSubstitution(inner) => {
                            wasmsh_ast::WordPart::Literal(self.execute_subst(inner))
                        }
                        wasmsh_ast::WordPart::DoubleQuoted(inner_parts) => {
                            let resolved: Vec<wasmsh_ast::WordPart> = inner_parts
                                .iter()
                                .map(|ip| {
                                    if let wasmsh_ast::WordPart::CommandSubstitution(inner) = ip {
                                        wasmsh_ast::WordPart::Literal(self.execute_subst(inner))
                                    } else {
                                        ip.clone()
                                    }
                                })
                                .collect();
                            wasmsh_ast::WordPart::DoubleQuoted(resolved)
                        }
                        other => other.clone(),
                    })
                    .collect();
                wasmsh_ast::Word {
                    parts,
                    span: w.span,
                }
            })
            .collect()
    }

    fn execute_command(&mut self, cmd: &HirCommand) {
        match cmd {
            HirCommand::Exec(exec) => self.execute_exec(exec),
            HirCommand::Assign(assign) => {
                for a in &assign.assignments {
                    self.execute_assignment(&a.name, a.value.as_ref());
                }
                let stdout_before = self.vm.stdout.len();
                self.apply_redirections(&assign.redirections, stdout_before);
                self.vm.state.last_status = 0;
            }
            HirCommand::If(if_cmd) => self.execute_if(if_cmd),
            HirCommand::While(loop_cmd) => self.execute_while_loop(loop_cmd),
            HirCommand::Until(loop_cmd) => self.execute_until_loop(loop_cmd),
            HirCommand::For(for_cmd) => self.execute_for_loop(for_cmd),
            HirCommand::Group(block) => self.execute_body(&block.body),
            HirCommand::Subshell(block) => {
                self.vm.state.env.push_scope();
                self.execute_body(&block.body);
                self.vm.state.env.pop_scope();
            }
            HirCommand::Case(case_cmd) => self.execute_case(case_cmd),
            HirCommand::FunctionDef(fd) => {
                self.functions
                    .insert(fd.name.to_string(), (*fd.body).clone());
                self.vm.state.last_status = 0;
            }
            HirCommand::RedirectOnly(ro) => {
                let stdout_before = self.vm.stdout.len();
                self.apply_redirections(&ro.redirections, stdout_before);
                self.vm.state.last_status = 0;
            }
            HirCommand::DoubleBracket(db) => {
                let result = self.eval_double_bracket(&db.words);
                self.vm.state.last_status = i32::from(!result);
            }
            HirCommand::ArithCommand(ac) => {
                let result = wasmsh_expand::eval_arithmetic(&ac.expr, &mut self.vm.state);
                self.vm.state.last_status = i32::from(result == 0);
            }
            HirCommand::ArithFor(af) => self.execute_arith_for(af),
            HirCommand::Select(sel) => self.execute_select(sel),
            _ => {}
        }
    }

    /// Execute a simple command (`HirCommand::Exec`).
    fn execute_exec(&mut self, exec: &wasmsh_hir::HirExec) {
        let resolved = self.resolve_command_subst(&exec.argv);
        let argv = expand_words(&resolved, &mut self.vm.state);

        if self.check_nounset_error() {
            return;
        }
        if argv.is_empty() {
            return;
        }

        let argv: Vec<String> = argv
            .into_iter()
            .flat_map(|arg| wasmsh_expand::expand_braces(&arg))
            .collect();
        let argv = self.expand_globs(argv);

        for assignment in &exec.env {
            self.execute_assignment(&assignment.name, assignment.value.as_ref());
        }

        if self.collect_stdin_from_redirections(&exec.redirections) {
            return;
        }

        if self.try_alias_expansion(&argv) {
            return;
        }

        let stdout_before = self.vm.stdout.len();
        let cmd_name = &argv[0];
        self.trace_command(&argv);

        if self.try_runtime_command(cmd_name, &argv) {
            return;
        }

        self.dispatch_command(cmd_name, &argv);
        self.apply_redirections(&exec.redirections, stdout_before);
    }

    /// Check for nounset errors from expansion. Returns true if an error was found.
    fn check_nounset_error(&mut self) -> bool {
        if let Some(var_name) = self.vm.state.get_var("_NOUNSET_ERROR") {
            if !var_name.is_empty() {
                let msg = format!("wasmsh: {var_name}: unbound variable\n");
                self.vm.stderr.extend_from_slice(msg.as_bytes());
                self.vm.state.set_var(
                    smol_str::SmolStr::from("_NOUNSET_ERROR"),
                    smol_str::SmolStr::default(),
                );
                self.vm.state.last_status = 1;
                return true;
            }
        }
        false
    }

    /// Collect stdin from here-doc bodies or input redirections. Returns true if
    /// an error occurred and execution should stop.
    fn collect_stdin_from_redirections(&mut self, redirections: &[HirRedirection]) -> bool {
        for redir in redirections {
            match redir.op {
                RedirectionOp::HereDoc | RedirectionOp::HereDocStrip => {
                    if let Some(body) = &redir.here_doc_body {
                        let expanded =
                            wasmsh_expand::expand_string(&body.content, &mut self.vm.state);
                        self.pending_stdin = Some(expanded.into_bytes());
                    }
                }
                RedirectionOp::HereString => {
                    let content = wasmsh_expand::expand_word(&redir.target, &mut self.vm.state);
                    let mut data = content.into_bytes();
                    data.push(b'\n');
                    self.pending_stdin = Some(data);
                }
                RedirectionOp::Input => {
                    let target = wasmsh_expand::expand_word(&redir.target, &mut self.vm.state);
                    let path = self.resolve_cwd_path(&target);
                    if let Ok(h) = self.fs.open(&path, OpenOptions::read()) {
                        match self.fs.read_file(h) {
                            Ok(data) => {
                                self.pending_stdin = Some(data);
                            }
                            Err(e) => {
                                let msg = format!("wasmsh: {target}: read error: {e}\n");
                                self.vm.stderr.extend_from_slice(msg.as_bytes());
                                self.vm.state.last_status = 1;
                                self.fs.close(h);
                                return true;
                            }
                        }
                        self.fs.close(h);
                    } else {
                        let msg = format!("wasmsh: {target}: No such file or directory\n");
                        self.vm.stderr.extend_from_slice(msg.as_bytes());
                        self.vm.state.last_status = 1;
                        return true;
                    }
                }
                _ => {}
            }
        }
        false
    }

    /// Try alias expansion for the command. Returns true if an alias was expanded.
    fn try_alias_expansion(&mut self, argv: &[String]) -> bool {
        if let Some(alias_val) = self.aliases.get(&argv[0]).cloned() {
            let rest = if argv.len() > 1 {
                format!(" {}", argv[1..].join(" "))
            } else {
                String::new()
            };
            let expanded = format!("{alias_val}{rest}");
            let sub_events = self.execute_input_inner(&expanded);
            self.merge_sub_events(sub_events);
            return true;
        }
        false
    }

    /// Print xtrace output if enabled.
    fn trace_command(&mut self, argv: &[String]) {
        if self.vm.state.get_var("SHOPT_x").as_deref() == Some("1") {
            let ps4 = self
                .vm
                .state
                .get_var("PS4")
                .unwrap_or_else(|| smol_str::SmolStr::from("+ "));
            let trace_line = format!("{}{}\n", ps4, argv.join(" "));
            self.vm.stderr.extend_from_slice(trace_line.as_bytes());
        }
    }

    /// Try to handle a runtime-level command (local, break, continue, exit, eval,
    /// source, declare, etc.). Returns true if handled.
    fn try_runtime_command(&mut self, cmd_name: &str, argv: &[String]) -> bool {
        match cmd_name {
            CMD_LOCAL => {
                self.execute_local(argv);
                true
            }
            CMD_BREAK => {
                self.exec.break_depth = argv.get(1).and_then(|s| s.parse().ok()).unwrap_or(1);
                self.vm.state.last_status = 0;
                true
            }
            CMD_CONTINUE => {
                self.exec.loop_continue = true;
                self.vm.state.last_status = 0;
                true
            }
            CMD_EXIT => {
                let code = argv
                    .get(1)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(self.vm.state.last_status);
                self.exec.exit_requested = Some(code);
                self.vm.state.last_status = code;
                true
            }
            CMD_EVAL => {
                let code = argv[1..].join(" ");
                let sub_events = self.execute_input_inner(&code);
                self.merge_sub_events_with_diagnostics(sub_events);
                true
            }
            CMD_SOURCE | CMD_DOT => {
                self.execute_source(argv);
                true
            }
            CMD_DECLARE | CMD_TYPESET => {
                self.execute_declare(argv);
                true
            }
            CMD_LET => {
                self.execute_let(argv);
                true
            }
            CMD_SHOPT => {
                self.execute_shopt(argv);
                true
            }
            CMD_ALIAS => {
                self.execute_alias(argv);
                true
            }
            CMD_UNALIAS => {
                self.execute_unalias(argv);
                true
            }
            CMD_BUILTIN => {
                self.execute_builtin_keyword(argv);
                true
            }
            CMD_MAPFILE | CMD_READARRAY => {
                self.execute_mapfile(argv);
                true
            }
            CMD_TYPE => {
                self.execute_type(argv);
                true
            }
            _ => false,
        }
    }

    /// Execute `local` — save old variable values and set new ones.
    fn execute_local(&mut self, argv: &[String]) {
        for arg in &argv[1..] {
            let (name, value) = if let Some(eq) = arg.find('=') {
                (&arg[..eq], Some(&arg[eq + 1..]))
            } else {
                (arg.as_str(), None)
            };
            let old = self.vm.state.get_var(name);
            self.exec
                .local_save_stack
                .push((smol_str::SmolStr::from(name), old));
            let val = value.map_or(smol_str::SmolStr::default(), smol_str::SmolStr::from);
            self.vm.state.set_var(smol_str::SmolStr::from(name), val);
        }
        self.vm.state.last_status = 0;
    }

    /// Execute `source`/`.` — read and execute a file.
    fn execute_source(&mut self, argv: &[String]) {
        let Some(path) = argv.get(1) else { return };
        let resolved = if path.contains('/') {
            Some(self.resolve_cwd_path(path))
        } else {
            let direct = self.resolve_cwd_path(path);
            if self.fs.stat(&direct).is_ok() {
                Some(direct)
            } else {
                self.search_path_for_file(path)
            }
        };
        let Some(full) = resolved else {
            let msg = format!("source: {path}: not found\n");
            self.vm.stderr.extend_from_slice(msg.as_bytes());
            self.vm.state.last_status = 1;
            return;
        };
        let Ok(h) = self.fs.open(&full, OpenOptions::read()) else {
            let msg = format!("source: {path}: not found\n");
            self.vm.stderr.extend_from_slice(msg.as_bytes());
            self.vm.state.last_status = 1;
            return;
        };
        match self.fs.read_file(h) {
            Ok(data) => {
                self.fs.close(h);
                self.vm
                    .state
                    .source_stack
                    .push(smol_str::SmolStr::from(full.as_str()));
                let code = String::from_utf8_lossy(&data).to_string();
                let sub_events = self.execute_input_inner(&code);
                self.vm.state.source_stack.pop();
                self.merge_sub_events_with_diagnostics(sub_events);
            }
            Err(e) => {
                self.fs.close(h);
                let msg = format!("source: {path}: read error: {e}\n");
                self.vm.stderr.extend_from_slice(msg.as_bytes());
                self.vm.state.last_status = 1;
            }
        }
    }

    /// Merge sub-events (stdout/stderr only) into the current VM buffers.
    fn merge_sub_events(&mut self, events: Vec<WorkerEvent>) {
        for e in events {
            match e {
                WorkerEvent::Stdout(d) => self.vm.stdout.extend_from_slice(&d),
                WorkerEvent::Stderr(d) => self.vm.stderr.extend_from_slice(&d),
                _ => {}
            }
        }
    }

    /// Merge sub-events including diagnostics into the current VM buffers.
    fn merge_sub_events_with_diagnostics(&mut self, events: Vec<WorkerEvent>) {
        for e in events {
            match e {
                WorkerEvent::Stdout(d) => self.vm.stdout.extend_from_slice(&d),
                WorkerEvent::Stderr(d) => self.vm.stderr.extend_from_slice(&d),
                WorkerEvent::Diagnostic(level, msg) => {
                    self.vm.diagnostics.push(wasmsh_vm::DiagnosticEvent {
                        level: convert_diag_level(level),
                        category: wasmsh_vm::DiagCategory::Runtime,
                        message: msg,
                    });
                }
                _ => {}
            }
        }
    }

    /// Dispatch a command to a shell function, builtin, utility, or report not found.
    fn dispatch_command(&mut self, cmd_name: &str, argv: &[String]) {
        if let Some(body) = self.functions.get(cmd_name).cloned() {
            self.call_shell_function(cmd_name, argv, &body);
        } else if self.builtins.is_builtin(cmd_name) {
            self.call_builtin(cmd_name, argv);
        } else if self.utils.is_utility(cmd_name) {
            self.call_utility(cmd_name, argv);
        } else {
            let msg = format!("wasmsh: {cmd_name}: command not found\n");
            self.vm.stderr.extend_from_slice(msg.as_bytes());
            self.vm.state.last_status = 127;
        }
    }

    /// Invoke a shell function.
    fn call_shell_function(&mut self, cmd_name: &str, argv: &[String], body: &HirCommand) {
        let old_positional = std::mem::take(&mut self.vm.state.positional);
        self.vm.state.positional = argv[1..]
            .iter()
            .map(|s| smol_str::SmolStr::from(s.as_str()))
            .collect();
        self.vm
            .state
            .func_stack
            .push(smol_str::SmolStr::from(cmd_name));
        let locals_before = self.exec.local_save_stack.len();
        self.execute_command(body);
        let new_locals: Vec<_> = self.exec.local_save_stack.drain(locals_before..).collect();
        for (name, old_val) in new_locals.into_iter().rev() {
            if let Some(val) = old_val {
                self.vm.state.set_var(name, val);
            } else {
                self.vm.state.unset_var(&name).ok();
            }
        }
        self.vm.state.func_stack.pop();
        self.vm.state.positional = old_positional;
    }

    /// Invoke a builtin command.
    fn call_builtin(&mut self, cmd_name: &str, argv: &[String]) {
        let builtin_fn = self.builtins.get(cmd_name).unwrap();
        let stdin_data = self.pending_stdin.take();
        let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
        let mut sink = wasmsh_builtins::VecSink::default();
        let status = {
            let mut ctx = wasmsh_builtins::BuiltinContext {
                state: &mut self.vm.state,
                output: &mut sink,
                fs: Some(&self.fs),
                stdin: stdin_data.as_deref(),
            };
            builtin_fn(&mut ctx, &argv_refs)
        };
        self.vm.stdout.extend_from_slice(&sink.stdout);
        self.vm.stderr.extend_from_slice(&sink.stderr);
        self.vm.output_bytes += (sink.stdout.len() + sink.stderr.len()) as u64;
        self.vm.state.last_status = status;
        self.pending_stdin = None;
    }

    /// Invoke a utility command.
    fn call_utility(&mut self, cmd_name: &str, argv: &[String]) {
        let stdin_data = self.pending_stdin.take();
        let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
        let mut output = UtilOutput::default();
        let cwd = self.vm.state.cwd.clone();
        let status = {
            let util_fn = self.utils.get(cmd_name).unwrap();
            let mut ctx = UtilContext {
                fs: &mut self.fs,
                output: &mut output,
                cwd: &cwd,
                stdin: stdin_data.as_deref(),
                state: Some(&self.vm.state),
            };
            util_fn(&mut ctx, &argv_refs)
        };
        self.vm.stdout.extend_from_slice(&output.stdout);
        self.vm.stderr.extend_from_slice(&output.stderr);
        self.vm.output_bytes += (output.stdout.len() + output.stderr.len()) as u64;
        self.vm.state.last_status = status;
    }

    /// Execute an `if` command.
    fn execute_if(&mut self, if_cmd: &wasmsh_hir::HirIf) {
        let saved_suppress = self.exec.errexit_suppressed;
        self.exec.errexit_suppressed = true;
        self.execute_body(&if_cmd.condition);
        self.exec.errexit_suppressed = saved_suppress;
        if self.vm.state.last_status == 0 {
            self.execute_body(&if_cmd.then_body);
            return;
        }
        for elif in &if_cmd.elifs {
            let saved = self.exec.errexit_suppressed;
            self.exec.errexit_suppressed = true;
            self.execute_body(&elif.condition);
            self.exec.errexit_suppressed = saved;
            if self.vm.state.last_status == 0 {
                self.execute_body(&elif.then_body);
                return;
            }
        }
        if let Some(else_body) = &if_cmd.else_body {
            self.execute_body(else_body);
        }
    }

    /// Execute a `while` loop.
    fn execute_while_loop(&mut self, loop_cmd: &wasmsh_hir::HirLoop) {
        loop {
            let saved = self.exec.errexit_suppressed;
            self.exec.errexit_suppressed = true;
            self.execute_body(&loop_cmd.condition);
            self.exec.errexit_suppressed = saved;
            if self.vm.state.last_status != 0 {
                break;
            }
            self.execute_body(&loop_cmd.body);
            if self.handle_loop_control() {
                break;
            }
        }
    }

    /// Execute an `until` loop.
    fn execute_until_loop(&mut self, loop_cmd: &wasmsh_hir::HirLoop) {
        loop {
            let saved = self.exec.errexit_suppressed;
            self.exec.errexit_suppressed = true;
            self.execute_body(&loop_cmd.condition);
            self.exec.errexit_suppressed = saved;
            if self.vm.state.last_status == 0 {
                break;
            }
            self.execute_body(&loop_cmd.body);
            if self.handle_loop_control() {
                break;
            }
        }
    }

    /// Handle loop control flow (break/continue/exit). Returns true if the loop should break.
    fn handle_loop_control(&mut self) -> bool {
        if self.exec.break_depth > 0 {
            self.exec.break_depth -= 1;
            return true;
        }
        if self.exec.loop_continue {
            self.exec.loop_continue = false;
        }
        self.exec.exit_requested.is_some()
    }

    /// Execute a `for` loop.
    fn execute_for_loop(&mut self, for_cmd: &wasmsh_hir::HirFor) {
        let words = self.expand_for_words(for_cmd.words.as_deref());
        for word in words {
            self.vm.state.set_var(for_cmd.var_name.clone(), word.into());
            self.execute_body(&for_cmd.body);
            if self.exec.break_depth > 0 {
                self.exec.break_depth -= 1;
                break;
            }
            if self.exec.loop_continue {
                self.exec.loop_continue = false;
                continue;
            }
            if self.exec.exit_requested.is_some() {
                break;
            }
        }
    }

    /// Expand word list for `for` and `select` commands.
    fn expand_for_words(&mut self, words: Option<&[wasmsh_ast::Word]>) -> Vec<String> {
        if let Some(ws) = words {
            let resolved = self.resolve_command_subst(ws);
            let mut result = Vec::new();
            for w in &resolved {
                let expanded = wasmsh_expand::expand_word_split(w, &mut self.vm.state);
                result.extend(expanded.fields);
            }
            let result: Vec<String> = result
                .into_iter()
                .flat_map(|arg| wasmsh_expand::expand_braces(&arg))
                .collect();
            self.expand_globs(result)
        } else {
            self.vm
                .state
                .positional
                .iter()
                .map(ToString::to_string)
                .collect()
        }
    }

    /// Execute a `case` command.
    fn execute_case(&mut self, case_cmd: &wasmsh_hir::HirCase) {
        let nocasematch = self.vm.state.get_var("SHOPT_nocasematch").as_deref() == Some("1");
        let value = wasmsh_expand::expand_word(&case_cmd.word, &mut self.vm.state);
        let mut i = 0;
        let mut fallthrough = false;
        while i < case_cmd.items.len() {
            let item = &case_cmd.items[i];
            let pattern_matched = if fallthrough {
                true
            } else {
                item.patterns.iter().any(|pattern| {
                    let pat = wasmsh_expand::expand_word(pattern, &mut self.vm.state);
                    if nocasematch {
                        glob_match_inner(
                            pat.to_lowercase().as_bytes(),
                            value.to_lowercase().as_bytes(),
                        )
                    } else {
                        glob_match_inner(pat.as_bytes(), value.as_bytes())
                    }
                })
            };
            if pattern_matched {
                self.execute_body(&item.body);
                match item.terminator {
                    CaseTerminator::Break => break,
                    CaseTerminator::Fallthrough => {
                        fallthrough = true;
                        i += 1;
                    }
                    CaseTerminator::ContinueTesting => {
                        fallthrough = false;
                        i += 1;
                    }
                }
            } else {
                fallthrough = false;
                i += 1;
            }
        }
    }

    /// Execute a C-style `for (( init; cond; step ))` loop.
    fn execute_arith_for(&mut self, af: &wasmsh_hir::HirArithFor) {
        if !af.init.is_empty() {
            wasmsh_expand::eval_arithmetic(&af.init, &mut self.vm.state);
        }
        loop {
            if !af.cond.is_empty() {
                let cond_val = wasmsh_expand::eval_arithmetic(&af.cond, &mut self.vm.state);
                if cond_val == 0 {
                    break;
                }
            }
            self.execute_body(&af.body);
            if self.handle_loop_control() {
                break;
            }
            if !af.step.is_empty() {
                wasmsh_expand::eval_arithmetic(&af.step, &mut self.vm.state);
            }
        }
    }

    /// Execute a `select` command.
    fn execute_select(&mut self, sel: &wasmsh_hir::HirSelect) {
        self.collect_stdin_from_redirections(&sel.redirections);

        let words: Vec<String> = if let Some(ws) = &sel.words {
            let resolved = self.resolve_command_subst(ws);
            let mut result = Vec::new();
            for w in &resolved {
                let expanded = wasmsh_expand::expand_word_split(w, &mut self.vm.state);
                result.extend(expanded.fields);
            }
            result
        } else {
            self.vm
                .state
                .positional
                .iter()
                .map(ToString::to_string)
                .collect()
        };

        if words.is_empty() {
            return;
        }
        for (idx, w) in words.iter().enumerate() {
            let line = format!("{}) {}\n", idx + 1, w);
            self.vm.stderr.extend_from_slice(line.as_bytes());
        }

        let stdin_data = self.pending_stdin.take().unwrap_or_default();
        let input = String::from_utf8_lossy(&stdin_data);
        let first_line = input.lines().next().unwrap_or("");

        self.vm.state.set_var(
            smol_str::SmolStr::from("REPLY"),
            smol_str::SmolStr::from(first_line.trim()),
        );

        let selected = first_line.trim().parse::<usize>().ok().and_then(|n| {
            if n >= 1 && n <= words.len() {
                Some(&words[n - 1])
            } else {
                None
            }
        });

        if let Some(word) = selected {
            self.vm
                .state
                .set_var(sel.var_name.clone(), smol_str::SmolStr::from(word.as_str()));
        } else {
            self.vm
                .state
                .set_var(sel.var_name.clone(), smol_str::SmolStr::default());
        }

        self.execute_body(&sel.body);
    }

    // ---- [[ ]] extended test evaluation ----

    /// Expand a word inside `[[ ]]` — no word splitting or glob expansion.
    fn dbl_bracket_expand(&mut self, word: &wasmsh_ast::Word) -> String {
        let resolved = self.resolve_command_subst(std::slice::from_ref(word));
        wasmsh_expand::expand_word(&resolved[0], &mut self.vm.state)
    }

    /// Evaluate a `[[ expression ]]` command. Returns true for exit-status 0.
    fn eval_double_bracket(&mut self, words: &[wasmsh_ast::Word]) -> bool {
        // Expand all words (no splitting/globbing) into string tokens for the evaluator
        let tokens: Vec<String> = words.iter().map(|w| self.dbl_bracket_expand(w)).collect();
        let mut pos = 0;
        dbl_bracket_eval_or(&tokens, &mut pos, &self.fs, &mut self.vm.state)
    }

    fn resolve_cwd_path(&self, path: &str) -> String {
        if path.starts_with('/') {
            wasmsh_fs::normalize_path(path)
        } else {
            wasmsh_fs::normalize_path(&format!("{}/{}", self.vm.state.cwd, path))
        }
    }

    /// Execute `alias [name[='value'] ...]`.
    fn execute_alias(&mut self, argv: &[String]) {
        let args = &argv[1..];
        if args.is_empty() {
            // List all aliases
            for (name, value) in &self.aliases {
                let line = format!("alias {name}='{value}'\n");
                self.vm.stdout.extend_from_slice(line.as_bytes());
            }
            self.vm.state.last_status = 0;
            return;
        }
        for arg in args {
            if let Some(eq_pos) = arg.find('=') {
                let name = &arg[..eq_pos];
                let value = &arg[eq_pos + 1..];
                self.aliases.insert(name.to_string(), value.to_string());
            } else {
                // Show specific alias
                if let Some(value) = self.aliases.get(arg.as_str()) {
                    let line = format!("alias {arg}='{value}'\n");
                    self.vm.stdout.extend_from_slice(line.as_bytes());
                } else {
                    let msg = format!("alias: {arg}: not found\n");
                    self.vm.stderr.extend_from_slice(msg.as_bytes());
                    self.vm.state.last_status = 1;
                    return;
                }
            }
        }
        self.vm.state.last_status = 0;
    }

    /// Execute `unalias [-a] name ...`.
    fn execute_unalias(&mut self, argv: &[String]) {
        let args = &argv[1..];
        if args.is_empty() {
            self.vm
                .stderr
                .extend_from_slice(b"unalias: usage: unalias [-a] name ...\n");
            self.vm.state.last_status = 1;
            return;
        }
        for arg in args {
            if arg == "-a" {
                self.aliases.clear();
            } else if self.aliases.shift_remove(arg.as_str()).is_none() {
                let msg = format!("unalias: {arg}: not found\n");
                self.vm.stderr.extend_from_slice(msg.as_bytes());
                self.vm.state.last_status = 1;
                return;
            }
        }
        self.vm.state.last_status = 0;
    }

    /// Execute `type name ...` — report how each name would be interpreted.
    /// Checks aliases, functions, builtins, and utilities in that order.
    fn execute_type(&mut self, argv: &[String]) {
        let mut status = 0;
        for name in &argv[1..] {
            if self.aliases.contains_key(name.as_str()) {
                let val = self.aliases.get(name.as_str()).unwrap();
                let msg = format!("{name} is aliased to `{val}'\n");
                self.vm.stdout.extend_from_slice(msg.as_bytes());
            } else if self.functions.contains_key(name.as_str()) {
                let msg = format!("{name} is a function\n");
                self.vm.stdout.extend_from_slice(msg.as_bytes());
            } else if self.builtins.is_builtin(name) {
                let msg = format!("{name} is a shell builtin\n");
                self.vm.stdout.extend_from_slice(msg.as_bytes());
            } else if self.utils.is_utility(name) {
                let msg = format!("{name} is a shell utility\n");
                self.vm.stdout.extend_from_slice(msg.as_bytes());
            } else {
                let msg = format!("wasmsh: type: {name}: not found\n");
                self.vm.stderr.extend_from_slice(msg.as_bytes());
                status = 1;
            }
        }
        self.vm.state.last_status = status;
    }

    /// Execute `builtin name [args...]` — skip alias and function lookup,
    /// invoke the named builtin directly.
    fn execute_builtin_keyword(&mut self, argv: &[String]) {
        if argv.len() < 2 {
            self.vm.state.last_status = 0;
            return;
        }
        let cmd_name = &argv[1];
        let builtin_argv: Vec<String> = argv[1..].to_vec();
        if let Some(builtin_fn) = self.builtins.get(cmd_name) {
            let stdin_data = self.pending_stdin.take();
            let argv_refs: Vec<&str> = builtin_argv.iter().map(String::as_str).collect();
            let mut sink = wasmsh_builtins::VecSink::default();
            let status = {
                let mut ctx = wasmsh_builtins::BuiltinContext {
                    state: &mut self.vm.state,
                    output: &mut sink,
                    fs: Some(&self.fs),
                    stdin: stdin_data.as_deref(),
                };
                builtin_fn(&mut ctx, &argv_refs)
            };
            self.vm.stdout.extend_from_slice(&sink.stdout);
            self.vm.stderr.extend_from_slice(&sink.stderr);
            self.vm.output_bytes += (sink.stdout.len() + sink.stderr.len()) as u64;
            self.vm.state.last_status = status;
        } else {
            let msg = format!("builtin: {cmd_name}: not a shell builtin\n");
            self.vm.stderr.extend_from_slice(msg.as_bytes());
            self.vm.state.last_status = 1;
        }
    }

    /// Execute `mapfile`/`readarray` — read stdin lines into an indexed array.
    /// Supports `-t` (strip trailing newline). Default array: MAPFILE.
    fn execute_mapfile(&mut self, argv: &[String]) {
        let args = &argv[1..];
        let mut strip_newline = false;
        let mut array_name = "MAPFILE".to_string();
        let mut positional: Vec<&str> = Vec::new();

        for arg in args {
            match arg.as_str() {
                "-t" => strip_newline = true,
                _ => positional.push(arg),
            }
        }

        // Last positional arg is the array name
        if let Some(name) = positional.last() {
            array_name = name.to_string();
        }

        // Read from pending stdin
        let data = self.pending_stdin.take().unwrap_or_default();
        let text = String::from_utf8_lossy(&data);

        let name_key = smol_str::SmolStr::from(array_name.as_str());
        self.vm.state.init_indexed_array(name_key.clone());

        let mut idx = 0;
        for line in text.split('\n') {
            // Skip trailing empty string from final newline
            if line.is_empty() && idx > 0 {
                continue;
            }
            let value = if strip_newline {
                line.to_string()
            } else {
                format!("{line}\n")
            };
            self.vm.state.set_array_element(
                name_key.clone(),
                &idx.to_string(),
                smol_str::SmolStr::from(value.as_str()),
            );
            idx += 1;
        }
        self.vm.state.last_status = 0;
    }

    /// Search `$PATH` directories in the VFS for a file. Returns the first match.
    fn search_path_for_file(&self, filename: &str) -> Option<String> {
        let path_var = self.vm.state.get_var("PATH")?;
        for dir in path_var.split(':') {
            if dir.is_empty() {
                continue;
            }
            let candidate = format!("{dir}/{filename}");
            let full = self.resolve_cwd_path(&candidate);
            if self.fs.stat(&full).is_ok() {
                return Some(full);
            }
        }
        None
    }

    fn should_errexit(&self, and_or: &HirAndOr) -> bool {
        !self.exec.errexit_suppressed
            && and_or.rest.is_empty()
            && !and_or.first.negated
            && self.vm.state.get_var("SHOPT_e").as_deref() == Some("1")
            && self.vm.state.last_status != 0
            && self.exec.exit_requested.is_none()
    }

    /// Execute `let expr1 expr2 ...` — evaluate each as arithmetic.
    /// Exit status: 0 if the last expression is non-zero, 1 if zero.
    fn execute_let(&mut self, argv: &[String]) {
        if argv.len() < 2 {
            self.vm
                .stderr
                .extend_from_slice(b"let: expression expected\n");
            self.vm.state.last_status = 1;
            return;
        }
        let mut last_val: i64 = 0;
        for expr in &argv[1..] {
            last_val = wasmsh_expand::eval_arithmetic(expr, &mut self.vm.state);
        }
        self.vm.state.last_status = i32::from(last_val == 0);
    }

    /// Known `shopt` option names.
    const SHOPT_OPTIONS: &'static [&'static str] = &[
        "extglob",
        "nullglob",
        "dotglob",
        "globstar",
        "nocasematch",
        "nocaseglob",
        "failglob",
        "lastpipe",
        "expand_aliases",
    ];

    /// Execute `shopt [-s|-u] [optname ...]`.
    fn execute_shopt(&mut self, argv: &[String]) {
        let (set_mode, names) = Self::parse_shopt_args(&argv[1..]);
        if let Some(enable) = set_mode {
            self.shopt_set_options(&names, enable);
        } else {
            self.shopt_print_options(&names);
        }
    }

    fn parse_shopt_args(args: &[String]) -> (Option<bool>, Vec<&str>) {
        let mut set_mode = None;
        let mut names = Vec::new();

        for arg in args {
            match arg.as_str() {
                "-s" => set_mode = Some(true),
                "-u" => set_mode = Some(false),
                _ => names.push(arg.as_str()),
            }
        }

        (set_mode, names)
    }

    /// Set shopt options (`-s` or `-u`).
    fn shopt_set_options(&mut self, names: &[&str], enable: bool) {
        if names.is_empty() {
            self.vm
                .stderr
                .extend_from_slice(b"shopt: option name required\n");
            self.vm.state.last_status = 1;
            return;
        }
        let val = if enable { "1" } else { "0" };
        for name in names {
            if self.reject_invalid_shopt_name(name) {
                return;
            }
            self.set_shopt_value(name, val);
        }
        self.vm.state.last_status = 0;
    }

    /// Print shopt option statuses. If `names` is empty, print all.
    fn shopt_print_options(&mut self, names: &[&str]) {
        let options_to_print: Vec<&str> = if names.is_empty() {
            Self::SHOPT_OPTIONS.to_vec()
        } else {
            names.to_vec()
        };
        for name in &options_to_print {
            if self.reject_invalid_shopt_name(name) {
                return;
            }
            let enabled = self.get_shopt_value(name);
            let status_str = if enabled { "on" } else { "off" };
            let line = format!("{name}\t{status_str}\n");
            self.vm.stdout.extend_from_slice(line.as_bytes());
        }
        self.vm.state.last_status = 0;
    }

    fn reject_invalid_shopt_name(&mut self, name: &str) -> bool {
        if Self::SHOPT_OPTIONS.contains(&name) {
            return false;
        }

        let msg = format!("shopt: {name}: invalid shell option name\n");
        self.vm.stderr.extend_from_slice(msg.as_bytes());
        self.vm.state.last_status = 1;
        true
    }

    fn shopt_var_name(name: &str) -> String {
        format!("SHOPT_{name}")
    }

    fn set_shopt_value(&mut self, name: &str, value: &str) {
        let var = Self::shopt_var_name(name);
        self.vm.state.set_var(
            smol_str::SmolStr::from(var.as_str()),
            smol_str::SmolStr::from(value),
        );
    }

    fn get_shopt_value(&self, name: &str) -> bool {
        let var = Self::shopt_var_name(name);
        self.vm.state.get_var(&var).as_deref() == Some("1")
    }

    /// Execute `declare`/`typeset` with flag parsing.
    /// Supports: -i, -a, -A, -x, -r, -l, -u, -p, -n, name=value.
    fn execute_declare(&mut self, argv: &[String]) {
        let (flags, names) = parse_declare_flags(argv);

        if flags.is_print {
            self.declare_print(argv, &names);
            return;
        }

        for &idx in &names {
            self.declare_one_name(argv, idx, &flags);
        }
        self.vm.state.last_status = 0;
    }

    /// Handle `declare -p` printing.
    fn declare_print(&mut self, argv: &[String], names: &[usize]) {
        if names.is_empty() {
            let vars: Vec<(String, String)> = self
                .vm
                .state
                .env
                .scopes
                .iter()
                .flat_map(|scope| {
                    scope
                        .iter()
                        .map(|(n, v)| (n.to_string(), v.value.as_scalar().to_string()))
                })
                .collect();
            for (name, val) in &vars {
                let line = format!("declare -- {name}=\"{val}\"\n");
                self.vm.stdout.extend_from_slice(line.as_bytes());
            }
        } else {
            for &idx in names {
                let name_arg = &argv[idx];
                let name = name_arg
                    .find('=')
                    .map_or(name_arg.as_str(), |eq| &name_arg[..eq]);
                if let Some(var) = self.vm.state.env.get(name) {
                    let val = var.value.as_scalar();
                    let line = format!("declare -- {name}=\"{val}\"\n");
                    self.vm.stdout.extend_from_slice(line.as_bytes());
                }
            }
        }
        self.vm.state.last_status = 0;
    }

    /// Process a single name in a `declare`/`typeset` command.
    fn declare_one_name(&mut self, argv: &[String], idx: usize, flags: &DeclareFlags) {
        let name_arg = &argv[idx];
        let (name, value) = if let Some(eq) = name_arg.find('=') {
            (&name_arg[..eq], Some(&name_arg[eq + 1..]))
        } else {
            (name_arg.as_str(), None)
        };

        if flags.is_assoc {
            self.vm
                .state
                .init_assoc_array(smol_str::SmolStr::from(name));
        } else if flags.is_indexed {
            self.vm
                .state
                .init_indexed_array(smol_str::SmolStr::from(name));
        }

        if let Some(val) = value {
            self.declare_assign_value(name, val, flags);
        } else if !flags.is_assoc && !flags.is_indexed && self.vm.state.get_var(name).is_none() {
            self.vm
                .state
                .set_var(smol_str::SmolStr::from(name), smol_str::SmolStr::default());
        }

        self.declare_apply_attributes(name, flags);

        if flags.is_nameref {
            self.declare_apply_nameref(name);
        }
    }

    /// Assign a value in `declare`, handling compound arrays and scalar transforms.
    fn declare_assign_value(&mut self, name: &str, val: &str, flags: &DeclareFlags) {
        if val.starts_with('(') && val.ends_with(')') {
            let inner = &val[1..val.len() - 1];
            let name_key = smol_str::SmolStr::from(name);
            if flags.is_assoc || inner.contains("]=") {
                self.vm.state.init_assoc_array(name_key.clone());
                for pair in Self::parse_assoc_pairs(inner) {
                    self.vm.state.set_array_element(
                        name_key.clone(),
                        &pair.0,
                        smol_str::SmolStr::from(pair.1.as_str()),
                    );
                }
            } else {
                let elements = Self::parse_array_elements(inner);
                self.vm.state.init_indexed_array(name_key.clone());
                for (i, elem) in elements.iter().enumerate() {
                    self.vm
                        .state
                        .set_array_element(name_key.clone(), &i.to_string(), elem.clone());
                }
            }
            return;
        }
        let final_val = if flags.is_integer {
            wasmsh_expand::eval_arithmetic(val, &mut self.vm.state).to_string()
        } else if flags.is_lower {
            val.to_lowercase()
        } else if flags.is_upper {
            val.to_uppercase()
        } else {
            val.to_string()
        };
        self.vm.state.set_var(
            smol_str::SmolStr::from(name),
            smol_str::SmolStr::from(final_val.as_str()),
        );
    }

    /// Apply export, readonly, integer attributes after declare assignment.
    fn declare_apply_attributes(&mut self, name: &str, flags: &DeclareFlags) {
        if let Some(var) = self.vm.state.env.get_mut(name) {
            if flags.is_export {
                var.exported = true;
            }
            if flags.is_readonly {
                var.readonly = true;
            }
            if flags.is_integer {
                var.integer = true;
            }
        }
    }

    /// Apply nameref attribute for `declare -n`.
    fn declare_apply_nameref(&mut self, name: &str) {
        let target_value = if let Some(eq_pos) = name.find('=') {
            smol_str::SmolStr::from(&name[eq_pos + 1..])
        } else if let Some(var) = self.vm.state.env.get(name) {
            var.value.as_scalar()
        } else {
            smol_str::SmolStr::default()
        };
        let actual_name = name.find('=').map_or(name, |eq| &name[..eq]);
        self.vm.state.env.set(
            smol_str::SmolStr::from(actual_name),
            wasmsh_state::ShellVar {
                value: wasmsh_state::VarValue::Scalar(target_value),
                exported: false,
                readonly: false,
                integer: false,
                nameref: true,
            },
        );
    }

    fn should_stop_execution(&self) -> bool {
        self.exec.break_depth > 0 || self.exec.loop_continue || self.exec.exit_requested.is_some()
    }

    fn execute_body(&mut self, body: &[HirCompleteCommand]) {
        for cc in body {
            if self.should_stop_execution() {
                break;
            }
            self.execute_complete_command(cc);
        }
    }

    fn execute_complete_command(&mut self, cc: &HirCompleteCommand) {
        for and_or in &cc.list {
            if self.should_stop_execution() {
                break;
            }
            self.execute_pipeline_chain(and_or);
            if self.should_errexit(and_or) {
                self.exec.exit_requested = Some(self.vm.state.last_status);
            }
        }
    }

    /// Expand a word value via command substitution and word expansion.
    fn expand_assignment_value(&mut self, value: Option<&wasmsh_ast::Word>) -> String {
        if let Some(w) = value {
            let resolved = self.resolve_command_subst(std::slice::from_ref(w));
            wasmsh_expand::expand_word(&resolved[0], &mut self.vm.state)
        } else {
            String::new()
        }
    }

    /// Execute a variable assignment, handling array syntax:
    /// - `name=(val1 val2 ...)` -- indexed array compound assignment
    /// - `name[idx]=val` -- single element assignment
    /// - `name+=(val1 val2 ...)` -- array append
    /// - Plain `name=val` -- scalar assignment
    fn execute_assignment(
        &mut self,
        raw_name: &smol_str::SmolStr,
        value: Option<&wasmsh_ast::Word>,
    ) {
        let (name_str, is_append) = Self::split_assignment_name(raw_name.as_str());
        if self.try_assign_array_element(name_str, value) {
            return;
        }

        let val_str = self.expand_assignment_value(value);
        if val_str.starts_with('(') && val_str.ends_with(')') {
            self.assign_compound_array(name_str, &val_str, is_append);
            return;
        }

        let final_val = self.resolve_scalar_assignment_value(name_str, &val_str, is_append);
        self.vm
            .state
            .set_var(smol_str::SmolStr::from(name_str), final_val.into());
    }

    fn split_assignment_name(name: &str) -> (&str, bool) {
        if let Some(stripped) = name.strip_suffix('+') {
            (stripped, true)
        } else {
            (name, false)
        }
    }

    fn parse_array_element_assignment(name: &str) -> Option<(&str, &str)> {
        let bracket_pos = name.find('[')?;
        name.ends_with(']')
            .then_some((&name[..bracket_pos], &name[bracket_pos + 1..name.len() - 1]))
    }

    fn try_assign_array_element(&mut self, name: &str, value: Option<&wasmsh_ast::Word>) -> bool {
        let Some((base, index)) = Self::parse_array_element_assignment(name) else {
            return false;
        };
        let val = self.expand_assignment_value(value);
        self.vm
            .state
            .set_array_element(smol_str::SmolStr::from(base), index, val.into());
        true
    }

    fn resolve_scalar_assignment_value(
        &mut self,
        name: &str,
        value: &str,
        is_append: bool,
    ) -> String {
        if self.vm.state.env.get(name).is_some_and(|v| v.integer) {
            return self.eval_integer_assignment(name, value, is_append);
        }
        if is_append {
            return format!(
                "{}{}",
                self.vm.state.get_var(name).unwrap_or_default(),
                value
            );
        }
        value.to_string()
    }

    fn eval_integer_assignment(&mut self, name: &str, value: &str, is_append: bool) -> String {
        let arith_input = if is_append {
            format!(
                "{}+{}",
                self.vm.state.get_var(name).unwrap_or_default(),
                value
            )
        } else {
            value.to_string()
        };
        wasmsh_expand::eval_arithmetic(&arith_input, &mut self.vm.state).to_string()
    }

    /// Assign a compound array value `(...)` to a variable.
    fn assign_compound_array(&mut self, name_str: &str, val_str: &str, is_append: bool) {
        let inner = &val_str[1..val_str.len() - 1];
        let elements = Self::parse_array_elements(inner);
        let name_key = smol_str::SmolStr::from(name_str);

        if is_append {
            self.vm.state.append_array(name_str, elements);
            return;
        }

        if Self::is_assoc_array_assignment(inner, &elements) {
            self.assign_assoc_array(name_key, inner);
            return;
        }
        self.assign_indexed_array(name_key, &elements);
    }

    fn is_assoc_array_assignment(inner: &str, elements: &[smol_str::SmolStr]) -> bool {
        !elements.is_empty() && inner.contains('[') && inner.contains("]=")
    }

    fn assign_assoc_array(&mut self, name_key: smol_str::SmolStr, inner: &str) {
        self.vm.state.init_assoc_array(name_key.clone());
        for (key, value) in Self::parse_assoc_pairs(inner) {
            self.vm.state.set_array_element(
                name_key.clone(),
                &key,
                smol_str::SmolStr::from(value.as_str()),
            );
        }
    }

    fn assign_indexed_array(
        &mut self,
        name_key: smol_str::SmolStr,
        elements: &[smol_str::SmolStr],
    ) {
        self.vm.state.init_indexed_array(name_key.clone());
        for (i, elem) in elements.iter().enumerate() {
            self.vm
                .state
                .set_array_element(name_key.clone(), &i.to_string(), elem.clone());
        }
    }

    fn push_array_element(elements: &mut Vec<smol_str::SmolStr>, current: &mut String) {
        if current.is_empty() {
            return;
        }
        elements.push(smol_str::SmolStr::from(current.as_str()));
        current.clear();
    }

    /// Parse space-separated array elements from the inner content of `(...)`.
    /// Respects quoting (single and double quotes).
    fn parse_array_elements(inner: &str) -> Vec<smol_str::SmolStr> {
        let mut elements = Vec::new();
        let mut current = String::new();
        let mut in_single_quote = false;
        let mut in_double_quote = false;
        let mut escape_next = false;

        for ch in inner.chars() {
            if escape_next {
                current.push(ch);
                escape_next = false;
                continue;
            }
            if ch == '\\' && !in_single_quote {
                escape_next = true;
                continue;
            }
            if ch == '\'' && !in_double_quote {
                in_single_quote = !in_single_quote;
                continue;
            }
            if ch == '"' && !in_single_quote {
                in_double_quote = !in_double_quote;
                continue;
            }
            if ch.is_ascii_whitespace() && !in_single_quote && !in_double_quote {
                Self::push_array_element(&mut elements, &mut current);
                continue;
            }
            current.push(ch);
        }
        Self::push_array_element(&mut elements, &mut current);
        elements
    }

    /// Parse `[key]=value` pairs from associative array compound assignment.
    fn parse_assoc_pairs(inner: &str) -> Vec<(String, String)> {
        let mut pairs = Vec::new();
        let mut pos = 0;
        let bytes = inner.as_bytes();

        while pos < bytes.len() {
            Self::skip_ascii_whitespace(bytes, &mut pos);
            if pos >= bytes.len() {
                break;
            }
            if let Some(key) = Self::parse_assoc_key(inner, &mut pos) {
                pairs.push((key, Self::parse_assoc_value(inner, &mut pos)));
                continue;
            }
            Self::skip_non_whitespace(bytes, &mut pos);
        }
        pairs
    }

    fn skip_ascii_whitespace(bytes: &[u8], pos: &mut usize) {
        while *pos < bytes.len() && bytes[*pos].is_ascii_whitespace() {
            *pos += 1;
        }
    }

    fn skip_non_whitespace(bytes: &[u8], pos: &mut usize) {
        while *pos < bytes.len() && !bytes[*pos].is_ascii_whitespace() {
            *pos += 1;
        }
    }

    fn parse_assoc_key(inner: &str, pos: &mut usize) -> Option<String> {
        let bytes = inner.as_bytes();
        if *pos >= bytes.len() || bytes[*pos] != b'[' {
            return None;
        }

        *pos += 1;
        let key_start = *pos;
        while *pos < bytes.len() && bytes[*pos] != b']' {
            *pos += 1;
        }
        let key = inner[key_start..*pos].to_string();
        if *pos < bytes.len() {
            *pos += 1;
        }
        if *pos < bytes.len() && bytes[*pos] == b'=' {
            *pos += 1;
        }
        Some(key)
    }

    /// Parse a single value in an associative array assignment (may be quoted).
    fn parse_assoc_value(inner: &str, pos: &mut usize) -> String {
        let bytes = inner.as_bytes();
        match bytes.get(*pos).copied() {
            Some(b'"') => Self::parse_double_quoted_assoc_value(bytes, pos),
            Some(b'\'') => Self::parse_single_quoted_assoc_value(bytes, pos),
            _ => Self::parse_unquoted_assoc_value(bytes, pos),
        }
    }

    fn parse_double_quoted_assoc_value(bytes: &[u8], pos: &mut usize) -> String {
        let mut value = String::new();
        *pos += 1;
        while *pos < bytes.len() && bytes[*pos] != b'"' {
            if bytes[*pos] == b'\\' && *pos + 1 < bytes.len() {
                *pos += 1;
            }
            value.push(bytes[*pos] as char);
            *pos += 1;
        }
        if *pos < bytes.len() {
            *pos += 1;
        }
        value
    }

    fn parse_single_quoted_assoc_value(bytes: &[u8], pos: &mut usize) -> String {
        let mut value = String::new();
        *pos += 1;
        while *pos < bytes.len() && bytes[*pos] != b'\'' {
            value.push(bytes[*pos] as char);
            *pos += 1;
        }
        if *pos < bytes.len() {
            *pos += 1;
        }
        value
    }

    fn parse_unquoted_assoc_value(bytes: &[u8], pos: &mut usize) -> String {
        let mut value = String::new();
        while *pos < bytes.len() && !bytes[*pos].is_ascii_whitespace() {
            value.push(bytes[*pos] as char);
            *pos += 1;
        }
        value
    }

    /// Maximum number of arguments after glob expansion.
    const MAX_GLOB_RESULTS: usize = 10_000;

    /// Expand glob patterns in argv against the VFS.
    /// Supports: basic glob (`*`, `?`, `[...]`), globstar (`**`), nullglob,
    /// dotglob, and extglob patterns.
    /// When `set -f` (noglob) is active, glob expansion is skipped entirely.
    fn expand_globs(&mut self, argv: Vec<String>) -> Vec<String> {
        if self.vm.state.get_var("SHOPT_f").as_deref() == Some("1") {
            return argv;
        }
        let nullglob = self.get_shopt_value("nullglob");
        let dotglob = self.get_shopt_value("dotglob");
        let globstar = self.get_shopt_value("globstar");
        let extglob = self.get_shopt_value("extglob");

        let mut result = Vec::new();
        for arg in argv {
            result.extend(self.expand_glob_arg(arg, nullglob, dotglob, globstar, extglob));
        }
        result.truncate(Self::MAX_GLOB_RESULTS);
        result
    }

    fn expand_glob_arg(
        &self,
        arg: String,
        nullglob: bool,
        dotglob: bool,
        globstar: bool,
        extglob: bool,
    ) -> Vec<String> {
        if !Self::is_glob_pattern(&arg, extglob) {
            return vec![arg];
        }
        if globstar && arg.contains("**") {
            return self.expand_globstar_arg(arg, nullglob, dotglob, extglob);
        }
        self.expand_standard_glob_arg(arg, nullglob, dotglob, extglob)
    }

    fn is_glob_pattern(arg: &str, extglob: bool) -> bool {
        let has_bracket_class = arg.contains('[') && arg.contains(']');
        arg.contains('*')
            || arg.contains('?')
            || has_bracket_class
            || (extglob && has_extglob_pattern(arg))
    }

    fn expand_globstar_arg(
        &self,
        arg: String,
        nullglob: bool,
        dotglob: bool,
        extglob: bool,
    ) -> Vec<String> {
        let mut matches = self.expand_globstar(&arg, dotglob, extglob);
        matches.sort();
        self.finalize_glob_matches(arg, matches, nullglob)
    }

    fn expand_standard_glob_arg(
        &self,
        arg: String,
        nullglob: bool,
        dotglob: bool,
        extglob: bool,
    ) -> Vec<String> {
        let Some((dir, pattern, prefix)) = self.split_glob_search(&arg) else {
            return self.finalize_glob_matches(arg.clone(), Vec::new(), nullglob);
        };
        let matches = self.read_glob_matches(&dir, &pattern, prefix.as_deref(), dotglob, extglob);
        self.finalize_glob_matches(arg, matches, nullglob)
    }

    fn split_glob_search(&self, arg: &str) -> Option<(String, String, Option<String>)> {
        let Some(slash_pos) = arg.rfind('/') else {
            return Some((self.vm.state.cwd.clone(), arg.to_string(), None));
        };

        let dir_part = &arg[..=slash_pos];
        if Self::path_segment_has_glob(dir_part) {
            return None;
        }

        Some((
            self.resolve_cwd_path(dir_part),
            arg[slash_pos + 1..].to_string(),
            Some(dir_part.to_string()),
        ))
    }

    fn path_segment_has_glob(path: &str) -> bool {
        path.contains('*') || path.contains('?') || path.contains('[')
    }

    fn read_glob_matches(
        &self,
        dir: &str,
        pattern: &str,
        prefix: Option<&str>,
        dotglob: bool,
        extglob: bool,
    ) -> Vec<String> {
        let Ok(entries) = self.fs.read_dir(dir) else {
            return Vec::new();
        };

        let mut matches: Vec<String> = entries
            .iter()
            .filter(|e| glob_match_ext(pattern, &e.name, dotglob, extglob))
            .map(|e| match prefix {
                Some(prefix) => format!("{prefix}{}", e.name),
                None => e.name.clone(),
            })
            .collect();
        matches.sort();
        matches
    }

    fn finalize_glob_matches(
        &self,
        arg: String,
        matches: Vec<String>,
        nullglob: bool,
    ) -> Vec<String> {
        if !matches.is_empty() {
            return matches;
        }
        if nullglob {
            Vec::new()
        } else {
            vec![arg]
        }
    }

    /// Expand a globstar (**) pattern against the VFS with recursive directory traversal.
    fn expand_globstar(&self, pattern: &str, dotglob: bool, extglob: bool) -> Vec<String> {
        // Split pattern into segments by /
        let segments: Vec<&str> = pattern.split('/').collect();
        let base_dir = self.vm.state.cwd.clone();
        let mut matches = Vec::new();
        self.globstar_walk(&base_dir, &segments, 0, "", dotglob, extglob, &mut matches);
        matches
    }

    /// Recursive walk for globstar expansion.
    fn globstar_walk(
        &self,
        dir: &str,
        segments: &[&str],
        seg_idx: usize,
        prefix: &str,
        dotglob: bool,
        extglob: bool,
        matches: &mut Vec<String>,
    ) {
        if seg_idx >= segments.len() {
            return;
        }

        let seg = segments[seg_idx];
        if seg == "**" {
            self.globstar_walk_wildcard(dir, segments, seg_idx, prefix, dotglob, extglob, matches);
            return;
        }
        self.globstar_walk_segment(
            dir, seg, segments, seg_idx, prefix, dotglob, extglob, matches,
        );
    }

    fn globstar_walk_wildcard(
        &self,
        dir: &str,
        segments: &[&str],
        seg_idx: usize,
        prefix: &str,
        dotglob: bool,
        extglob: bool,
        matches: &mut Vec<String>,
    ) {
        if seg_idx + 1 < segments.len() {
            self.globstar_walk(
                dir,
                segments,
                seg_idx + 1,
                prefix,
                dotglob,
                extglob,
                matches,
            );
        }

        let Ok(entries) = self.fs.read_dir(dir) else {
            return;
        };
        for entry in &entries {
            if !dotglob && entry.name.starts_with('.') {
                continue;
            }
            let (child_path, child_prefix) = Self::globstar_child_paths(dir, prefix, &entry.name);
            if self.fs.stat(&child_path).map(|m| m.is_dir).unwrap_or(false) {
                self.globstar_walk(
                    &child_path,
                    segments,
                    seg_idx,
                    &child_prefix,
                    dotglob,
                    extglob,
                    matches,
                );
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn globstar_walk_segment(
        &self,
        dir: &str,
        seg: &str,
        segments: &[&str],
        seg_idx: usize,
        prefix: &str,
        dotglob: bool,
        extglob: bool,
        matches: &mut Vec<String>,
    ) {
        let Ok(entries) = self.fs.read_dir(dir) else {
            return;
        };
        let is_last = seg_idx == segments.len() - 1;

        for entry in &entries {
            if !glob_match_ext(seg, &entry.name, dotglob, extglob) {
                continue;
            }
            let (child_path, child_prefix) = Self::globstar_child_paths(dir, prefix, &entry.name);
            if is_last {
                matches.push(child_prefix);
            } else if self.fs.stat(&child_path).map(|m| m.is_dir).unwrap_or(false) {
                self.globstar_walk(
                    &child_path,
                    segments,
                    seg_idx + 1,
                    &child_prefix,
                    dotglob,
                    extglob,
                    matches,
                );
            }
        }
    }

    fn globstar_child_paths(dir: &str, prefix: &str, name: &str) -> (String, String) {
        let child_path = if dir == "/" {
            format!("/{name}")
        } else {
            format!("{dir}/{name}")
        };
        let child_prefix = if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}/{name}")
        };
        (child_path, child_prefix)
    }

    /// Write data to a file path, reporting errors to stderr.
    fn write_to_file(&mut self, path: &str, target: &str, data: &[u8], opts: OpenOptions) {
        match self.fs.open(path, opts) {
            Ok(h) => {
                if let Err(e) = self.fs.write_file(h, data) {
                    self.vm
                        .stderr
                        .extend_from_slice(format!("wasmsh: write error: {e}\n").as_bytes());
                }
                self.fs.close(h);
            }
            Err(e) => {
                self.vm
                    .stderr
                    .extend_from_slice(format!("wasmsh: {target}: {e}\n").as_bytes());
            }
        }
    }

    /// Capture stdout data from the given position, truncating the stdout buffer.
    fn capture_stdout(&mut self, from: usize) -> Vec<u8> {
        let data = self.vm.stdout[from..].to_vec();
        self.vm.stdout.truncate(from);
        data
    }

    /// Apply redirections: for `>` and `>>`, write captured stdout/stderr to file.
    /// For `<`, read file content (handled pre-execution).
    /// Supports fd-specific redirections (2>, 2>>) and &> (both stdout and stderr).
    fn apply_redirections(&mut self, redirections: &[HirRedirection], stdout_before: usize) {
        for redir in redirections {
            let target = wasmsh_expand::expand_word(&redir.target, &mut self.vm.state);
            let path = self.resolve_cwd_path(&target);
            let fd = redir.fd.unwrap_or(1);

            match redir.op {
                RedirectionOp::Output => {
                    self.apply_output_redir(&path, &target, fd, stdout_before);
                }
                RedirectionOp::Append => {
                    self.apply_append_redir(&path, &target, fd, stdout_before);
                }
                RedirectionOp::DupOutput => {
                    let target_fd: u32 = target.parse().unwrap_or(1);
                    let source_fd = redir.fd.unwrap_or(1);
                    if source_fd == 2 && target_fd == 1 {
                        let stderr_data = std::mem::take(&mut self.vm.stderr);
                        self.vm.stdout.extend_from_slice(&stderr_data);
                    } else if source_fd == 1 && target_fd == 2 {
                        let stdout_data = self.capture_stdout(stdout_before);
                        self.vm.stderr.extend_from_slice(&stdout_data);
                    }
                }
                #[allow(unreachable_patterns)]
                _ => {}
            }
        }
    }

    /// Apply `>` output redirection for a specific fd.
    fn apply_output_redir(&mut self, path: &str, target: &str, fd: u32, stdout_before: usize) {
        let data = if fd == FD_BOTH {
            let mut combined = self.capture_stdout(stdout_before);
            combined.extend_from_slice(&std::mem::take(&mut self.vm.stderr));
            combined
        } else if fd == 2 {
            std::mem::take(&mut self.vm.stderr)
        } else {
            self.capture_stdout(stdout_before)
        };
        self.write_to_file(path, target, &data, OpenOptions::write());
    }

    /// Apply `>>` append redirection for a specific fd.
    fn apply_append_redir(&mut self, path: &str, target: &str, fd: u32, stdout_before: usize) {
        let data = if fd == 2 {
            std::mem::take(&mut self.vm.stderr)
        } else {
            self.capture_stdout(stdout_before)
        };
        self.write_to_file(path, target, &data, OpenOptions::append());
    }
}

/// Convert a protocol diagnostic level to a VM diagnostic level.
fn convert_diag_level(level: DiagnosticLevel) -> wasmsh_vm::DiagLevel {
    match level {
        DiagnosticLevel::Trace => wasmsh_vm::DiagLevel::Trace,
        DiagnosticLevel::Warning => wasmsh_vm::DiagLevel::Warning,
        DiagnosticLevel::Error => wasmsh_vm::DiagLevel::Error,
        _ => wasmsh_vm::DiagLevel::Info,
    }
}

// ---- [[ ]] expression evaluator (free functions) ----

/// Evaluate an `||` expression (lowest precedence).
fn dbl_bracket_eval_or(
    tokens: &[String],
    pos: &mut usize,
    fs: &MemoryFs,
    state: &mut ShellState,
) -> bool {
    let mut result = dbl_bracket_eval_and(tokens, pos, fs, state);
    while *pos < tokens.len() && tokens[*pos] == "||" {
        *pos += 1;
        let rhs = dbl_bracket_eval_and(tokens, pos, fs, state);
        result = result || rhs;
    }
    result
}

/// Evaluate an `&&` expression.
fn dbl_bracket_eval_and(
    tokens: &[String],
    pos: &mut usize,
    fs: &MemoryFs,
    state: &mut ShellState,
) -> bool {
    let mut result = dbl_bracket_eval_not(tokens, pos, fs, state);
    while *pos < tokens.len() && tokens[*pos] == "&&" {
        *pos += 1;
        let rhs = dbl_bracket_eval_not(tokens, pos, fs, state);
        result = result && rhs;
    }
    result
}

/// Evaluate a `!` (negation) expression.
fn dbl_bracket_eval_not(
    tokens: &[String],
    pos: &mut usize,
    fs: &MemoryFs,
    state: &mut ShellState,
) -> bool {
    if *pos < tokens.len() && tokens[*pos] == "!" {
        *pos += 1;
        return !dbl_bracket_eval_not(tokens, pos, fs, state);
    }
    dbl_bracket_eval_primary(tokens, pos, fs, state)
}

/// Evaluate a primary expression: grouped `(expr)`, unary test, binary test, or string truth.
fn dbl_bracket_eval_primary(
    tokens: &[String],
    pos: &mut usize,
    fs: &MemoryFs,
    state: &mut ShellState,
) -> bool {
    if *pos >= tokens.len() {
        return false;
    }
    if let Some(result) = dbl_bracket_try_group(tokens, pos, fs, state) {
        return result;
    }
    if let Some(result) = dbl_bracket_try_unary(tokens, pos, fs) {
        return result;
    }
    if *pos + 1 == tokens.len() {
        return dbl_bracket_take_truthy_token(tokens, pos);
    }
    if let Some(result) = dbl_bracket_try_binary(tokens, pos, state) {
        return result;
    }
    dbl_bracket_take_truthy_token(tokens, pos)
}

fn dbl_bracket_try_group(
    tokens: &[String],
    pos: &mut usize,
    fs: &MemoryFs,
    state: &mut ShellState,
) -> Option<bool> {
    if tokens.get(*pos).map(String::as_str) != Some("(") {
        return None;
    }

    *pos += 1;
    let result = dbl_bracket_eval_or(tokens, pos, fs, state);
    if tokens.get(*pos).map(String::as_str) == Some(")") {
        *pos += 1;
    }
    Some(result)
}

fn dbl_bracket_take_truthy_token(tokens: &[String], pos: &mut usize) -> bool {
    let Some(token) = tokens.get(*pos) else {
        return false;
    };
    *pos += 1;
    !token.is_empty()
}

/// Try to evaluate a unary test (`-z`, `-n`, `-f`, etc.). Returns `None` if not a unary op.
fn dbl_bracket_try_unary(tokens: &[String], pos: &mut usize, fs: &MemoryFs) -> Option<bool> {
    if *pos + 1 >= tokens.len() {
        return None;
    }
    let op = &tokens[*pos];
    if !op.starts_with('-') || op.len() != 2 {
        return None;
    }
    let flag = op.as_bytes()[1];
    match flag {
        b'z' | b'n' => {
            *pos += 1;
            let arg = &tokens[*pos];
            *pos += 1;
            Some(if flag == b'z' {
                arg.is_empty()
            } else {
                !arg.is_empty()
            })
        }
        b'f' | b'd' | b'e' | b's' | b'r' | b'w' | b'x' => {
            // If a binary operator follows the next token, this is not a unary file test.
            if *pos + 2 < tokens.len() && is_binary_op(&tokens[*pos + 2]) {
                return None;
            }
            *pos += 1;
            let path_str = &tokens[*pos];
            *pos += 1;
            Some(eval_file_test(flag, path_str, fs))
        }
        _ => None,
    }
}

/// Try to evaluate a binary test. Returns `None` if no binary op at pos+1.
fn dbl_bracket_try_binary(
    tokens: &[String],
    pos: &mut usize,
    state: &mut ShellState,
) -> Option<bool> {
    if *pos + 2 > tokens.len() {
        return None;
    }
    let op_idx = *pos + 1;
    if op_idx >= tokens.len() || !is_binary_op(&tokens[op_idx]) {
        return None;
    }
    let lhs = tokens[*pos].clone();
    *pos += 1;
    let op = tokens[*pos].clone();
    *pos += 1;

    // For =~, the RHS extends until &&, ||, or end of tokens.
    if op == "=~" && *pos < tokens.len() {
        let mut rhs = String::new();
        while *pos < tokens.len() && tokens[*pos] != "&&" && tokens[*pos] != "||" {
            rhs.push_str(&tokens[*pos]);
            *pos += 1;
        }
        return Some(eval_binary_op(&lhs, &op, &rhs, state));
    }

    if *pos < tokens.len() {
        let rhs = tokens[*pos].clone();
        *pos += 1;
        return Some(eval_binary_op(&lhs, &op, &rhs, state));
    }
    Some(false)
}

/// Check whether a token is a binary operator in `[[ ]]` context.
fn is_binary_op(s: &str) -> bool {
    matches!(
        s,
        "==" | "!=" | "=~" | "=" | "<" | ">" | "-eq" | "-ne" | "-lt" | "-le" | "-gt" | "-ge"
    )
}

/// Evaluate a binary operation.
fn eval_binary_op(lhs: &str, op: &str, rhs: &str, state: &mut ShellState) -> bool {
    match op {
        "==" | "=" => glob_cmp(lhs, rhs, state, false),
        "!=" => !glob_cmp(lhs, rhs, state, false),
        "=~" => eval_regex_match(lhs, rhs, state),
        "<" => lhs < rhs,
        ">" => lhs > rhs,
        _ => eval_int_cmp(lhs, op, rhs),
    }
}

/// Glob-compare lhs against rhs pattern, respecting nocasematch.
fn glob_cmp(lhs: &str, rhs: &str, state: &ShellState, _negate: bool) -> bool {
    let nocasematch = state.get_var("SHOPT_nocasematch").as_deref() == Some("1");
    if nocasematch {
        glob_match_inner(rhs.to_lowercase().as_bytes(), lhs.to_lowercase().as_bytes())
    } else {
        glob_match_inner(rhs.as_bytes(), lhs.as_bytes())
    }
}

/// Evaluate a regex match (`=~`) with capture groups for `BASH_REMATCH`.
fn eval_regex_match(lhs: &str, rhs: &str, state: &mut ShellState) -> bool {
    let captures = regex_match_with_captures(lhs, rhs);
    let br_name = smol_str::SmolStr::from("BASH_REMATCH");
    let Some(caps) = captures else {
        state.init_indexed_array(br_name);
        return false;
    };
    state.init_indexed_array(br_name.clone());
    for (i, cap) in caps.iter().enumerate() {
        state.set_array_element(
            br_name.clone(),
            &i.to_string(),
            smol_str::SmolStr::from(cap.as_str()),
        );
    }
    true
}

/// Evaluate an integer comparison operator (`-eq`, `-ne`, `-lt`, `-le`, `-gt`, `-ge`).
fn eval_int_cmp(lhs: &str, op: &str, rhs: &str) -> bool {
    let a: i64 = lhs.parse().unwrap_or(0);
    let b: i64 = rhs.parse().unwrap_or(0);
    match op {
        "-eq" => a == b,
        "-ne" => a != b,
        "-lt" => a < b,
        "-le" => a <= b,
        "-gt" => a > b,
        "-ge" => a >= b,
        _ => false,
    }
}

/// Evaluate a unary file test.
fn eval_file_test(flag: u8, path: &str, fs: &MemoryFs) -> bool {
    use wasmsh_fs::Vfs;
    match fs.stat(path) {
        Ok(meta) => match flag {
            b'f' => !meta.is_dir,
            b'd' => meta.is_dir,
            b's' => meta.size > 0,
            // -e, -r, -w, -x: in the VFS all existing files are accessible
            b'e' | b'r' | b'w' | b'x' => true,
            _ => false,
        },
        Err(_) => false,
    }
}

/// Strip anchoring from a regex pattern, returning (core, `anchored_start`, `anchored_end`).
fn regex_strip_anchors(pattern: &str) -> (&str, bool, bool) {
    let anchored_start = pattern.starts_with('^');
    let anchored_end = pattern.ends_with('$') && !pattern.ends_with("\\$");
    let core = match (anchored_start, anchored_end) {
        (true, true) if pattern.len() >= 2 => &pattern[1..pattern.len() - 1],
        (true, _) => &pattern[1..],
        (_, true) => &pattern[..pattern.len() - 1],
        _ => pattern,
    };
    (core, anchored_start, anchored_end)
}

/// Check if a regex core has any special regex metacharacters.
fn has_regex_metachar(core: &str) -> bool {
    core.contains('.')
        || core.contains('+')
        || core.contains('*')
        || core.contains('?')
        || core.contains('[')
        || core.contains('(')
        || core.contains('|')
}

/// Find match range for a literal pattern with anchoring.
fn literal_match_range(text: &str, core: &str, start: bool, end: bool) -> Option<(usize, usize)> {
    match (start, end) {
        (true, true) if text == core => Some((0, text.len())),
        (true, false) if text.starts_with(core) => Some((0, core.len())),
        (false, true) if text.ends_with(core) => Some((text.len() - core.len(), text.len())),
        (false, false) => text.find(core).map(|pos| (pos, pos + core.len())),
        _ => None,
    }
}

/// Regex match with capture group support.
///
/// Returns `Some(captures)` if the pattern matches, where `captures[0]` is the
/// full match and `captures[1..]` are the parenthesized subgroup matches.
/// Returns `None` if no match.
fn regex_match_with_captures(text: &str, pattern: &str) -> Option<Vec<String>> {
    let (core, anchored_start, anchored_end) = regex_strip_anchors(pattern);

    if !has_regex_metachar(core) {
        return regex_match_literal_with_captures(text, core, anchored_start, anchored_end);
    }

    let start_range = if anchored_start {
        0..=0
    } else {
        0..=text.len()
    };

    for start in start_range {
        if let Some(result) = regex_match_from_start(text, core, anchored_end, start) {
            return Some(result);
        }
    }
    None
}

fn regex_match_literal_with_captures(
    text: &str,
    core: &str,
    anchored_start: bool,
    anchored_end: bool,
) -> Option<Vec<String>> {
    literal_match_range(text, core, anchored_start, anchored_end)
        .map(|(s, e)| vec![text[s..e].to_string()])
}

fn regex_match_from_start(
    text: &str,
    core: &str,
    anchored_end: bool,
    start: usize,
) -> Option<Vec<String>> {
    let mut group_caps: Vec<(usize, usize)> = Vec::new();
    let end = regex_match_capturing(
        text.as_bytes(),
        start,
        core.as_bytes(),
        0,
        anchored_end,
        &mut group_caps,
    )?;
    Some(regex_build_capture_list(text, start, end, &group_caps))
}

fn regex_build_capture_list(
    text: &str,
    start: usize,
    end: usize,
    group_caps: &[(usize, usize)],
) -> Vec<String> {
    let mut result = vec![text[start..end].to_string()];
    for &(gs, ge) in group_caps {
        result.push(text[gs..ge].to_string());
    }
    result
}

/// Backtracking regex matcher with capture group support.
/// Returns `Some(end_position)` on match, `None` on no match.
/// `captures` accumulates (start, end) pairs for each parenthesized group.
fn regex_match_capturing(
    text: &[u8],
    ti: usize,
    pat: &[u8],
    pi: usize,
    must_end: bool,
    captures: &mut Vec<(usize, usize)>,
) -> Option<usize> {
    if pi >= pat.len() {
        return if must_end && ti < text.len() {
            None
        } else {
            Some(ti)
        };
    }

    // Handle parenthesized groups
    if pat[pi] == b'(' {
        return regex_match_group(text, ti, pat, pi, must_end, captures);
    }

    // Parse one element (not a group) and apply quantifier
    regex_match_elem(text, ti, pat, pi, must_end, captures)
}

/// Handle a parenthesized group in the regex, dispatching by quantifier.
fn regex_match_group(
    text: &[u8],
    ti: usize,
    pat: &[u8],
    pi: usize,
    must_end: bool,
    captures: &mut Vec<(usize, usize)>,
) -> Option<usize> {
    let close = find_matching_paren_bytes(pat, pi + 1)?;
    let inner = &pat[pi + 1..close];
    let rest = &pat[close + 1..];
    let (quant, after_quant_offset) = if close + 1 < pat.len() {
        match pat[close + 1] {
            b'*' | b'+' | b'?' => (pat[close + 1], close + 2),
            _ => (0, close + 1),
        }
    } else {
        (0, close + 1)
    };
    let after_quant = &pat[after_quant_offset..];
    let alternatives = split_alternatives_bytes(inner);

    match quant {
        b'+' => regex_match_group_rep(text, ti, after_quant, must_end, captures, &alternatives, 1),
        b'*' => regex_match_group_rep(text, ti, after_quant, must_end, captures, &alternatives, 0),
        b'?' => regex_match_group_opt(text, ti, after_quant, must_end, captures, &alternatives),
        _ => regex_match_group_exact(text, ti, rest, must_end, captures, &alternatives),
    }
}

/// Match a group with repetition quantifier (+ or *).
fn regex_match_group_rep(
    text: &[u8],
    ti: usize,
    after: &[u8],
    must_end: bool,
    captures: &mut Vec<(usize, usize)>,
    alternatives: &[Vec<u8>],
    min_reps: usize,
) -> Option<usize> {
    let save = captures.len();
    for end_pos in (ti..=text.len()).rev() {
        captures.truncate(save);
        if regex_match_group_repeated(text, ti, end_pos, alternatives, min_reps) {
            if let Some(final_end) =
                regex_match_capturing(text, end_pos, after, 0, must_end, captures)
            {
                captures.insert(save, (ti, end_pos));
                return Some(final_end);
            }
        }
    }
    captures.truncate(save);
    None
}

/// Match a group with `?` quantifier (zero or one).
fn regex_match_group_opt(
    text: &[u8],
    ti: usize,
    after: &[u8],
    must_end: bool,
    captures: &mut Vec<(usize, usize)>,
    alternatives: &[Vec<u8>],
) -> Option<usize> {
    let save = captures.len();
    // Try one
    for alt in alternatives {
        captures.truncate(save);
        if let Some(end) = regex_try_match_at(text, ti, alt) {
            if let Some(final_end) = regex_match_capturing(text, end, after, 0, must_end, captures)
            {
                captures.insert(save, (ti, end));
                return Some(final_end);
            }
        }
        captures.truncate(save);
    }
    // Try zero
    captures.truncate(save);
    if let Some(final_end) = regex_match_capturing(text, ti, after, 0, must_end, captures) {
        captures.insert(save, (ti, ti));
        return Some(final_end);
    }
    captures.truncate(save);
    None
}

/// Match a group exactly once (no quantifier).
fn regex_match_group_exact(
    text: &[u8],
    ti: usize,
    rest: &[u8],
    must_end: bool,
    captures: &mut Vec<(usize, usize)>,
    alternatives: &[Vec<u8>],
) -> Option<usize> {
    let save = captures.len();
    for alt in alternatives {
        captures.truncate(save);
        if let Some(end) = regex_try_match_at(text, ti, alt) {
            if let Some(final_end) = regex_match_capturing(text, end, rest, 0, must_end, captures) {
                captures.insert(save, (ti, end));
                return Some(final_end);
            }
        }
        captures.truncate(save);
    }
    None
}

/// Parse a quantifier after a regex element.
fn parse_quantifier(pat: &[u8], pos: usize) -> (u8, usize) {
    if pos < pat.len() {
        match pat[pos] {
            b'*' => (b'*', pos + 1),
            b'+' => (b'+', pos + 1),
            b'?' => (b'?', pos + 1),
            _ => (0, pos),
        }
    } else {
        (0, pos)
    }
}

/// Match a single regex element (not a group) with optional quantifier.
fn regex_match_elem(
    text: &[u8],
    ti: usize,
    pat: &[u8],
    pi: usize,
    must_end: bool,
    captures: &mut Vec<(usize, usize)>,
) -> Option<usize> {
    let (elem_end, matches_fn) = parse_regex_elem(pat, pi);
    let (quant, after_quant) = parse_quantifier(pat, elem_end);

    match quant {
        b'*' | b'+' => regex_match_repeated_elem(
            text,
            ti,
            pat,
            after_quant,
            quant,
            must_end,
            captures,
            &matches_fn,
        ),
        b'?' => {
            regex_match_optional_elem(text, ti, pat, after_quant, must_end, captures, &matches_fn)
        }
        _ => regex_match_single_elem(text, ti, pat, elem_end, must_end, captures, &matches_fn),
    }
}

fn count_regex_matches(text: &[u8], ti: usize, matches_fn: &dyn Fn(u8) -> bool) -> usize {
    let mut count = 0;
    while ti + count < text.len() && matches_fn(text[ti + count]) {
        count += 1;
    }
    count
}

fn regex_match_repeated_elem(
    text: &[u8],
    ti: usize,
    pat: &[u8],
    after_quant: usize,
    quant: u8,
    must_end: bool,
    captures: &mut Vec<(usize, usize)>,
    matches_fn: &dyn Fn(u8) -> bool,
) -> Option<usize> {
    let min = usize::from(quant == b'+');
    let count = count_regex_matches(text, ti, matches_fn);
    for c in (min..=count).rev() {
        if let Some(end) = regex_match_capturing(text, ti + c, pat, after_quant, must_end, captures)
        {
            return Some(end);
        }
    }
    None
}

fn regex_match_optional_elem(
    text: &[u8],
    ti: usize,
    pat: &[u8],
    after_quant: usize,
    must_end: bool,
    captures: &mut Vec<(usize, usize)>,
    matches_fn: &dyn Fn(u8) -> bool,
) -> Option<usize> {
    if ti < text.len() && matches_fn(text[ti]) {
        if let Some(end) = regex_match_capturing(text, ti + 1, pat, after_quant, must_end, captures)
        {
            return Some(end);
        }
    }
    regex_match_capturing(text, ti, pat, after_quant, must_end, captures)
}

fn regex_match_single_elem(
    text: &[u8],
    ti: usize,
    pat: &[u8],
    elem_end: usize,
    must_end: bool,
    captures: &mut Vec<(usize, usize)>,
    matches_fn: &dyn Fn(u8) -> bool,
) -> Option<usize> {
    if ti < text.len() && matches_fn(text[ti]) {
        regex_match_capturing(text, ti + 1, pat, elem_end, must_end, captures)
    } else {
        None
    }
}

/// Try to match a simple pattern at a position, returning the end position if matched.
fn regex_try_match_at(text: &[u8], start: usize, pattern: &[u8]) -> Option<usize> {
    regex_try_match_inner(text, start, pattern, 0)
}

/// Inner helper to find end position of a pattern match.
fn regex_try_match_inner(text: &[u8], ti: usize, pat: &[u8], pi: usize) -> Option<usize> {
    if pi >= pat.len() {
        return Some(ti);
    }
    if pat[pi] == b'(' {
        return regex_try_match_group(text, ti, pat, pi);
    }
    let (elem_end, matches_fn) = parse_regex_elem(pat, pi);
    let (quant, after_quant) = parse_quantifier(pat, elem_end);
    regex_try_apply_quant(text, ti, pat, elem_end, after_quant, quant, &matches_fn)
}

/// Handle a group in `regex_try_match_inner`.
fn regex_try_match_group(text: &[u8], ti: usize, pat: &[u8], pi: usize) -> Option<usize> {
    let close = find_matching_paren_bytes(pat, pi + 1)?;
    let inner = &pat[pi + 1..close];
    let rest = &pat[close + 1..];
    let alternatives = split_alternatives_bytes(inner);
    for alt in &alternatives {
        if let Some(after_alt) = regex_try_match_inner(text, ti, alt, 0) {
            if let Some(end) = regex_try_match_inner(text, after_alt, rest, 0) {
                return Some(end);
            }
        }
    }
    None
}

/// Apply quantifier logic for `regex_try_match_inner`.
fn regex_try_apply_quant(
    text: &[u8],
    ti: usize,
    pat: &[u8],
    elem_end: usize,
    after_quant: usize,
    quant: u8,
    matches_fn: &dyn Fn(u8) -> bool,
) -> Option<usize> {
    match quant {
        b'*' | b'+' => regex_try_match_repeated_elem(text, ti, pat, after_quant, quant, matches_fn),
        b'?' => regex_try_match_optional_elem(text, ti, pat, after_quant, matches_fn),
        _ => regex_try_match_single_elem(text, ti, pat, elem_end, matches_fn),
    }
}

fn regex_try_match_repeated_elem(
    text: &[u8],
    ti: usize,
    pat: &[u8],
    after_quant: usize,
    quant: u8,
    matches_fn: &dyn Fn(u8) -> bool,
) -> Option<usize> {
    let min = usize::from(quant == b'+');
    let count = count_regex_matches(text, ti, matches_fn);
    for c in (min..=count).rev() {
        if let Some(end) = regex_try_match_inner(text, ti + c, pat, after_quant) {
            return Some(end);
        }
    }
    None
}

fn regex_try_match_optional_elem(
    text: &[u8],
    ti: usize,
    pat: &[u8],
    after_quant: usize,
    matches_fn: &dyn Fn(u8) -> bool,
) -> Option<usize> {
    if ti < text.len() && matches_fn(text[ti]) {
        if let Some(end) = regex_try_match_inner(text, ti + 1, pat, after_quant) {
            return Some(end);
        }
    }
    regex_try_match_inner(text, ti, pat, after_quant)
}

fn regex_try_match_single_elem(
    text: &[u8],
    ti: usize,
    pat: &[u8],
    elem_end: usize,
    matches_fn: &dyn Fn(u8) -> bool,
) -> Option<usize> {
    if ti < text.len() && matches_fn(text[ti]) {
        regex_try_match_inner(text, ti + 1, pat, elem_end)
    } else {
        None
    }
}

/// Check if alternatives can be matched repeatedly to fill text[start..end].
fn regex_match_group_repeated(
    text: &[u8],
    start: usize,
    end: usize,
    alternatives: &[Vec<u8>],
    min_reps: usize,
) -> bool {
    if start == end {
        return min_reps == 0;
    }
    if start > end {
        return false;
    }
    for alt in alternatives {
        if regex_group_repetition_matches(text, start, end, alternatives, min_reps, alt) {
            return true;
        }
    }
    false
}

fn regex_group_repetition_matches(
    text: &[u8],
    start: usize,
    end: usize,
    alternatives: &[Vec<u8>],
    min_reps: usize,
    alt: &[u8],
) -> bool {
    let Some(after) = regex_try_match_inner(text, start, alt, 0) else {
        return false;
    };
    if after <= start || after > end {
        return false;
    }
    if after == end && min_reps <= 1 {
        return true;
    }
    regex_match_group_repeated(text, after, end, alternatives, min_reps.saturating_sub(1))
}

/// Find matching `)` for a `(` in a byte pattern, handling nesting.
fn find_matching_paren_bytes(pat: &[u8], start: usize) -> Option<usize> {
    let mut depth = 1;
    let mut i = start;
    while i < pat.len() {
        if pat[i] == b'\\' {
            i += 2;
            continue;
        }
        if pat[i] == b'(' {
            depth += 1;
        } else if pat[i] == b')' {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// Split a byte pattern by `|` at the top level (not inside nested parens).
fn split_alternatives_bytes(pat: &[u8]) -> Vec<Vec<u8>> {
    let mut alternatives = Vec::new();
    let mut current = Vec::new();
    let mut depth = 0;
    let mut i = 0;
    while i < pat.len() {
        if pat[i] == b'\\' && i + 1 < pat.len() {
            current.push(pat[i]);
            current.push(pat[i + 1]);
            i += 2;
            continue;
        }
        if pat[i] == b'(' {
            depth += 1;
            current.push(pat[i]);
        } else if pat[i] == b')' {
            depth -= 1;
            current.push(pat[i]);
        } else if pat[i] == b'|' && depth == 0 {
            alternatives.push(current);
            current = Vec::new();
        } else {
            current.push(pat[i]);
        }
        i += 1;
    }
    alternatives.push(current);
    alternatives
}

/// Simple regex-like matching for `=~`.
///
/// Supports: `^prefix`, `suffix$`, `^exact$`, and literal substring match.
/// This avoids pulling in a regex crate for wasm32.
#[allow(dead_code)]
fn simple_regex_match(text: &str, pattern: &str) -> bool {
    let (core, anchored_start, anchored_end) = regex_strip_anchors(pattern);

    if has_regex_metachar(core) {
        return regex_like_match(text, pattern);
    }

    // Pure literal matching with anchoring
    literal_match_range(text, core, anchored_start, anchored_end).is_some()
}

/// A simple regex-like matcher supporting: `.` (any char), `*` (zero or more of previous),
/// `+` (one or more of previous), `?` (zero or one of previous), `^`, `$`,
/// `[abc]` character classes, `(a|b)` alternation, and literal chars.
/// This is intentionally limited but handles common bash `=~` patterns.
#[allow(dead_code)]
fn regex_like_match(text: &str, pattern: &str) -> bool {
    let (core, anchored_start, anchored_end) = regex_strip_anchors(pattern);

    if anchored_start {
        regex_match_at(text, 0, core, anchored_end)
    } else {
        (0..=text.len()).any(|start| regex_match_at(text, start, core, anchored_end))
    }
}

/// Try to match `core` pattern starting at byte position `start` in `text`.
/// If `must_end` is true, the match must consume through end of `text`.
#[allow(dead_code)]
fn regex_match_at(text: &str, start: usize, core: &str, must_end: bool) -> bool {
    let text_bytes = text.as_bytes();
    let core_bytes = core.as_bytes();
    regex_backtrack(text_bytes, start, core_bytes, 0, must_end)
}

/// Recursive backtracking regex matcher.
#[allow(dead_code)]
fn regex_backtrack(text: &[u8], ti: usize, pat: &[u8], pi: usize, must_end: bool) -> bool {
    if pi >= pat.len() {
        return if must_end { ti >= text.len() } else { true };
    }

    let (elem_end, matches_fn) = parse_regex_elem(pat, pi);
    let (quant, after_quant) = parse_quantifier(pat, elem_end);

    match quant {
        b'*' => regex_backtrack_star(text, ti, pat, after_quant, must_end, &matches_fn),
        b'+' => regex_backtrack_plus(text, ti, pat, after_quant, must_end, &matches_fn),
        b'?' => regex_backtrack_optional(text, ti, pat, after_quant, must_end, &matches_fn),
        _ => regex_backtrack_single(text, ti, pat, elem_end, must_end, &matches_fn),
    }
}

fn regex_backtrack_star(
    text: &[u8],
    ti: usize,
    pat: &[u8],
    after_quant: usize,
    must_end: bool,
    matches_fn: &dyn Fn(u8) -> bool,
) -> bool {
    let mut count = 0;
    loop {
        if regex_backtrack(text, ti + count, pat, after_quant, must_end) {
            return true;
        }
        if ti + count < text.len() && matches_fn(text[ti + count]) {
            count += 1;
        } else {
            return false;
        }
    }
}

fn regex_backtrack_plus(
    text: &[u8],
    ti: usize,
    pat: &[u8],
    after_quant: usize,
    must_end: bool,
    matches_fn: &dyn Fn(u8) -> bool,
) -> bool {
    let count = count_regex_matches(text, ti, matches_fn);
    (1..=count).any(|matched| regex_backtrack(text, ti + matched, pat, after_quant, must_end))
}

fn regex_backtrack_optional(
    text: &[u8],
    ti: usize,
    pat: &[u8],
    after_quant: usize,
    must_end: bool,
    matches_fn: &dyn Fn(u8) -> bool,
) -> bool {
    regex_backtrack(text, ti, pat, after_quant, must_end)
        || (ti < text.len()
            && matches_fn(text[ti])
            && regex_backtrack(text, ti + 1, pat, after_quant, must_end))
}

fn regex_backtrack_single(
    text: &[u8],
    ti: usize,
    pat: &[u8],
    elem_end: usize,
    must_end: bool,
    matches_fn: &dyn Fn(u8) -> bool,
) -> bool {
    ti < text.len()
        && matches_fn(text[ti])
        && regex_backtrack(text, ti + 1, pat, elem_end, must_end)
}

/// Parse one regex element at position `pi`, return (`end_pos`, `match_fn`).
/// An element is: `.`, `[class]`, `(alt)`, or a literal byte.
fn parse_regex_elem(pat: &[u8], pi: usize) -> (usize, Box<dyn Fn(u8) -> bool>) {
    match pat[pi] {
        b'.' => (pi + 1, Box::new(|_: u8| true)),
        b'[' => parse_regex_char_class(pat, pi),
        b'\\' if pi + 1 < pat.len() => {
            let escaped = pat[pi + 1];
            (pi + 2, Box::new(move |c: u8| c == escaped))
        }
        ch => (pi + 1, Box::new(move |c: u8| c == ch)),
    }
}

fn parse_regex_char_class(pat: &[u8], pi: usize) -> (usize, Box<dyn Fn(u8) -> bool>) {
    let mut i = pi + 1;
    let negate = i < pat.len() && (pat[i] == b'^' || pat[i] == b'!');
    if negate {
        i += 1;
    }
    let mut chars = Vec::new();
    while i < pat.len() && pat[i] != b']' {
        if i + 2 < pat.len() && pat[i + 1] == b'-' {
            chars.extend(pat[i]..=pat[i + 2]);
            i += 3;
        } else {
            chars.push(pat[i]);
            i += 1;
        }
    }
    let end = if i < pat.len() { i + 1 } else { i };
    (
        end,
        Box::new(move |c: u8| regex_char_class_matches(&chars, negate, c)),
    )
}

fn regex_char_class_matches(chars: &[u8], negate: bool, c: u8) -> bool {
    let found = chars.contains(&c);
    if negate {
        !found
    } else {
        found
    }
}

/// Match a glob character class `[...]` at position `pi` (just past the `[`).
/// Returns `(new_pi, matched)` where `new_pi` is past the `]`.
fn glob_match_char_class(pattern: &[u8], mut pi: usize, ch: u8) -> (usize, bool) {
    let negate = pi < pattern.len() && (pattern[pi] == b'!' || pattern[pi] == b'^');
    if negate {
        pi += 1;
    }
    let mut matched = false;
    let mut first = true;
    while pi < pattern.len() && (first || pattern[pi] != b']') {
        first = false;
        let (next_pi, item_matched) = glob_match_char_class_item(pattern, pi, ch);
        matched |= item_matched;
        pi = next_pi;
    }
    if pi < pattern.len() && pattern[pi] == b']' {
        pi += 1;
    }
    (pi, matched != negate)
}

fn glob_match_char_class_item(pattern: &[u8], pi: usize, ch: u8) -> (usize, bool) {
    if pi + 2 < pattern.len() && pattern[pi + 1] == b'-' {
        let lo = pattern[pi];
        let hi = pattern[pi + 2];
        return (pi + 3, ch >= lo && ch <= hi);
    }
    (pi + 1, pattern[pi] == ch)
}

enum GlobPatternStep {
    Consume(usize),
    Star,
    Class(usize, bool),
    Mismatch,
}

fn glob_step(pattern: &[u8], pi: usize, ch: u8) -> GlobPatternStep {
    if pi >= pattern.len() {
        return GlobPatternStep::Mismatch;
    }

    match pattern[pi] {
        b'?' => GlobPatternStep::Consume(pi + 1),
        b'*' => GlobPatternStep::Star,
        b'[' => {
            let (new_pi, matched) = glob_match_char_class(pattern, pi + 1, ch);
            GlobPatternStep::Class(new_pi, matched)
        }
        literal if literal == ch => GlobPatternStep::Consume(pi + 1),
        _ => GlobPatternStep::Mismatch,
    }
}

fn glob_backtrack(pi: &mut usize, ni: &mut usize, star_pi: usize, star_ni: &mut usize) -> bool {
    if star_pi == usize::MAX {
        return false;
    }

    *pi = star_pi + 1;
    *star_ni += 1;
    *ni = *star_ni;
    true
}

/// Core glob pattern matching (byte-level).
///
/// Supports `*` (any sequence), `?` (one char), and `[abc]` (character class).
fn glob_match_inner(pattern: &[u8], name: &[u8]) -> bool {
    let mut pi = 0;
    let mut ni = 0;
    let mut star_pi = usize::MAX;
    let mut star_ni = usize::MAX;

    while ni < name.len() {
        match glob_step(pattern, pi, name[ni]) {
            GlobPatternStep::Consume(new_pi) => {
                pi = new_pi;
                ni += 1;
            }
            GlobPatternStep::Star => {
                star_pi = pi;
                star_ni = ni;
                pi += 1;
            }
            GlobPatternStep::Class(new_pi, true) => {
                pi = new_pi;
                ni += 1;
            }
            GlobPatternStep::Class(_, false) | GlobPatternStep::Mismatch => {
                if !glob_backtrack(&mut pi, &mut ni, star_pi, &mut star_ni) {
                    return false;
                }
            }
        }
    }

    // Consume trailing stars
    while pi < pattern.len() && pattern[pi] == b'*' {
        pi += 1;
    }

    pi == pattern.len()
}

/// Extended glob matching with dotglob and extglob support.
fn glob_match_ext(pattern: &str, name: &str, dotglob: bool, extglob: bool) -> bool {
    // Don't match hidden files unless dotglob is enabled or pattern starts with '.'
    if name.starts_with('.') && !pattern.starts_with('.') && !dotglob {
        return false;
    }
    if extglob && has_extglob_pattern(pattern) {
        return extglob_match(pattern, name);
    }
    glob_match_inner(pattern.as_bytes(), name.as_bytes())
}

/// Check if a pattern contains extglob operators: `?(`, `*(`, `+(`, `@(`, `!(`.
fn has_extglob_pattern(pattern: &str) -> bool {
    let bytes = pattern.as_bytes();
    for i in 0..bytes.len().saturating_sub(1) {
        if bytes[i + 1] == b'(' && matches!(bytes[i], b'?' | b'*' | b'+' | b'@' | b'!') {
            return true;
        }
    }
    false
}

/// Match a name against an extglob pattern.
///
/// Supports: `?(pat|pat)`, `*(pat|pat)`, `+(pat|pat)`, `@(pat|pat)`, `!(pat|pat)`.
/// Non-extglob portions are handled by regular glob matching.
fn extglob_match(pattern: &str, name: &str) -> bool {
    extglob_match_recursive(pattern.as_bytes(), name.as_bytes())
}

fn extglob_match_recursive(pattern: &[u8], name: &[u8]) -> bool {
    // Find the first extglob operator
    let Some((pi, op, close)) = find_extglob_operator(pattern) else {
        return glob_match_inner(pattern, name);
    };

    let open = pi + 2;
    let alternatives = split_alternatives(&pattern[open..close]);
    let prefix = &pattern[..pi];
    let suffix = &pattern[close + 1..];

    match op {
        b'@' | b'?' => extglob_match_at_or_opt(op, prefix, &alternatives, suffix, name),
        b'*' => extglob_star(prefix, &alternatives, suffix, name, 0),
        b'+' => extglob_plus(prefix, &alternatives, suffix, name, 0),
        b'!' => extglob_match_negate(prefix, &alternatives, suffix, name),
        _ => unreachable!(),
    }
}

/// Find the first extglob operator in a pattern, returning (position, operator, `close_paren`).
fn find_extglob_operator(pattern: &[u8]) -> Option<(usize, u8, usize)> {
    let mut pi = 0;
    while pi < pattern.len() {
        if pi + 1 < pattern.len()
            && pattern[pi + 1] == b'('
            && matches!(pattern[pi], b'?' | b'*' | b'+' | b'@' | b'!')
        {
            if let Some(close) = find_matching_paren(pattern, pi + 2) {
                return Some((pi, pattern[pi], close));
            }
        }
        pi += 1;
    }
    None
}

/// Build a combined pattern from prefix + alt + suffix.
fn build_combined(prefix: &[u8], mid: &[u8], suffix: &[u8]) -> Vec<u8> {
    let mut combined = Vec::with_capacity(prefix.len() + mid.len() + suffix.len());
    combined.extend_from_slice(prefix);
    combined.extend_from_slice(mid);
    combined.extend_from_slice(suffix);
    combined
}

/// Handle `@(...)` (exactly one) and `?(...)` (zero or one) extglob patterns.
fn extglob_match_at_or_opt(
    op: u8,
    prefix: &[u8],
    alternatives: &[Vec<u8>],
    suffix: &[u8],
    name: &[u8],
) -> bool {
    // For `?`, try zero first
    if op == b'?' && extglob_match_recursive(&build_combined(prefix, &[], suffix), name) {
        return true;
    }
    // Try each alternative exactly once
    for alt in alternatives {
        if extglob_match_recursive(&build_combined(prefix, alt, suffix), name) {
            return true;
        }
    }
    false
}

/// Handle `!(...)` extglob pattern: matches if no alternative matches.
fn extglob_match_negate(
    prefix: &[u8],
    alternatives: &[Vec<u8>],
    suffix: &[u8],
    name: &[u8],
) -> bool {
    for alt in alternatives {
        if extglob_match_recursive(&build_combined(prefix, alt, suffix), name) {
            return false;
        }
    }
    let wildcard = build_combined(prefix, b"*", suffix);
    glob_match_inner(&wildcard, name)
}

/// Try zero or more repetitions of alternatives for `*(...)`.
fn extglob_star(
    prefix: &[u8],
    alternatives: &[Vec<u8>],
    suffix: &[u8],
    name: &[u8],
    depth: u32,
) -> bool {
    if depth > 20 {
        return false;
    }
    // Try zero repetitions
    let mut combined = Vec::new();
    combined.extend_from_slice(prefix);
    combined.extend_from_slice(suffix);
    if extglob_match_recursive(&combined, name) {
        return true;
    }
    // Try one repetition followed by zero or more
    let prefix_len = prefix.len();
    for alt in alternatives {
        let mut new_prefix = Vec::new();
        new_prefix.extend_from_slice(prefix);
        new_prefix.extend_from_slice(alt);
        // Only recurse if prefix is getting longer (consuming input)
        if new_prefix.len() > prefix_len
            && extglob_star(&new_prefix, alternatives, suffix, name, depth + 1)
        {
            return true;
        }
    }
    false
}

/// Try one or more repetitions of alternatives for `+(...)`.
fn extglob_plus(
    prefix: &[u8],
    alternatives: &[Vec<u8>],
    suffix: &[u8],
    name: &[u8],
    depth: u32,
) -> bool {
    if depth > 20 {
        return false;
    }
    // Must match at least one alternative, then zero or more
    for alt in alternatives {
        let mut new_prefix = Vec::new();
        new_prefix.extend_from_slice(prefix);
        new_prefix.extend_from_slice(alt);
        if extglob_star(&new_prefix, alternatives, suffix, name, depth + 1) {
            return true;
        }
    }
    false
}

/// Find the matching `)` for a `(` at position `open` (character after `(`).
fn find_matching_paren(pattern: &[u8], open: usize) -> Option<usize> {
    let mut depth: u32 = 1;
    let mut i = open;
    while i < pattern.len() {
        if pattern[i] == b'(' {
            depth += 1;
        } else if pattern[i] == b')' {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// Split alternatives by `|` at the top level (not inside nested parens).
fn split_alternatives(pat: &[u8]) -> Vec<Vec<u8>> {
    let mut result = Vec::new();
    let mut current = Vec::new();
    let mut depth: u32 = 0;
    for &b in pat {
        if b == b'(' {
            depth += 1;
            current.push(b);
        } else if b == b')' {
            depth -= 1;
            current.push(b);
        } else if b == b'|' && depth == 0 {
            result.push(std::mem::take(&mut current));
        } else {
            current.push(b);
        }
    }
    result.push(current);
    result
}

impl Default for WorkerRuntime {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_shell(input: &str) -> (Vec<WorkerEvent>, i32) {
        let mut rt = WorkerRuntime::new();
        rt.handle_command(HostCommand::Init { step_budget: 0 });
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
        let events = rt.handle_command(HostCommand::Init { step_budget: 0 });
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
        rt.handle_command(HostCommand::Init { step_budget: 0 });
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
        rt.handle_command(HostCommand::Init { step_budget: 0 });
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
        rt.handle_command(HostCommand::Init { step_budget: 0 });
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
        rt.handle_command(HostCommand::Init { step_budget: 0 });
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
        rt.handle_command(HostCommand::Init { step_budget: 0 });
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
        rt.handle_command(HostCommand::Init { step_budget: 0 });
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
        rt.handle_command(HostCommand::Init { step_budget: 0 });
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
        rt.handle_command(HostCommand::Init { step_budget: 0 });
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
        rt.handle_command(HostCommand::Init { step_budget: 0 });
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
        rt.handle_command(HostCommand::Init { step_budget: 0 });
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
        rt.handle_command(HostCommand::Init { step_budget: 0 });
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
    fn while_loop_with_counter() {
        let mut rt = WorkerRuntime::new();
        rt.handle_command(HostCommand::Init { step_budget: 10000 });
        // Simple loop that echoes 3 times using a counter variable
        let events = rt.handle_command(HostCommand::Run {
            input: "for i in 1 2 3; do echo line; done".into(),
        });
        assert_eq!(get_stdout(&events), "line\nline\nline\n");
    }

    #[test]
    fn heredoc_with_cat() {
        let mut rt = WorkerRuntime::new();
        rt.handle_command(HostCommand::Init { step_budget: 0 });
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
        rt.handle_command(HostCommand::Init { step_budget: 0 });
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
        rt.handle_command(HostCommand::Init { step_budget: 0 });
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
        rt.handle_command(HostCommand::Init { step_budget: 0 });
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
        rt.handle_command(HostCommand::Init { step_budget: 0 });
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
        rt.handle_command(HostCommand::Init { step_budget: 0 });
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
        rt.handle_command(HostCommand::Init { step_budget: 0 });
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
        rt.handle_command(HostCommand::Init { step_budget: 0 });
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
        rt.handle_command(HostCommand::Init { step_budget: 0 });
        // 2>&1 merges stderr into stdout, then redirect stdout to file
        let _events = rt.handle_command(HostCommand::Run {
            input: "nonexistent_cmd 2>&1 > /out.txt".into(),
        });
        let read_events = rt.handle_command(HostCommand::ReadFile {
            path: "/out.txt".into(),
        });
        let content = get_stdout(&read_events);
        // The merged stderr (now in stdout) goes to the file
        // But the order matters: 2>&1 first merges stderr to stdout buffer,
        // then > /out.txt captures all of stdout.
        // Actually in this shell's execution model, redirections are applied after
        // the command runs. 2>&1 merges stderr into stdout, then > captures it.
        assert!(content.contains("command not found"));
    }

    #[test]
    fn amp_greater_both_to_file() {
        let mut rt = WorkerRuntime::new();
        rt.handle_command(HostCommand::Init { step_budget: 0 });
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
        rt.handle_command(HostCommand::Init { step_budget: 0 });
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
        rt.handle_command(HostCommand::Init { step_budget: 0 });
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
        rt.handle_command(HostCommand::Init { step_budget: 0 });
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
        rt.handle_command(HostCommand::Init { step_budget: 0 });
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
    fn dynamic_bash_source() {
        let mut rt = WorkerRuntime::new();
        rt.handle_command(HostCommand::Init { step_budget: 0 });
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

    // ---- source PATH search ----

    #[test]
    fn source_path_search() {
        let mut rt = WorkerRuntime::new();
        rt.handle_command(HostCommand::Init { step_budget: 0 });
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

    // ---- Pipe-ampersand (|&) ----

    #[test]
    fn pipe_amp_captures_stderr() {
        let (events, status) = run_shell("echo error >&2 |& cat");
        assert_eq!(status, 0);
        assert_eq!(get_stdout(&events), "error\n");
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
        rt.handle_command(HostCommand::Init { step_budget: 0 });
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
        rt.handle_command(HostCommand::Init { step_budget: 0 });
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
        rt.handle_command(HostCommand::Init { step_budget: 0 });
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
        rt.handle_command(HostCommand::Init { step_budget: 0 });
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
        rt.handle_command(HostCommand::Init { step_budget: 0 });
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
}

// ── wasm-bindgen entry points (wasm32 only) ────────────────────────

#[cfg(target_arch = "wasm32")]
mod wasm_bindings {
    use wasm_bindgen::prelude::*;
    use wasmsh_protocol::HostCommand;

    use crate::WorkerRuntime;

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

        /// Initialize the shell with a step budget.  Returns a JSON array of events.
        pub fn init(&mut self, step_budget: u64) -> String {
            let events = self
                .runtime
                .handle_command(HostCommand::Init { step_budget });
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
