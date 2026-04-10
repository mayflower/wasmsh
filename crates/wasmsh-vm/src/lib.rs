//! Cooperative virtual machine for the wasmsh shell.
//!
//! Executes IR instructions with step budgets, yield points,
//! and cancellation tokens. All execution is in-process — no
//! OS processes are spawned.

pub mod pipe;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use wasmsh_builtins::{BuiltinContext, BuiltinRegistry, VecSink as BuiltinSink};
use wasmsh_ir::{Ir, IrProgram};
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

    /// Track output bytes and check the limit. Returns true if within limits.
    pub fn track_output(&mut self, bytes: u64) -> bool {
        self.output_bytes += bytes;
        self.limits.output_byte_limit == 0 || self.output_bytes <= self.limits.output_byte_limit
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
        if self.limits.output_byte_limit > 0 && self.output_bytes > self.limits.output_byte_limit {
            return Err(self.budget_stop(
                StepResult::OutputLimitExceeded,
                format!(
                    "output limit exceeded: {} bytes (limit: {})",
                    self.output_bytes, self.limits.output_byte_limit
                ),
            ));
        }
        Ok(())
    }

    /// Consume one execution step using the VM's shared budget/cancel semantics.
    pub fn begin_step(&mut self) -> Result<(), StepResult> {
        if self.cancel.is_cancelled() {
            return Err(self.budget_stop(StepResult::Cancelled, "execution cancelled".to_string()));
        }
        self.check_output_limit()?;
        if self.limits.step_limit > 0 && self.steps >= self.limits.step_limit {
            return Err(self.budget_stop(
                StepResult::Yield,
                format!(
                    "step budget exhausted: {} steps (limit: {})",
                    self.steps, self.limits.step_limit
                ),
            ));
        }
        self.steps += 1;
        Ok(())
    }

    /// Get the cancellation token (can be cloned and shared).
    #[must_use]
    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    /// Execute an IR program to completion (or until yield/cancel).
    pub fn run(&mut self, program: &IrProgram) -> StepResult {
        let mut pc = 0;
        let mut argv: Vec<String> = Vec::new();
        let instructions = &program.instructions;

        while pc < instructions.len() {
            if let Err(stop) = self.begin_step() {
                return stop;
            }

            match &instructions[pc] {
                Ir::SetVar { name, value } => {
                    self.state.set_var(name.clone(), value.clone());
                }
                Ir::PushArg { value } => {
                    argv.push(value.to_string());
                }
                Ir::CallBuiltin { name } => {
                    if let Some(builtin_fn) = self.builtins.get(name) {
                        let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
                        let mut sink = BuiltinSink::default();
                        let status = {
                            let mut ctx = BuiltinContext {
                                state: &mut self.state,
                                output: &mut sink,
                                fs: None,
                                stdin: None,
                            };
                            builtin_fn(&mut ctx, &argv_refs)
                        };
                        self.write_streams(&sink.stdout, &sink.stderr);
                        self.state.last_status = status;
                    } else {
                        self.emit_diagnostic(
                            DiagLevel::Error,
                            DiagCategory::Builtin,
                            format!("unknown builtin: {name}"),
                        );
                        self.state.last_status = 127;
                    }
                    argv.clear();
                }
                Ir::CallUtility { name: _ } => {
                    // Utility dispatch requires a VFS instance which is managed
                    // by the runtime layer. Set status to 127 (command not found)
                    // at this level; the runtime handles utility dispatch directly.
                    self.state.last_status = 127;
                    argv.clear();
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
            Ir::SetVar {
                name: "FOO".into(),
                value: "bar".into(),
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
            Ir::PushArg {
                value: "echo".into(),
            },
            Ir::PushArg {
                value: "hello".into(),
            },
            Ir::CallBuiltin {
                name: "echo".into(),
            },
            Ir::Return { status: 0 },
        ]);
        assert_eq!(vm.run(&prog), StepResult::Done(0));
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
            Ir::SetVar {
                name: "X".into(),
                value: "1".into(),
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
            },
        );
        vm.write_stdout(b"four");
        assert_eq!(vm.begin_step(), Err(StepResult::OutputLimitExceeded));
        assert!(vm
            .diagnostics
            .iter()
            .any(|d| d.message.contains("output limit exceeded")));
    }
}
