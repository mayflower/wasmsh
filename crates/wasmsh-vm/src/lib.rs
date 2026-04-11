//! Cooperative virtual machine for the wasmsh executor subset.
//!
//! The full shell still runs primarily in `wasmsh-runtime`. This VM is
//! used for the lowered IR subset and provides the shared budgeting,
//! cancellation, diagnostics, and output accounting primitives that the
//! runtime also relies on for resumable execution.

pub mod pipe;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use wasmsh_builtins::{BuiltinContext, BuiltinRegistry, VecSink as BuiltinSink};
use wasmsh_ast::Word;
use wasmsh_ir::{Ir, IrProgram, IrRedirection};
use wasmsh_state::ShellState;

/// Outcome of VM execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepResult {
    /// The program completed with the given exit code.
    Done(i32),
    /// The step budget was exhausted; the caller should yield.
    Yield,
    /// Execution was cancelled externally.
    Cancelled,
    /// Output byte limit was exceeded.
    OutputLimitExceeded,
}

/// Configurable execution limits.
#[derive(Debug, Clone, Default)]
pub struct ExecutionLimits {
    /// Maximum VM steps (0 = unlimited).
    pub step_limit: u64,
    /// Maximum bytes of combined stdout+stderr output (0 = unlimited).
    pub output_byte_limit: u64,
    /// Maximum bytes buffered in pipes/streaming buffers (0 = unlimited).
    pub pipe_byte_limit: u64,
    /// Maximum nested execution depth (0 = unlimited).
    pub recursion_limit: u32,
}

/// A structured diagnostic event emitted during execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticEvent {
    pub level: DiagLevel,
    pub category: DiagCategory,
    pub message: String,
}

/// Diagnostic severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagLevel {
    Trace,
    Info,
    Warning,
    Error,
}

/// Category of diagnostic event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagCategory {
    Parse,
    Expansion,
    Runtime,
    Filesystem,
    Builtin,
    Budget,
}

/// Structured budget category tracked during execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetCategory {
    Steps,
    VisibleOutputBytes,
    PipeBytes,
    RecursionDepth,
}

/// Stable exhaustion reason for a specific tracked budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExhaustionReason {
    pub category: BudgetCategory,
    pub used: u64,
    pub limit: u64,
}

impl ExhaustionReason {
    #[must_use]
    pub fn diagnostic_message(&self) -> String {
        match self.category {
            BudgetCategory::Steps => {
                format!("step budget exhausted: {} steps (limit: {})", self.used, self.limit)
            }
            BudgetCategory::VisibleOutputBytes => format!(
                "output limit exceeded: {} bytes (limit: {})",
                self.used, self.limit
            ),
            BudgetCategory::PipeBytes => format!(
                "pipe buffer limit exceeded: {} bytes (limit: {})",
                self.used, self.limit
            ),
            BudgetCategory::RecursionDepth => format!(
                "maximum recursion depth exceeded: {} frames (limit: {})",
                self.used, self.limit
            ),
        }
    }
}

/// Structured stop reason for the current execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    Exhausted(ExhaustionReason),
    Cancelled,
}

/// Shared budget accounting across the VM and runtime layers.
#[derive(Debug, Clone, Default)]
pub struct BudgetTracker {
    pub steps: u64,
    pub visible_output_bytes: u64,
    pub pipe_bytes: u64,
    pub recursion_depth: u32,
    stop_reason: Option<StopReason>,
}

impl BudgetTracker {
    #[must_use]
    pub fn stop_reason(&self) -> Option<&StopReason> {
        self.stop_reason.as_ref()
    }

    pub fn clear_stop_reason(&mut self) {
        self.stop_reason = None;
    }

    fn exhaust(&mut self, reason: ExhaustionReason) -> ExhaustionReason {
        self.stop_reason = Some(StopReason::Exhausted(reason.clone()));
        reason
    }

    pub fn note_cancelled(&mut self) {
        self.stop_reason = Some(StopReason::Cancelled);
    }

    pub fn begin_step(&mut self, limit: u64) -> Result<(), ExhaustionReason> {
        if limit > 0 && self.steps >= limit {
            return Err(self.exhaust(ExhaustionReason {
                category: BudgetCategory::Steps,
                used: self.steps,
                limit,
            }));
        }
        self.steps += 1;
        Ok(())
    }

    pub fn track_visible_output(&mut self, bytes: u64, limit: u64) -> Result<(), ExhaustionReason> {
        self.visible_output_bytes = self.visible_output_bytes.saturating_add(bytes);
        if limit > 0 && self.visible_output_bytes > limit {
            return Err(self.exhaust(ExhaustionReason {
                category: BudgetCategory::VisibleOutputBytes,
                used: self.visible_output_bytes,
                limit,
            }));
        }
        Ok(())
    }

    pub fn set_pipe_bytes(&mut self, bytes: u64, limit: u64) -> Result<(), ExhaustionReason> {
        self.pipe_bytes = bytes;
        if limit > 0 && self.pipe_bytes > limit {
            return Err(self.exhaust(ExhaustionReason {
                category: BudgetCategory::PipeBytes,
                used: self.pipe_bytes,
                limit,
            }));
        }
        Ok(())
    }

    pub fn enter_recursion(&mut self, limit: u32) -> Result<(), ExhaustionReason> {
        self.recursion_depth = self.recursion_depth.saturating_add(1);
        if limit > 0 && self.recursion_depth > limit {
            return Err(self.exhaust(ExhaustionReason {
                category: BudgetCategory::RecursionDepth,
                used: self.recursion_depth as u64,
                limit: limit as u64,
            }));
        }
        Ok(())
    }

    pub fn exit_recursion(&mut self) {
        self.recursion_depth = self.recursion_depth.saturating_sub(1);
    }
}

/// A cancellation token that can be shared across threads.
#[derive(Debug, Clone)]
pub struct CancellationToken {
    flag: Arc<AtomicBool>,
}

impl CancellationToken {
    #[must_use]
    pub fn new() -> Self {
        Self {
            flag: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Signal cancellation.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::Relaxed);
    }

    /// Check whether cancellation was requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Relaxed)
    }

    /// Reset the cancellation flag.
    pub fn reset(&self) {
        self.flag.store(false, Ordering::Relaxed);
    }
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

/// The shell virtual machine.
#[allow(missing_debug_implementations)]
pub struct Vm {
    /// Shell state (variables, params, cwd, etc.).
    pub state: ShellState,
    /// Number of steps executed so far.
    pub steps: u64,
    /// Execution limits.
    pub limits: ExecutionLimits,
    /// Bytes of output produced so far.
    pub output_bytes: u64,
    /// Shared budget accounting and stable stop reasons.
    pub budget: BudgetTracker,
    /// Cancellation token.
    cancel: CancellationToken,
    /// Collected diagnostic events.
    pub diagnostics: Vec<DiagnosticEvent>,
    /// Builtin command registry.
    builtins: BuiltinRegistry,
    /// Collected stdout output from command execution.
    pub stdout: Vec<u8>,
    /// Collected stderr output from command execution.
    pub stderr: Vec<u8>,
}

pub trait VmExecutor {
    fn assign(&mut self, vm: &mut Vm, name: &str, value: Option<&Word>);

    fn execute_builtin(
        &mut self,
        vm: &mut Vm,
        name: &str,
        argv: &[Word],
        redirections: &[IrRedirection],
    ) -> i32;
}

struct BuiltinVmExecutor {
    builtins: BuiltinRegistry,
}

impl VmExecutor for BuiltinVmExecutor {
    fn assign(&mut self, vm: &mut Vm, name: &str, value: Option<&Word>) {
        let value = value.map_or_else(String::new, |word| {
            wasmsh_expand::expand_word(word, &mut vm.state)
        });
        vm.state.set_var(name.into(), value.into());
        vm.state.last_status = 0;
    }

    fn execute_builtin(
        &mut self,
        vm: &mut Vm,
        name: &str,
        argv: &[Word],
        _redirections: &[IrRedirection],
    ) -> i32 {
        let Some(builtin_fn) = self.builtins.get(name) else {
            vm.emit_diagnostic(
                DiagLevel::Error,
                DiagCategory::Builtin,
                format!("unknown builtin: {name}"),
            );
            vm.state.last_status = 127;
            return 127;
        };

        let expanded: Vec<String> = argv
            .iter()
            .map(|word| wasmsh_expand::expand_word(word, &mut vm.state))
            .collect();
        let argv_refs: Vec<&str> = expanded.iter().map(String::as_str).collect();
        let mut sink = BuiltinSink::default();
        let status = {
            let mut ctx = BuiltinContext {
                state: &mut vm.state,
                output: &mut sink,
                fs: None,
                stdin: None,
            };
            builtin_fn(&mut ctx, &argv_refs)
        };
        vm.write_streams(&sink.stdout, &sink.stderr);
        vm.state.last_status = status;
        status
    }
}

impl Vm {
    /// Create a new VM with the given state and limits.
    #[must_use]
    pub fn new(state: ShellState, step_budget: u64) -> Self {
        Self {
            state,
            steps: 0,
            limits: ExecutionLimits {
                step_limit: step_budget,
                ..ExecutionLimits::default()
            },
            output_bytes: 0,
            budget: BudgetTracker::default(),
            cancel: CancellationToken::new(),
            diagnostics: Vec::new(),
            builtins: BuiltinRegistry::new(),
            stdout: Vec::new(),
            stderr: Vec::new(),
        }
    }

    /// Create a VM with full execution limits.
    #[must_use]
    pub fn with_limits(state: ShellState, limits: ExecutionLimits) -> Self {
        Self {
            state,
            steps: 0,
            limits,
            output_bytes: 0,
            budget: BudgetTracker::default(),
            cancel: CancellationToken::new(),
            diagnostics: Vec::new(),
            builtins: BuiltinRegistry::new(),
            stdout: Vec::new(),
            stderr: Vec::new(),
        }
    }

    /// Emit a diagnostic event.
    pub fn emit_diagnostic(&mut self, level: DiagLevel, category: DiagCategory, message: String) {
        self.diagnostics.push(DiagnosticEvent {
            level,
            category,
            message,
        });
    }

    fn budget_stop(&mut self, result: StepResult, message: String) -> StepResult {
        self.emit_diagnostic(DiagLevel::Error, DiagCategory::Budget, message);
        result
    }

    #[must_use]
    pub fn stop_reason(&self) -> Option<&StopReason> {
        self.budget.stop_reason()
    }

    /// Track output bytes and check the limit. Returns true if within limits.
    pub fn track_output(&mut self, bytes: u64) -> bool {
        self.output_bytes += bytes;
        self.budget
            .track_visible_output(bytes, self.limits.output_byte_limit)
            .is_ok()
    }

    /// Append stdout bytes and update output accounting.
    pub fn write_stdout(&mut self, data: &[u8]) {
        self.stdout.extend_from_slice(data);
        self.track_output(data.len() as u64);
    }

    /// Append stderr bytes and update output accounting.
    pub fn write_stderr(&mut self, data: &[u8]) {
        self.stderr.extend_from_slice(data);
        self.track_output(data.len() as u64);
    }

    /// Append both stdout and stderr bytes and update output accounting once.
    pub fn write_streams(&mut self, stdout: &[u8], stderr: &[u8]) {
        self.stdout.extend_from_slice(stdout);
        self.stderr.extend_from_slice(stderr);
        self.track_output((stdout.len() + stderr.len()) as u64);
    }

    /// Check whether the accumulated output has exceeded the configured limit.
    pub fn check_output_limit(&mut self) -> Result<(), StepResult> {
        if let Some(StopReason::Exhausted(reason)) = self.stop_reason() {
            if reason.category == BudgetCategory::VisibleOutputBytes {
                return Err(self.budget_stop(
                    StepResult::OutputLimitExceeded,
                    reason.diagnostic_message(),
                ));
            }
        }
        Ok(())
    }

    /// Consume one execution step using the VM's shared budget/cancel semantics.
    pub fn begin_step(&mut self) -> Result<(), StepResult> {
        if self.cancel.is_cancelled() {
            self.budget.note_cancelled();
            return Err(self.budget_stop(StepResult::Cancelled, "execution cancelled".to_string()));
        }
        self.check_output_limit()?;
        if let Err(reason) = self.budget.begin_step(self.limits.step_limit) {
            self.steps = self.budget.steps;
            return Err(self.budget_stop(
                StepResult::Yield,
                reason.diagnostic_message(),
            ));
        }
        self.steps = self.budget.steps;
        Ok(())
    }

    /// Get the cancellation token (can be cloned and shared).
    #[must_use]
    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    /// Execute an IR program to completion (or until yield/cancel).
    pub fn run(&mut self, program: &IrProgram) -> StepResult {
        let builtins = std::mem::take(&mut self.builtins);
        let mut executor = BuiltinVmExecutor { builtins };
        let result = self.run_with_executor(program, &mut executor);
        self.builtins = executor.builtins;
        result
    }

    pub fn run_with_executor<E: VmExecutor>(
        &mut self,
        program: &IrProgram,
        executor: &mut E,
    ) -> StepResult {
        let mut pc = 0;
        let instructions = &program.instructions;

        while pc < instructions.len() {
            if let Err(stop) = self.begin_step() {
                return stop;
            }

            match &instructions[pc] {
                Ir::Assign { name, value } => {
                    executor.assign(self, name.as_str(), value.as_ref());
                }
                Ir::ExecuteBuiltin {
                    name,
                    argv,
                    redirections,
                } => {
                    let status = executor.execute_builtin(self, name, argv, redirections);
                    self.state.last_status = status;
                }
                Ir::JumpIfFailure { target } => {
                    if self.state.last_status != 0 {
                        pc = *target;
                        continue;
                    }
                }
                Ir::JumpIfSuccess { target } => {
                    if self.state.last_status == 0 {
                        pc = *target;
                        continue;
                    }
                }
                Ir::ReturnLastStatus => {
                    return StepResult::Done(self.state.last_status);
                }
                Ir::Return { status } => {
                    self.state.last_status = *status;
                    return StepResult::Done(*status);
                }
                Ir::Nop => {}
            }

            pc += 1;
        }

        StepResult::Done(self.state.last_status)
    }
}

impl Default for Vm {
    fn default() -> Self {
        Self::new(ShellState::new(), 0)
    }
}

#[cfg(test)]
    mod tests {
        use super::*;
        use wasmsh_ast::{RedirectionOp, Span, WordPart};

        #[derive(Default)]
        struct TestExecutor {
            seen_redirections: Vec<Vec<IrRedirection>>,
        }

        impl VmExecutor for TestExecutor {
            fn assign(&mut self, vm: &mut Vm, name: &str, value: Option<&Word>) {
                let value = value.map_or_else(String::new, |word| {
                    wasmsh_expand::expand_word(word, &mut vm.state)
                });
                vm.state.set_var(name.into(), value.into());
                vm.state.last_status = 0;
            }

            fn execute_builtin(
                &mut self,
                vm: &mut Vm,
                name: &str,
                argv: &[Word],
                redirections: &[IrRedirection],
            ) -> i32 {
                self.seen_redirections.push(redirections.to_vec());
                let expanded: Vec<String> = argv
                    .iter()
                    .map(|word| wasmsh_expand::expand_word(word, &mut vm.state))
                    .collect();
                let status = match name {
                    "echo" => {
                        let text = expanded[1..].join(" ");
                        vm.write_stdout(format!("{text}\n").as_bytes());
                        0
                    }
                    "true" => 0,
                    "false" => 1,
                    _ => 127,
                };
                vm.state.last_status = status;
                status
            }
        }

    #[test]
    fn run_empty_program() {
        let mut vm = Vm::default();
        let prog = IrProgram::new(vec![]);
        assert_eq!(vm.run(&prog), StepResult::Done(0));
    }

    #[test]
    fn run_return() {
        let mut vm = Vm::default();
        let prog = IrProgram::new(vec![Ir::Return { status: 42 }]);
        assert_eq!(vm.run(&prog), StepResult::Done(42));
        assert_eq!(vm.state.last_status, 42);
    }

    #[test]
    fn run_set_var() {
        let mut vm = Vm::default();
        let prog = IrProgram::new(vec![
            Ir::Assign {
                name: "FOO".into(),
                value: Some(literal_word("bar")),
            },
            Ir::Return { status: 0 },
        ]);
        assert_eq!(vm.run(&prog), StepResult::Done(0));
        assert_eq!(vm.state.get_var("FOO").unwrap(), "bar");
    }

    #[test]
    fn run_builtin_placeholder() {
        let mut vm = Vm::default();
        let prog = IrProgram::new(vec![
            Ir::ExecuteBuiltin {
                name: "echo".into(),
                argv: vec![literal_word("echo"), literal_word("hello")],
                redirections: Vec::new(),
            },
            Ir::Return { status: 0 },
        ]);
        assert_eq!(vm.run(&prog), StepResult::Done(0));
        assert_eq!(String::from_utf8(vm.stdout).unwrap(), "hello\n");
    }

    #[test]
    fn step_counting() {
        let mut vm = Vm::default();
        let prog = IrProgram::new(vec![Ir::Nop, Ir::Nop, Ir::Nop]);
        vm.run(&prog);
        assert_eq!(vm.steps, 3);
    }

    #[test]
    fn step_budget_yield() {
        let mut vm = Vm::new(ShellState::new(), 2);
        let prog = IrProgram::new(vec![Ir::Nop, Ir::Nop, Ir::Nop, Ir::Nop]);
        assert_eq!(vm.run(&prog), StepResult::Yield);
        assert_eq!(vm.steps, 2);
    }

    #[test]
    fn output_limit() {
        let mut vm = Vm::with_limits(
            ShellState::new(),
            ExecutionLimits {
                step_limit: 0,
                output_byte_limit: 10,
                ..ExecutionLimits::default()
            },
        );
        assert!(vm.track_output(5));
        assert!(vm.track_output(5));
        assert!(!vm.track_output(1));
    }

    #[test]
    fn diagnostics_collected() {
        let mut vm = Vm::default();
        vm.emit_diagnostic(
            DiagLevel::Warning,
            DiagCategory::Budget,
            "step limit approaching".into(),
        );
        assert_eq!(vm.diagnostics.len(), 1);
        assert_eq!(vm.diagnostics[0].level, DiagLevel::Warning);
        assert_eq!(vm.diagnostics[0].category, DiagCategory::Budget);
    }

    #[test]
    fn cancellation() {
        let mut vm = Vm::default();
        let token = vm.cancellation_token();
        token.cancel();
        let prog = IrProgram::new(vec![Ir::Nop]);
        assert_eq!(vm.run(&prog), StepResult::Cancelled);
        assert!(vm
            .diagnostics
            .iter()
            .any(|d| d.message.contains("execution cancelled")));
    }

    #[test]
    fn cancellation_token_reset() {
        let token = CancellationToken::new();
        assert!(!token.is_cancelled());
        token.cancel();
        assert!(token.is_cancelled());
        token.reset();
        assert!(!token.is_cancelled());
    }

    #[test]
    fn status_propagation() {
        let mut vm = Vm::default();
        let prog = IrProgram::new(vec![
            Ir::Assign {
                name: "X".into(),
                value: Some(literal_word("1")),
            },
            Ir::Return { status: 7 },
        ]);
        vm.run(&prog);
        assert_eq!(vm.state.last_status, 7);
        assert_eq!(vm.state.get_var("?").unwrap(), "7");
        assert_eq!(vm.state.get_var("X").unwrap(), "1");
    }

    #[test]
    fn begin_step_matches_vm_budget_semantics() {
        let mut vm = Vm::new(ShellState::new(), 1);
        assert_eq!(vm.begin_step(), Ok(()));
        assert_eq!(vm.steps, 1);
        assert_eq!(vm.begin_step(), Err(StepResult::Yield));
        assert!(vm
            .diagnostics
            .iter()
            .any(|d| d.message.contains("step budget exhausted")));
    }

    #[test]
    fn output_limit_is_reported_through_begin_step() {
        let mut vm = Vm::with_limits(
            ShellState::new(),
            ExecutionLimits {
                step_limit: 0,
                output_byte_limit: 3,
                ..ExecutionLimits::default()
            },
        );
        vm.write_stdout(b"four");
        assert_eq!(vm.begin_step(), Err(StepResult::OutputLimitExceeded));
        assert!(vm
            .diagnostics
            .iter()
            .any(|d| d.message.contains("output limit exceeded")));
    }

    #[test]
    fn step_limit_exposes_structured_stop_reason() {
        let mut vm = Vm::new(ShellState::new(), 1);
        assert_eq!(vm.begin_step(), Ok(()));
        assert_eq!(vm.begin_step(), Err(StepResult::Yield));
        assert_eq!(
            vm.stop_reason(),
            Some(&StopReason::Exhausted(ExhaustionReason {
                category: BudgetCategory::Steps,
                used: 1,
                limit: 1,
            }))
        );
    }

    #[test]
    fn cancellation_remains_distinct_from_budget_exhaustion() {
        let mut vm = Vm::default();
        vm.cancellation_token().cancel();
        assert_eq!(vm.begin_step(), Err(StepResult::Cancelled));
        assert_eq!(vm.stop_reason(), Some(&StopReason::Cancelled));
    }

    #[test]
    fn budget_tracker_tracks_pipe_and_recursion_limits() {
        let mut tracker = BudgetTracker::default();
        let pipe = tracker.set_pipe_bytes(9, 8).unwrap_err();
        assert_eq!(pipe.category, BudgetCategory::PipeBytes);
        assert_eq!(pipe.limit, 8);

        let mut tracker = BudgetTracker::default();
        tracker.enter_recursion(2).unwrap();
        tracker.enter_recursion(2).unwrap();
        let recursion = tracker.enter_recursion(2).unwrap_err();
        assert_eq!(recursion.category, BudgetCategory::RecursionDepth);
        assert_eq!(recursion.used, 3);
    }

    #[test]
    fn run_assignment_and_expanding_builtin_with_executor() {
        let mut vm = Vm::default();
        let mut executor = TestExecutor::default();
        let prog = IrProgram::new(vec![
            Ir::Assign {
                name: "FOO".into(),
                value: Some(literal_word("bar")),
            },
            Ir::ExecuteBuiltin {
                name: "echo".into(),
                argv: vec![literal_word("echo"), parameter_word("FOO")],
                redirections: Vec::new(),
            },
            Ir::ReturnLastStatus,
        ]);
        assert_eq!(vm.run_with_executor(&prog, &mut executor), StepResult::Done(0));
        assert_eq!(vm.state.get_var("FOO").unwrap(), "bar");
        assert_eq!(String::from_utf8(vm.stdout).unwrap(), "bar\n");
    }

    #[test]
    fn jump_if_failure_skips_rhs_of_and_list() {
        let mut vm = Vm::default();
        let mut executor = TestExecutor::default();
        let prog = IrProgram::new(vec![
            Ir::ExecuteBuiltin {
                name: "false".into(),
                argv: vec![literal_word("false")],
                redirections: Vec::new(),
            },
            Ir::JumpIfFailure { target: 3 },
            Ir::ExecuteBuiltin {
                name: "echo".into(),
                argv: vec![literal_word("echo"), literal_word("nope")],
                redirections: Vec::new(),
            },
            Ir::ReturnLastStatus,
        ]);
        assert_eq!(vm.run_with_executor(&prog, &mut executor), StepResult::Done(1));
        assert!(vm.stdout.is_empty());
    }

    #[test]
    fn jump_if_success_skips_rhs_of_or_list() {
        let mut vm = Vm::default();
        let mut executor = TestExecutor::default();
        let prog = IrProgram::new(vec![
            Ir::ExecuteBuiltin {
                name: "true".into(),
                argv: vec![literal_word("true")],
                redirections: Vec::new(),
            },
            Ir::JumpIfSuccess { target: 3 },
            Ir::ExecuteBuiltin {
                name: "echo".into(),
                argv: vec![literal_word("echo"), literal_word("nope")],
                redirections: Vec::new(),
            },
            Ir::ReturnLastStatus,
        ]);
        assert_eq!(vm.run_with_executor(&prog, &mut executor), StepResult::Done(0));
        assert!(vm.stdout.is_empty());
    }

    #[test]
    fn executor_receives_redirection_plan() {
        let mut vm = Vm::default();
        let mut executor = TestExecutor::default();
        let prog = IrProgram::new(vec![
            Ir::ExecuteBuiltin {
                name: "echo".into(),
                argv: vec![literal_word("echo"), literal_word("hello")],
                redirections: vec![IrRedirection {
                    fd: None,
                    op: RedirectionOp::Output,
                    target: literal_word("/out.txt"),
                    here_doc_body: None,
                }],
            },
            Ir::ReturnLastStatus,
        ]);
        assert_eq!(vm.run_with_executor(&prog, &mut executor), StepResult::Done(0));
        assert_eq!(executor.seen_redirections.len(), 1);
        assert_eq!(executor.seen_redirections[0][0].op, RedirectionOp::Output);
    }

    fn literal_word(text: &str) -> Word {
        Word {
            parts: vec![WordPart::Literal(text.into())],
            span: Span { start: 0, end: 0 },
        }
    }

    fn parameter_word(name: &str) -> Word {
        Word {
            parts: vec![WordPart::Parameter(name.into())],
            span: Span { start: 0, end: 0 },
        }
    }
}
