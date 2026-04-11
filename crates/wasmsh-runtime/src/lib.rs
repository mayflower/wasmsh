//! Shared shell runtime core for wasmsh.
//!
//! Platform-agnostic execution engine:
//! `parse -> AST -> HIR -> runtime executor`.
//!
//! Most shell semantics are executed by interpreting HIR directly inside
//! this crate. A bounded subset of top-level `and/or` lists is lowered
//! through `wasmsh-ir` into `wasmsh-vm`, but that is an optimization and
//! parity path rather than the primary executor for the whole grammar.

mod fd_table;

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::{Cursor, ErrorKind, Read};
use std::rc::Rc;

use indexmap::IndexMap;

use crate::fd_table::{ExecIo, InputTarget, OutputTarget};
use wasmsh_ast::{CaseTerminator, RedirectionOp, Word, WordPart};
use wasmsh_expand::expand_words_argv;
use wasmsh_fs::{BackendFs, FileHandle, OpenOptions, Vfs, VfsWriteSink};
use wasmsh_hir::{
    HirAndOr, HirAndOrOp, HirCommand, HirCompleteCommand, HirPipeline, HirProgram, HirRedirection,
};
use wasmsh_ir::{lower_supported_and_or, IrProgram, IrRedirection, LoweringError};
use wasmsh_protocol::{DiagnosticLevel, HostCommand, WorkerEvent, PROTOCOL_VERSION};
use wasmsh_state::ShellState;
use wasmsh_utils::{UtilContext, UtilRegistry};
use wasmsh_vm::pipe::{PipeBuffer, ReadResult, WriteResult};
use wasmsh_vm::{BudgetCategory, ExecutionLimits, ExhaustionReason, StopReason, Vm, VmExecutor};

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
    /// Hostnames/IPs allowed for network access (empty = no network).
    pub allowed_hosts: Vec<String>,
    pub output_byte_limit: u64,
    pub pipe_byte_limit: u64,
    pub recursion_limit: u32,
    pub vm_subset_enabled: bool,
}

impl Default for BrowserConfig {
    fn default() -> Self {
        Self {
            step_budget: 100_000,
            allowed_hosts: Vec::new(),
            output_byte_limit: 0,
            pipe_byte_limit: 0,
            recursion_limit: MAX_RECURSION_DEPTH,
            vm_subset_enabled: true,
        }
    }
}

/// Maximum recursion depth for eval, source, and command substitution.
const MAX_RECURSION_DEPTH: u32 = 100;

/// Transient execution state, reset between top-level commands.
#[derive(Clone)]
#[allow(clippy::struct_excessive_bools)]
struct ExecState {
    break_depth: u32,
    loop_continue: bool,
    exit_requested: Option<i32>,
    errexit_suppressed: bool,
    local_save_stack: Vec<(smol_str::SmolStr, Option<smol_str::SmolStr>)>,
    recursion_depth: u32,
    /// Set when a resource limit (step budget, output limit, cancel) is hit.
    resource_exhausted: bool,
    stop_reason: Option<StopReason>,
    /// Set when word expansion reports a hard semantic error.
    expansion_failed: bool,
    /// Nested output capture scopes for pipelines and substitutions.
    output_captures: Vec<OutputCapture>,
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
            resource_exhausted: false,
            stop_reason: None,
            expansion_failed: false,
            output_captures: Vec::new(),
        }
    }

    fn reset(&mut self) {
        self.break_depth = 0;
        self.loop_continue = false;
        self.exit_requested = None;
        self.errexit_suppressed = false;
        self.resource_exhausted = false;
        self.stop_reason = None;
        self.expansion_failed = false;
        self.output_captures.clear();
    }
}

const STREAMING_YES_MAX_LINES: usize = 65_536;
const PIPEBUFFER_STREAMING_CAPACITY: usize = 1;

#[derive(Clone, Debug, Default)]
struct OutputCapture {
    capture_stdout: bool,
    capture_stderr: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

#[derive(Clone, Debug, Default)]
struct CapturedOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

struct RuntimeOutputRouter<'a> {
    exec: &'a mut ExecState,
    exec_io: Option<&'a mut ExecIo>,
    proc_subst_out_scopes: &'a mut Vec<Vec<PendingProcessSubstOut>>,
    vm_stdout: &'a mut Vec<u8>,
    vm_stderr: &'a mut Vec<u8>,
    vm_output_bytes: &'a mut u64,
    vm_output_limit: u64,
    vm_diagnostics: &'a mut Vec<wasmsh_vm::DiagnosticEvent>,
}

impl RuntimeOutputRouter<'_> {
    fn process_subst_out_sink_mut(&mut self, path: &str) -> Option<&mut PendingProcessSubstOut> {
        for scope in self.proc_subst_out_scopes.iter_mut().rev() {
            if let Some(index) = scope.iter().position(|sink| sink.path == path) {
                return scope.get_mut(index);
            }
        }
        None
    }

    fn append_visible_output_direct(&mut self, data: &[u8], stdout: bool) {
        if stdout {
            self.vm_stdout.extend_from_slice(data);
        } else {
            self.vm_stderr.extend_from_slice(data);
        }
    }

    fn write_output_destination_direct(&mut self, destination: &OutputTarget, data: &[u8]) -> bool {
        match destination {
            OutputTarget::InheritStdout => {
                self.append_visible_output_direct(data, true);
                true
            }
            OutputTarget::InheritStderr => {
                self.append_visible_output_direct(data, false);
                true
            }
            OutputTarget::ProcessSubst { path } => {
                if let Some(sink) = self.process_subst_out_sink_mut(path) {
                    sink.write(data);
                }
                false
            }
            OutputTarget::File { path, sink, .. } => {
                if let Err(err) = sink.borrow_mut().write(data) {
                    let msg = format!("wasmsh: write error: {err}\n");
                    self.append_visible_output_direct(msg.as_bytes(), false);
                    self.vm_diagnostics.push(wasmsh_vm::DiagnosticEvent {
                        level: wasmsh_vm::DiagLevel::Error,
                        category: wasmsh_vm::DiagCategory::Filesystem,
                        message: format!("write failed for {path}: {err}"),
                    });
                }
                false
            }
            OutputTarget::Pipe(pipe) => {
                pipe.borrow_mut().write_all(data);
                false
            }
            OutputTarget::Closed => false,
        }
    }

    fn route_output(&mut self, data: &[u8], stdout: bool) -> bool {
        let mut routed_stdout = stdout;
        if let Some(exec_io) = self.exec_io.as_deref_mut() {
            let destination = exec_io.output_target(stdout);
            match destination {
                OutputTarget::InheritStdout => {
                    routed_stdout = true;
                }
                OutputTarget::InheritStderr => {
                    routed_stdout = false;
                }
                OutputTarget::File { .. }
                | OutputTarget::ProcessSubst { .. }
                | OutputTarget::Pipe(_)
                | OutputTarget::Closed => {
                    return self.write_output_destination_direct(&destination, data);
                }
            }
        }

        for capture in self.exec.output_captures.iter_mut().rev() {
            let should_capture = if routed_stdout {
                capture.capture_stdout
            } else {
                capture.capture_stderr
            };
            if !should_capture {
                continue;
            }
            if routed_stdout {
                capture.stdout.extend_from_slice(data);
            } else {
                capture.stderr.extend_from_slice(data);
            }
            return false;
        }

        self.append_visible_output_direct(data, routed_stdout);
        true
    }

    fn account_output(&mut self, bytes: usize) {
        *self.vm_output_bytes += bytes as u64;
        self.exec.stop_reason = None;
        if self.exec.resource_exhausted {
            return;
        }
        let used = *self.vm_output_bytes;
        if self.vm_output_limit > 0 && used > self.vm_output_limit {
            let reason = ExhaustionReason {
                category: BudgetCategory::VisibleOutputBytes,
                used,
                limit: self.vm_output_limit,
            };
            self.exec.resource_exhausted = true;
            self.exec.stop_reason = Some(StopReason::Exhausted(reason.clone()));
            self.vm_diagnostics.push(wasmsh_vm::DiagnosticEvent {
                level: wasmsh_vm::DiagLevel::Error,
                category: wasmsh_vm::DiagCategory::Budget,
                message: reason.diagnostic_message(),
            });
        }
    }

    fn write_stdout(&mut self, data: &[u8]) {
        if self.route_output(data, true) {
            self.account_output(data.len());
        }
    }

    fn write_stderr(&mut self, data: &[u8]) {
        if self.route_output(data, false) {
            self.account_output(data.len());
        }
    }
}

struct RuntimeBuiltinSink<'a> {
    router: &'a mut RuntimeOutputRouter<'a>,
}

impl wasmsh_builtins::OutputSink for RuntimeBuiltinSink<'_> {
    fn stdout(&mut self, data: &[u8]) {
        self.router.write_stdout(data);
    }

    fn stderr(&mut self, data: &[u8]) {
        self.router.write_stderr(data);
    }
}

struct RuntimeUtilSink<'a> {
    router: &'a mut RuntimeOutputRouter<'a>,
}

impl wasmsh_utils::UtilOutput for RuntimeUtilSink<'_> {
    fn stdout(&mut self, data: &[u8]) {
        self.router.write_stdout(data);
    }

    fn stderr(&mut self, data: &[u8]) {
        self.router.write_stderr(data);
    }
}

fn resolve_path_from_cwd(cwd: &str, path: &str) -> String {
    if path.starts_with('/') {
        wasmsh_fs::normalize_path(path)
    } else {
        wasmsh_fs::normalize_path(&format!("{cwd}/{path}"))
    }
}

struct PipeReader {
    pipe: Rc<RefCell<PipeBuffer>>,
}

impl PipeReader {
    fn new(pipe: Rc<RefCell<PipeBuffer>>) -> Self {
        Self { pipe }
    }
}

impl Read for PipeReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self.pipe.borrow_mut().read(buf) {
            ReadResult::Read(read) => Ok(read),
            ReadResult::WouldBlock => Err(std::io::Error::new(ErrorKind::WouldBlock, "pipe empty")),
            ReadResult::Eof => Ok(0),
        }
    }
}

impl Drop for PipeReader {
    fn drop(&mut self) {
        self.pipe.borrow_mut().close_read();
    }
}

enum PipeProcessPoll {
    Ready,
    PendingRead,
    PendingWrite,
    Exited,
}

struct LiveProcessSubstRunner {
    isolated_runtime: Option<Box<WorkerRuntime>>,
    source_pipe: Rc<RefCell<PipeBuffer>>,
    processes: Vec<StreamingPipeProcess<'static>>,
    finished: Vec<bool>,
    final_pipe: Rc<RefCell<PipeBuffer>>,
    stage_stderr: Vec<Rc<RefCell<Vec<u8>>>>,
    stage_pipe_stderr: Vec<bool>,
    captured_stdout: Vec<u8>,
    captured_stderr: Vec<u8>,
    captured_diagnostics: Vec<wasmsh_vm::DiagnosticEvent>,
    done: bool,
    synced_steps: u64,
}

struct LiveProcessSubstInReader {
    isolated_runtime: Option<Box<WorkerRuntime>>,
    processes: Vec<StreamingPipeProcess<'static>>,
    finished: Vec<bool>,
    final_pipe: Rc<RefCell<PipeBuffer>>,
    stage_stderr: Vec<Rc<RefCell<Vec<u8>>>>,
    stage_pipe_stderr: Vec<bool>,
    flushed_stderr: Rc<RefCell<Vec<u8>>>,
    flushed_diagnostics: Rc<RefCell<Vec<wasmsh_vm::DiagnosticEvent>>>,
    done: bool,
}

impl LiveProcessSubstInReader {
    fn finalize_stderr(&mut self) {
        let mut flushed = self.flushed_stderr.borrow_mut();
        for (idx, stderr) in self.stage_stderr.iter().enumerate() {
            if self.stage_pipe_stderr[idx] {
                continue;
            }
            let data = stderr.borrow();
            if !data.is_empty() {
                flushed.extend_from_slice(&data);
            }
        }
        if let Some(runtime) = self.isolated_runtime.as_mut() {
            self.flushed_diagnostics
                .borrow_mut()
                .extend(runtime.vm.diagnostics.drain(..));
        }
    }

    fn pump(&mut self) -> bool {
        if self.done {
            return false;
        }
        let mut progressed = false;
        if let Some(runtime) = self.isolated_runtime.as_mut() {
            for idx in (0..self.processes.len()).rev() {
                if self.finished[idx] {
                    continue;
                }
                match self.processes[idx].poll(runtime.as_mut()) {
                    PipeProcessPoll::Ready => progressed = true,
                    PipeProcessPoll::PendingRead | PipeProcessPoll::PendingWrite => {}
                    PipeProcessPoll::Exited => {
                        self.finished[idx] = true;
                        progressed = true;
                    }
                }
            }
        } else {
            for idx in (0..self.processes.len()).rev() {
                if self.finished[idx] {
                    continue;
                }
                match self.processes[idx].poll_without_runtime() {
                    PipeProcessPoll::Ready => progressed = true,
                    PipeProcessPoll::PendingRead | PipeProcessPoll::PendingWrite => {}
                    PipeProcessPoll::Exited => {
                        self.finished[idx] = true;
                        progressed = true;
                    }
                }
            }
        }
        if self.finished.iter().all(|done| *done) {
            self.finalize_stderr();
            self.done = true;
        }
        progressed
    }
}

impl Read for LiveProcessSubstInReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            let read_result = {
                let mut pipe = self.final_pipe.borrow_mut();
                pipe.read(buf)
            };
            match read_result {
                ReadResult::Read(read) => return Ok(read),
                ReadResult::Eof if self.done => return Ok(0),
                ReadResult::WouldBlock | ReadResult::Eof => {}
            }

            if !self.pump() {
                if self.done {
                    continue;
                }
                return Err(std::io::Error::new(
                    ErrorKind::WouldBlock,
                    "process substitution pipeline stalled",
                ));
            }
        }
    }
}

impl Drop for LiveProcessSubstInReader {
    fn drop(&mut self) {
        self.final_pipe.borrow_mut().close_read();
        if let Some(runtime) = self.isolated_runtime.as_mut() {
            for process in &mut self.processes {
                process.close(runtime.as_mut());
            }
        } else {
            for process in &mut self.processes {
                process.close_without_runtime();
            }
        }
    }
}

impl LiveProcessSubstRunner {
    fn sync_isolated_runtime_with_parent(&mut self, parent: &mut WorkerRuntime) {
        let Some(runtime) = self.isolated_runtime.as_mut() else {
            return;
        };
        if parent.vm.cancellation_token().is_cancelled() {
            runtime.vm.cancellation_token().cancel();
        }
        let current_steps = runtime.vm.steps;
        if current_steps > self.synced_steps {
            let delta = current_steps - self.synced_steps;
            parent.vm.steps = parent.vm.steps.saturating_add(delta);
            parent.vm.budget.steps = parent.vm.steps;
            self.synced_steps = current_steps;
            if parent.vm.steps > parent.vm.limits.step_limit && parent.vm.limits.step_limit > 0 {
                let reason = ExhaustionReason {
                    category: BudgetCategory::Steps,
                    used: parent.vm.steps,
                    limit: parent.vm.limits.step_limit,
                };
                parent.mark_budget_exhaustion(reason.clone());
                parent.vm.emit_diagnostic(
                    wasmsh_vm::DiagLevel::Error,
                    wasmsh_vm::DiagCategory::Budget,
                    reason.diagnostic_message(),
                );
                runtime.vm.cancellation_token().cancel();
            }
        }
    }

    fn drain_final_pipe(&mut self) -> bool {
        let mut progressed = false;
        loop {
            let mut buffer = [0u8; 4096];
            let read_result = {
                let mut pipe = self.final_pipe.borrow_mut();
                pipe.read(&mut buffer)
            };
            match read_result {
                ReadResult::Read(read) => {
                    self.captured_stdout.extend_from_slice(&buffer[..read]);
                    progressed = true;
                }
                ReadResult::WouldBlock | ReadResult::Eof => break,
            }
        }
        progressed
    }

    fn finalize_stderr(&mut self) {
        for (idx, stderr) in self.stage_stderr.iter().enumerate() {
            if self.stage_pipe_stderr[idx] {
                continue;
            }
            let data = stderr.borrow();
            if !data.is_empty() {
                self.captured_stderr.extend_from_slice(&data);
            }
        }
        if let Some(runtime) = self.isolated_runtime.as_mut() {
            self.captured_diagnostics
                .append(&mut runtime.vm.diagnostics);
        }
    }

    fn pump(&mut self, parent: Option<&mut WorkerRuntime>) -> bool {
        if self.done {
            return false;
        }
        let mut parent = parent;
        let mut progressed = false;
        if self.isolated_runtime.is_some() {
            if let Some(parent_rt) = parent.as_deref_mut() {
                self.sync_isolated_runtime_with_parent(parent_rt);
            }
            let runtime = self
                .isolated_runtime
                .as_mut()
                .expect("isolated process substitution runtime missing");
            for idx in (0..self.processes.len()).rev() {
                if self.finished[idx] {
                    continue;
                }
                match self.processes[idx].poll(runtime.as_mut()) {
                    PipeProcessPoll::Ready => progressed = true,
                    PipeProcessPoll::PendingRead | PipeProcessPoll::PendingWrite => {}
                    PipeProcessPoll::Exited => {
                        self.finished[idx] = true;
                        progressed = true;
                    }
                }
            }
            if let Some(parent_rt) = parent {
                self.sync_isolated_runtime_with_parent(parent_rt);
            }
        } else {
            for idx in (0..self.processes.len()).rev() {
                if self.finished[idx] {
                    continue;
                }
                match self.processes[idx].poll_without_runtime() {
                    PipeProcessPoll::Ready => progressed = true,
                    PipeProcessPoll::PendingRead | PipeProcessPoll::PendingWrite => {}
                    PipeProcessPoll::Exited => {
                        self.finished[idx] = true;
                        progressed = true;
                    }
                }
            }
        }

        if self.drain_final_pipe() {
            progressed = true;
        }

        if self.finished.iter().all(|done| *done) {
            self.finalize_stderr();
            self.done = true;
        }

        progressed
    }

    fn write_input(&mut self, data: &[u8]) {
        let mut offset = 0;
        while offset < data.len() && !self.done {
            let write_result = {
                let mut pipe = self.source_pipe.borrow_mut();
                pipe.write(&data[offset..])
            };
            match write_result {
                WriteResult::Written(written) | WriteResult::WouldBlock(written) if written > 0 => {
                    offset += written;
                    let _ = self.pump(None);
                }
                WriteResult::Written(_) | WriteResult::WouldBlock(_) => {
                    if !self.pump(None) {
                        break;
                    }
                }
                WriteResult::BrokenPipe => {
                    self.source_pipe.borrow_mut().close_write();
                    while self.pump(None) {}
                    break;
                }
            }
        }
    }

    fn write_input_with_parent(&mut self, parent: &mut WorkerRuntime, data: &[u8]) {
        let mut offset = 0;
        while offset < data.len() && !self.done {
            let write_result = {
                let mut pipe = self.source_pipe.borrow_mut();
                pipe.write(&data[offset..])
            };
            match write_result {
                WriteResult::Written(written) | WriteResult::WouldBlock(written) if written > 0 => {
                    offset += written;
                    let _ = self.pump(Some(parent));
                }
                WriteResult::Written(_) | WriteResult::WouldBlock(_) => {
                    if !self.pump(Some(parent)) {
                        break;
                    }
                }
                WriteResult::BrokenPipe => {
                    self.source_pipe.borrow_mut().close_write();
                    while self.pump(Some(parent)) {}
                    break;
                }
            }
        }
    }

    fn finish(&mut self) {
        if self.done {
            return;
        }
        self.source_pipe.borrow_mut().close_write();
        while self.pump(None) {}
        if !self.done {
            self.finalize_stderr();
            self.done = true;
        }
        let _ = self.drain_final_pipe();
    }

    fn finish_with_parent(&mut self, parent: &mut WorkerRuntime) {
        if self.done {
            return;
        }
        self.source_pipe.borrow_mut().close_write();
        while self.pump(Some(parent)) {}
        if !self.done {
            self.finalize_stderr();
            self.done = true;
        }
        self.sync_isolated_runtime_with_parent(parent);
        let _ = self.drain_final_pipe();
    }
}

enum PendingProcessSubstOutMode {
    Buffered { data: Vec<u8> },
    Live { runner: LiveProcessSubstRunner },
}

struct PendingProcessSubstOut {
    path: String,
    inner: String,
    mode: PendingProcessSubstOutMode,
}

impl std::fmt::Debug for PendingProcessSubstOut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingProcessSubstOut")
            .field("path", &self.path)
            .field("inner", &self.inner)
            .finish_non_exhaustive()
    }
}

struct PendingProcessSubstIn {
    path: String,
    stderr: Option<Rc<RefCell<Vec<u8>>>>,
    diagnostics: Option<Rc<RefCell<Vec<wasmsh_vm::DiagnosticEvent>>>>,
}

impl PendingProcessSubstOut {
    fn clear(&mut self) {
        match &mut self.mode {
            PendingProcessSubstOutMode::Buffered { data } => data.clear(),
            PendingProcessSubstOutMode::Live { .. } => {}
        }
    }

    fn write(&mut self, data: &[u8]) {
        match &mut self.mode {
            PendingProcessSubstOutMode::Buffered { data: buffered } => {
                buffered.extend_from_slice(data);
            }
            PendingProcessSubstOutMode::Live { runner } => runner.write_input(data),
        }
    }

    fn write_with_parent(&mut self, runtime: &mut WorkerRuntime, data: &[u8]) {
        match &mut self.mode {
            PendingProcessSubstOutMode::Buffered { data: buffered } => {
                buffered.extend_from_slice(data);
            }
            PendingProcessSubstOutMode::Live { runner } => {
                if runner.isolated_runtime.is_some() {
                    runner.write_input_with_parent(runtime, data);
                } else {
                    runner.write_input(data);
                }
            }
        }
    }
}

#[derive(Clone, Debug)]
enum BufferedPipelineCommand {
    Argv(Vec<String>),
    Hir(HirCommand),
}

enum StreamingPipeProcess<'a> {
    Read(PipeReadProcess<'a>),
    Head(HeadPipeProcess),
    Tee(TeePipeProcess<'a>),
    Buffered(BufferedPipeProcess),
}

impl StreamingPipeProcess<'_> {
    fn poll(&mut self, runtime: &mut WorkerRuntime) -> PipeProcessPoll {
        match self {
            Self::Read(process) => process.poll(),
            Self::Head(process) => process.poll(),
            Self::Tee(process) => process.poll(),
            Self::Buffered(process) => process.poll(runtime),
        }
    }

    fn close(&mut self, runtime: &mut WorkerRuntime) {
        match self {
            Self::Tee(process) => process.close(),
            Self::Buffered(process) => process.close(runtime),
            Self::Read(_) | Self::Head(_) => {}
        }
    }

    fn poll_without_runtime(&mut self) -> PipeProcessPoll {
        match self {
            Self::Read(process) => process.poll(),
            Self::Head(process) => process.poll(),
            Self::Tee(process) => process.poll(),
            Self::Buffered(_) => {
                unreachable!("buffered pipeline stage requires runtime access")
            }
        }
    }

    fn close_without_runtime(&mut self) {
        match self {
            Self::Tee(process) => process.close(),
            Self::Read(_) | Self::Head(_) => {}
            Self::Buffered(_) => {
                unreachable!("buffered pipeline stage requires runtime access")
            }
        }
    }
}

struct BufferedPipeProcess {
    input: Option<Rc<RefCell<PipeBuffer>>>,
    output: Rc<RefCell<PipeBuffer>>,
    command: BufferedPipelineCommand,
    pipe_stderr: bool,
    pending_stdout: Vec<u8>,
    pending_offset: usize,
    finished: bool,
    command_ran: bool,
    stage_stderr: Rc<RefCell<Vec<u8>>>,
    stage_status: Rc<RefCell<i32>>,
    staging_path: Option<String>,
    staging_handle: Option<FileHandle>,
}

impl BufferedPipeProcess {
    fn new(
        input: Option<Rc<RefCell<PipeBuffer>>>,
        output: Rc<RefCell<PipeBuffer>>,
        command: BufferedPipelineCommand,
        pipe_stderr: bool,
        stage_stderr: Rc<RefCell<Vec<u8>>>,
        stage_status: Rc<RefCell<i32>>,
    ) -> Self {
        Self {
            input,
            output,
            command,
            pipe_stderr,
            pending_stdout: Vec::new(),
            pending_offset: 0,
            finished: false,
            command_ran: false,
            stage_stderr,
            stage_status,
            staging_path: None,
            staging_handle: None,
        }
    }

    fn command_label(&self) -> String {
        match &self.command {
            BufferedPipelineCommand::Argv(argv) => argv
                .first()
                .cloned()
                .unwrap_or_else(|| "command".to_string()),
            BufferedPipelineCommand::Hir(cmd) => match cmd {
                HirCommand::Exec(_) => "exec".to_string(),
                HirCommand::Assign(_) => "assign".to_string(),
                HirCommand::RedirectOnly(_) => "redirect".to_string(),
                HirCommand::If(_) => "if".to_string(),
                HirCommand::While(_) => "while".to_string(),
                HirCommand::Until(_) => "until".to_string(),
                HirCommand::For(_) => "for".to_string(),
                HirCommand::Subshell(_) => "subshell".to_string(),
                HirCommand::Group(_) => "group".to_string(),
                HirCommand::FunctionDef(_) => "function".to_string(),
                HirCommand::Case(_) => "case".to_string(),
                HirCommand::DoubleBracket(_) => "[[".to_string(),
                HirCommand::ArithFor(_) => "arith-for".to_string(),
                HirCommand::ArithCommand(_) => "arith".to_string(),
                HirCommand::Select(_) => "select".to_string(),
                _ => "command".to_string(),
            },
        }
    }

    fn ensure_staging_handle(
        &mut self,
        runtime: &mut WorkerRuntime,
    ) -> Result<(String, FileHandle), String> {
        if let (Some(path), Some(handle)) = (&self.staging_path, self.staging_handle) {
            return Ok((path.clone(), handle));
        }
        let path = format!(
            "/tmp/_wasmsh_pipe_{}",
            WorkerRuntime::next_pending_input_id()
        );
        let create_handle = runtime
            .fs
            .open(&path, OpenOptions::write())
            .map_err(|err| err.to_string())?;
        runtime.fs.close(create_handle);
        let handle = runtime
            .fs
            .open(&path, OpenOptions::append())
            .map_err(|err| err.to_string())?;
        self.staging_path = Some(path.clone());
        self.staging_handle = Some(handle);
        Ok((path, handle))
    }

    fn emit_error(
        &mut self,
        runtime: &mut WorkerRuntime,
        cmd_name: &str,
        err: &str,
    ) -> PipeProcessPoll {
        *self.stage_status.borrow_mut() = 1;
        self.stage_stderr.borrow_mut().extend_from_slice(
            format!("wasmsh: {cmd_name}: failed to stage pipeline input for streaming: {err}\n")
                .as_bytes(),
        );
        self.output.borrow_mut().close_write();
        self.close(runtime);
        self.finished = true;
        PipeProcessPoll::Exited
    }

    fn run_command(&mut self, runtime: &mut WorkerRuntime) -> PipeProcessPoll {
        if let Some(handle) = self.staging_handle.take() {
            runtime.fs.close(handle);
        }
        let saved_exec_io = runtime.current_exec_io.take();
        if let Some(path) = self.staging_path.take() {
            runtime.set_pending_input_file(path, true);
        }
        let ((), captured) =
            runtime.with_output_capture(true, self.pipe_stderr, |runtime| match &self.command {
                BufferedPipelineCommand::Argv(argv) => runtime.execute_argv_command(argv),
                BufferedPipelineCommand::Hir(cmd) => runtime.execute_command(cmd),
            });
        *self.stage_status.borrow_mut() = runtime.vm.state.last_status;
        if self.pipe_stderr {
            self.pending_stdout = captured.stdout;
            self.pending_stdout.extend_from_slice(&captured.stderr);
        } else {
            self.pending_stdout = captured.stdout;
            self.stage_stderr
                .borrow_mut()
                .extend_from_slice(&captured.stderr);
        }
        runtime.clear_pending_input();
        runtime.current_exec_io = saved_exec_io;
        self.pending_offset = 0;
        self.command_ran = true;
        if self.pending_stdout.is_empty() {
            self.output.borrow_mut().close_write();
            self.finished = true;
            PipeProcessPoll::Exited
        } else {
            PipeProcessPoll::Ready
        }
    }

    fn close(&mut self, runtime: &mut WorkerRuntime) {
        if let Some(handle) = self.staging_handle.take() {
            runtime.fs.close(handle);
        }
        if let Some(path) = self.staging_path.take() {
            let _ = runtime.fs.remove_file(&path);
        }
    }

    fn poll(&mut self, runtime: &mut WorkerRuntime) -> PipeProcessPoll {
        if self.finished {
            return PipeProcessPoll::Exited;
        }
        if self.pending_offset < self.pending_stdout.len() {
            let write_result = {
                let mut pipe = self.output.borrow_mut();
                pipe.write(&self.pending_stdout[self.pending_offset..])
            };
            return match write_result {
                WriteResult::Written(written) => {
                    self.pending_offset += written;
                    if self.pending_offset == self.pending_stdout.len() {
                        self.pending_stdout.clear();
                        self.pending_offset = 0;
                        if self.command_ran {
                            self.output.borrow_mut().close_write();
                            self.finished = true;
                            return PipeProcessPoll::Exited;
                        }
                    }
                    PipeProcessPoll::Ready
                }
                WriteResult::WouldBlock(0) => PipeProcessPoll::PendingWrite,
                WriteResult::WouldBlock(written) => {
                    self.pending_offset += written;
                    PipeProcessPoll::Ready
                }
                WriteResult::BrokenPipe => {
                    self.output.borrow_mut().close_write();
                    self.finished = true;
                    PipeProcessPoll::Exited
                }
            };
        }

        if self.command_ran {
            self.output.borrow_mut().close_write();
            self.finished = true;
            return PipeProcessPoll::Exited;
        }

        let Some(input) = &self.input else {
            return self.run_command(runtime);
        };
        let cmd_name = self.command_label();
        let mut scratch = [0u8; 4096];
        let read_result = {
            let mut input = input.borrow_mut();
            input.read(&mut scratch)
        };
        match read_result {
            ReadResult::Read(read) => {
                let (_, handle) = match self.ensure_staging_handle(runtime) {
                    Ok(parts) => parts,
                    Err(err) => return self.emit_error(runtime, &cmd_name, &err),
                };
                if let Err(err) = runtime.fs.write_file(handle, &scratch[..read]) {
                    return self.emit_error(runtime, &cmd_name, &err.to_string());
                }
                PipeProcessPoll::Ready
            }
            ReadResult::WouldBlock => PipeProcessPoll::PendingRead,
            ReadResult::Eof => {
                input.borrow_mut().close_read();
                self.run_command(runtime)
            }
        }
    }
}

struct HeadPipeProcess {
    input: Rc<RefCell<PipeBuffer>>,
    output: Rc<RefCell<PipeBuffer>>,
    mode: StreamingHeadMode,
    pending: Vec<u8>,
    pending_offset: usize,
    lines_seen: usize,
    input_closed: bool,
    stream_complete: bool,
    finished: bool,
}

impl HeadPipeProcess {
    fn new(
        input: Rc<RefCell<PipeBuffer>>,
        output: Rc<RefCell<PipeBuffer>>,
        mode: StreamingHeadMode,
    ) -> Self {
        Self {
            input,
            output,
            mode,
            pending: Vec::new(),
            pending_offset: 0,
            lines_seen: 0,
            input_closed: false,
            stream_complete: false,
            finished: false,
        }
    }

    fn close_input(&mut self) {
        if !self.input_closed {
            self.input.borrow_mut().close_read();
            self.input_closed = true;
        }
    }

    fn finish(&mut self) -> PipeProcessPoll {
        self.close_input();
        self.output.borrow_mut().close_write();
        self.finished = true;
        PipeProcessPoll::Exited
    }

    fn poll(&mut self) -> PipeProcessPoll {
        if self.finished {
            return PipeProcessPoll::Exited;
        }
        loop {
            if self.pending_offset < self.pending.len() {
                let write_result = {
                    let mut pipe = self.output.borrow_mut();
                    pipe.write(&self.pending[self.pending_offset..])
                };
                match write_result {
                    WriteResult::Written(written) => {
                        self.pending_offset += written;
                        if self.pending_offset == self.pending.len() {
                            self.pending.clear();
                            self.pending_offset = 0;
                            if self.stream_complete {
                                return self.finish();
                            }
                        }
                        return PipeProcessPoll::Ready;
                    }
                    WriteResult::WouldBlock(0) => return PipeProcessPoll::PendingWrite,
                    WriteResult::WouldBlock(written) => {
                        self.pending_offset += written;
                        return PipeProcessPoll::Ready;
                    }
                    WriteResult::BrokenPipe => return self.finish(),
                }
            }

            if self.stream_complete {
                return self.finish();
            }

            let mut one = [0u8; 1];
            let read_result = {
                let mut input = self.input.borrow_mut();
                input.read(&mut one)
            };
            match read_result {
                ReadResult::Read(read) => {
                    self.pending.extend_from_slice(&one[..read]);
                    match &mut self.mode {
                        StreamingHeadMode::Bytes(remaining) => {
                            *remaining = remaining.saturating_sub(read);
                            if *remaining == 0 {
                                self.stream_complete = true;
                                self.close_input();
                            }
                        }
                        StreamingHeadMode::Lines(limit) => {
                            if one[0] == b'\n' {
                                self.lines_seen += 1;
                                if self.lines_seen >= *limit {
                                    self.stream_complete = true;
                                    self.close_input();
                                }
                            }
                        }
                    }
                }
                ReadResult::WouldBlock => return PipeProcessPoll::PendingRead,
                ReadResult::Eof => {
                    self.stream_complete = true;
                    self.close_input();
                }
            }
        }
    }
}

struct PipeReadProcess<'a> {
    reader: Option<Box<dyn Read + 'a>>,
    output: Rc<RefCell<PipeBuffer>>,
    pending: Vec<u8>,
    pending_offset: usize,
    stderr_offset: usize,
    finished: bool,
    stderr: Rc<RefCell<Vec<u8>>>,
    status: Rc<RefCell<i32>>,
    label: &'static str,
    pipe_stderr: bool,
    reader_done: bool,
}

impl<'a> PipeReadProcess<'a> {
    fn new(
        reader: Box<dyn Read + 'a>,
        output: Rc<RefCell<PipeBuffer>>,
        stderr: Rc<RefCell<Vec<u8>>>,
        status: Rc<RefCell<i32>>,
        label: &'static str,
        pipe_stderr: bool,
    ) -> Self {
        Self {
            reader: Some(reader),
            output,
            pending: Vec::new(),
            pending_offset: 0,
            stderr_offset: 0,
            finished: false,
            stderr,
            status,
            label,
            pipe_stderr,
            reader_done: false,
        }
    }

    fn finish(&mut self) -> PipeProcessPoll {
        self.output.borrow_mut().close_write();
        self.reader = None;
        self.finished = true;
        PipeProcessPoll::Exited
    }

    fn poll_stderr(&mut self) -> Option<PipeProcessPoll> {
        if !self.pipe_stderr {
            return None;
        }
        let len = self.stderr.borrow().len();
        if self.stderr_offset >= len {
            return None;
        }
        let chunk = {
            let stderr = self.stderr.borrow();
            stderr[self.stderr_offset..].to_vec()
        };
        let write_result = {
            let mut output = self.output.borrow_mut();
            output.write(&chunk)
        };
        match write_result {
            WriteResult::Written(written) | WriteResult::WouldBlock(written) if written > 0 => {
                self.stderr_offset += written;
                Some(PipeProcessPoll::Ready)
            }
            WriteResult::Written(_) | WriteResult::WouldBlock(_) => {
                Some(PipeProcessPoll::PendingWrite)
            }
            WriteResult::BrokenPipe => Some(self.finish()),
        }
    }

    fn poll(&mut self) -> PipeProcessPoll {
        if self.finished {
            return PipeProcessPoll::Exited;
        }
        loop {
            if self.pending_offset < self.pending.len() {
                let write_result = {
                    let mut pipe = self.output.borrow_mut();
                    pipe.write(&self.pending[self.pending_offset..])
                };
                match write_result {
                    WriteResult::Written(written) => {
                        self.pending_offset += written;
                        if self.pending_offset == self.pending.len() {
                            self.pending.clear();
                            self.pending_offset = 0;
                        }
                        return PipeProcessPoll::Ready;
                    }
                    WriteResult::WouldBlock(0) => return PipeProcessPoll::PendingWrite,
                    WriteResult::WouldBlock(written) => {
                        self.pending_offset += written;
                        return PipeProcessPoll::Ready;
                    }
                    WriteResult::BrokenPipe => {
                        return self.finish();
                    }
                }
            }

            if let Some(result) = self.poll_stderr() {
                return result;
            }

            if self.reader_done {
                return self.finish();
            }

            let mut buffer = [0u8; 4096];
            let reader = self
                .reader
                .as_mut()
                .expect("pipe read process polled after reader finished");
            match reader.read(&mut buffer) {
                Ok(0) => {
                    self.reader_done = true;
                }
                Ok(read) => {
                    self.pending.extend_from_slice(&buffer[..read]);
                }
                Err(err) if err.kind() == ErrorKind::WouldBlock => {
                    return PipeProcessPoll::PendingRead;
                }
                Err(err) => {
                    *self.status.borrow_mut() = 1;
                    self.stderr.borrow_mut().extend_from_slice(
                        format!(
                            "wasmsh: {}: streaming pipeline read error: {err}\n",
                            self.label
                        )
                        .as_bytes(),
                    );
                    self.reader_done = true;
                }
            }
        }
    }
}

struct TeePipeProcess<'a> {
    reader: Option<Box<dyn Read + 'a>>,
    output: Rc<RefCell<PipeBuffer>>,
    pending: Vec<u8>,
    pending_offset: usize,
    stderr_offset: usize,
    finished: bool,
    stderr: Rc<RefCell<Vec<u8>>>,
    status: Rc<RefCell<i32>>,
    targets: Vec<TeeTarget>,
    pipe_stderr: bool,
    reader_done: bool,
}

impl<'a> TeePipeProcess<'a> {
    fn new(
        reader: Box<dyn Read + 'a>,
        output: Rc<RefCell<PipeBuffer>>,
        fs: &mut BackendFs,
        cwd: &str,
        stage: &StreamingTeeStage,
        stderr: Rc<RefCell<Vec<u8>>>,
        status: Rc<RefCell<i32>>,
        pipe_stderr: bool,
    ) -> Self {
        let mut targets = Vec::new();
        for path in &stage.paths {
            let resolved = resolve_path_from_cwd(cwd, path);
            match fs.open_write_sink(&resolved, stage.append) {
                Ok(sink) => targets.push(TeeTarget {
                    display_path: path.clone(),
                    sink,
                }),
                Err(err) => {
                    stderr
                        .borrow_mut()
                        .extend_from_slice(format!("tee: {path}: {err}\n").as_bytes());
                    *status.borrow_mut() = 1;
                }
            }
        }
        Self {
            reader: Some(reader),
            output,
            pending: Vec::new(),
            pending_offset: 0,
            stderr_offset: 0,
            finished: false,
            stderr,
            status,
            targets,
            pipe_stderr,
            reader_done: false,
        }
    }

    fn close(&mut self) {
        self.reader = None;
        self.targets.clear();
    }

    fn finish(&mut self) -> PipeProcessPoll {
        self.output.borrow_mut().close_write();
        self.close();
        self.finished = true;
        PipeProcessPoll::Exited
    }

    fn write_targets(&mut self, chunk: &[u8]) {
        for target in &mut self.targets {
            if let Err(err) = target.sink.write(chunk) {
                self.stderr
                    .borrow_mut()
                    .extend_from_slice(format!("tee: {}: {err}\n", target.display_path).as_bytes());
                *self.status.borrow_mut() = 1;
            }
        }
    }

    fn poll(&mut self) -> PipeProcessPoll {
        if self.finished {
            return PipeProcessPoll::Exited;
        }
        loop {
            if self.pending_offset < self.pending.len() {
                let write_result = {
                    let mut pipe = self.output.borrow_mut();
                    pipe.write(&self.pending[self.pending_offset..])
                };
                match write_result {
                    WriteResult::Written(written) => {
                        let end = self.pending_offset + written;
                        let chunk = self.pending[self.pending_offset..end].to_vec();
                        self.write_targets(&chunk);
                        self.pending_offset += written;
                        if self.pending_offset == self.pending.len() {
                            self.pending.clear();
                            self.pending_offset = 0;
                        }
                        return PipeProcessPoll::Ready;
                    }
                    WriteResult::WouldBlock(0) => return PipeProcessPoll::PendingWrite,
                    WriteResult::WouldBlock(written) => {
                        let end = self.pending_offset + written;
                        let chunk = self.pending[self.pending_offset..end].to_vec();
                        self.write_targets(&chunk);
                        self.pending_offset += written;
                        return PipeProcessPoll::Ready;
                    }
                    WriteResult::BrokenPipe => {
                        return self.finish();
                    }
                }
            }

            if self.pipe_stderr {
                let len = self.stderr.borrow().len();
                if self.stderr_offset < len {
                    let chunk = {
                        let stderr = self.stderr.borrow();
                        stderr[self.stderr_offset..].to_vec()
                    };
                    let write_result = {
                        let mut output = self.output.borrow_mut();
                        output.write(&chunk)
                    };
                    match write_result {
                        WriteResult::Written(written) | WriteResult::WouldBlock(written)
                            if written > 0 =>
                        {
                            self.stderr_offset += written;
                            return PipeProcessPoll::Ready;
                        }
                        WriteResult::Written(_) | WriteResult::WouldBlock(_) => {
                            return PipeProcessPoll::PendingWrite
                        }
                        WriteResult::BrokenPipe => return self.finish(),
                    }
                }
            }

            if self.reader_done {
                return self.finish();
            }

            let mut buffer = [0u8; 4096];
            let reader = self
                .reader
                .as_mut()
                .expect("tee pipe process polled after reader finished");
            match reader.read(&mut buffer) {
                Ok(0) => {
                    self.reader_done = true;
                }
                Ok(read) => self.pending.extend_from_slice(&buffer[..read]),
                Err(err) if err.kind() == ErrorKind::WouldBlock => {
                    return PipeProcessPoll::PendingRead;
                }
                Err(err) => {
                    *self.status.borrow_mut() = 1;
                    self.stderr.borrow_mut().extend_from_slice(
                        format!("wasmsh: tee: streaming pipeline read error: {err}\n").as_bytes(),
                    );
                    self.reader_done = true;
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum StreamingHeadMode {
    Lines(usize),
    Bytes(usize),
}

#[derive(Clone, Copy, Debug)]
enum StreamingTailMode {
    Lines(usize),
    Bytes(usize),
}

struct YesStreamReader {
    line: Vec<u8>,
    offset: usize,
    remaining_lines: usize,
}

impl YesStreamReader {
    fn new(line: Vec<u8>, remaining_lines: usize) -> Self {
        Self {
            line,
            offset: 0,
            remaining_lines,
        }
    }
}

impl Read for YesStreamReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() || self.line.is_empty() || self.remaining_lines == 0 {
            return Ok(0);
        }
        let mut written = 0usize;
        while written < buf.len() && self.remaining_lines > 0 {
            let remaining_line = &self.line[self.offset..];
            let to_copy = remaining_line.len().min(buf.len() - written);
            buf[written..written + to_copy].copy_from_slice(&remaining_line[..to_copy]);
            written += to_copy;
            self.offset += to_copy;
            if self.offset == self.line.len() {
                self.offset = 0;
                self.remaining_lines = self.remaining_lines.saturating_sub(1);
            }
        }
        Ok(written)
    }
}

struct HeadStreamReader<R> {
    inner: R,
    mode: StreamingHeadMode,
    finished: bool,
    pending: Vec<u8>,
    pending_offset: usize,
    lines_seen: usize,
}

struct TailStreamReader<R> {
    inner: R,
    mode: StreamingTailMode,
    output_pending: Vec<u8>,
    output_offset: usize,
    finalized: bool,
    byte_ring: VecDeque<u8>,
    line_ring: VecDeque<Vec<u8>>,
    current_line: Vec<u8>,
}

#[derive(Clone, Copy, Debug)]
struct StreamingBatStage {
    show_numbers: bool,
    show_header: bool,
    line_range: Option<(Option<usize>, Option<usize>)>,
    show_all: bool,
}

struct BatStreamReader<R> {
    inner: R,
    stage: StreamingBatStage,
    input_pending: Vec<u8>,
    output_pending: Vec<u8>,
    output_offset: usize,
    finished: bool,
    header_emitted: bool,
    footer_emitted: bool,
    line_num: usize,
}

#[derive(Clone, Debug)]
struct StreamingSedSubstitute {
    pattern: String,
    replacement: String,
    global: bool,
}

#[derive(Clone, Debug)]
enum StreamingSedAddr {
    None,
    Line(usize),
    Last,
    Regex(String),
    Range(Box<StreamingSedAddr>, Box<StreamingSedAddr>),
}

#[derive(Clone, Debug)]
enum StreamingSedCmd {
    Substitute(StreamingSedSubstitute),
    Delete,
    Print,
    Transliterate(Vec<char>, Vec<char>),
    AppendText(String),
    InsertText(String),
    ChangeText(String),
    Quit,
}

#[derive(Clone, Debug)]
struct StreamingSedInstruction {
    addr: StreamingSedAddr,
    cmd: StreamingSedCmd,
}

#[derive(Clone, Debug)]
struct StreamingSedStage {
    suppress_print: bool,
    instructions: Vec<StreamingSedInstruction>,
}

struct SedStreamReader<R> {
    inner: R,
    stage: StreamingSedStage,
    input_pending: Vec<u8>,
    output_pending: Vec<u8>,
    output_offset: usize,
    initialized: bool,
    finished: bool,
    current: Option<(String, bool)>,
    next: Option<(String, bool)>,
    line_num: usize,
    range_states: Vec<bool>,
    input_eof: bool,
}

#[derive(Clone, Debug)]
struct StreamingPasteStage {
    delimiter: String,
    serial: bool,
}

struct PasteStreamReader<R> {
    inner: R,
    stage: StreamingPasteStage,
    input_pending: Vec<u8>,
    output_pending: Vec<u8>,
    output_offset: usize,
    finalized: bool,
    ended_with_newline: bool,
    serial_first: bool,
}

#[derive(Clone, Copy, Debug)]
struct StreamingColumnStage;

struct ColumnStreamReader<R> {
    inner: R,
    output_pending: Vec<u8>,
    output_offset: usize,
    finalized: bool,
    ended_with_newline: bool,
}

#[derive(Clone, Debug)]
struct StreamingTeeStage {
    append: bool,
    paths: Vec<String>,
}

struct TeeTarget {
    display_path: String,
    sink: Box<dyn VfsWriteSink>,
}

#[derive(Clone, Copy, Debug)]
#[allow(clippy::struct_excessive_bools)]
struct StreamingWcFlags {
    lines: bool,
    words: bool,
    bytes: bool,
    max_line_length: bool,
}

#[allow(clippy::struct_excessive_bools)]
struct WcStreamReader<R> {
    inner: R,
    flags: StreamingWcFlags,
    summary: Vec<u8>,
    summary_offset: usize,
    finalized: bool,
    lines: usize,
    words: usize,
    bytes: usize,
    max_line_length: usize,
    current_line_length: usize,
    in_word: bool,
    saw_input: bool,
    ended_with_newline: bool,
}

#[derive(Clone, Debug)]
#[allow(clippy::struct_excessive_bools)]
struct StreamingGrepFlags {
    ignore_case: bool,
    invert: bool,
    count_only: bool,
    show_line_numbers: bool,
    files_only: bool,
    word_match: bool,
    only_matching: bool,
    quiet: bool,
    extended: bool,
    fixed: bool,
    after_context: usize,
    before_context: usize,
    max_count: Option<usize>,
    show_filename: Option<bool>,
}

#[derive(Clone, Debug)]
struct StreamingGrepStage {
    flags: StreamingGrepFlags,
    patterns: Vec<String>,
}

#[derive(Clone, Debug)]
#[allow(clippy::struct_excessive_bools)]
struct StreamingUniqFlags {
    count: bool,
    duplicates_only: bool,
    unique_only: bool,
    ignore_case: bool,
    skip_fields: usize,
    skip_chars: usize,
    compare_chars: Option<usize>,
}

impl<R> WcStreamReader<R> {
    fn new(inner: R, flags: StreamingWcFlags) -> Self {
        Self {
            inner,
            flags,
            summary: Vec::new(),
            summary_offset: 0,
            finalized: false,
            lines: 0,
            words: 0,
            bytes: 0,
            max_line_length: 0,
            current_line_length: 0,
            in_word: false,
            saw_input: false,
            ended_with_newline: false,
        }
    }

    fn take_summary(&mut self, buf: &mut [u8]) -> usize {
        if self.summary_offset >= self.summary.len() {
            return 0;
        }
        let remaining = &self.summary[self.summary_offset..];
        let to_copy = remaining.len().min(buf.len());
        buf[..to_copy].copy_from_slice(&remaining[..to_copy]);
        self.summary_offset += to_copy;
        to_copy
    }

    fn process_chunk(&mut self, chunk: &[u8]) {
        if chunk.is_empty() {
            return;
        }
        self.saw_input = true;
        self.bytes += chunk.len();
        for &byte in chunk {
            let is_whitespace = byte.is_ascii_whitespace();
            if is_whitespace {
                self.in_word = false;
            } else if !self.in_word {
                self.words += 1;
                self.in_word = true;
            }

            if byte == b'\n' {
                self.lines += 1;
                self.max_line_length = self.max_line_length.max(self.current_line_length);
                self.current_line_length = 0;
                self.ended_with_newline = true;
            } else {
                self.current_line_length += 1;
                self.ended_with_newline = false;
            }
        }
    }

    fn finalize_summary(&mut self) {
        if self.finalized {
            return;
        }
        self.finalized = true;
        if self.saw_input && !self.ended_with_newline {
            self.lines += 1;
            self.max_line_length = self.max_line_length.max(self.current_line_length);
        }

        let mut parts = Vec::new();
        if self.flags.lines {
            parts.push(format!("{:>7}", self.lines));
        }
        if self.flags.words {
            parts.push(format!("{:>7}", self.words));
        }
        if self.flags.bytes {
            parts.push(format!("{:>7}", self.bytes));
        }
        if self.flags.max_line_length {
            parts.push(format!("{:>7}", self.max_line_length));
        }
        let mut output = parts.join("");
        output.push('\n');
        self.summary = output.into_bytes();
    }
}

impl<R: Read> Read for WcStreamReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let copied = self.take_summary(buf);
        if copied > 0 {
            return Ok(copied);
        }
        if self.finalized {
            return Ok(0);
        }

        let mut scratch = [0u8; 4096];
        loop {
            let read = self.inner.read(&mut scratch)?;
            if read == 0 {
                self.finalize_summary();
                return Ok(self.take_summary(buf));
            }
            self.process_chunk(&scratch[..read]);
        }
    }
}

impl<R> HeadStreamReader<R> {
    fn new(inner: R, mode: StreamingHeadMode) -> Self {
        Self {
            inner,
            mode,
            finished: false,
            pending: Vec::new(),
            pending_offset: 0,
            lines_seen: 0,
        }
    }

    fn take_from_pending(&mut self, buf: &mut [u8]) -> usize {
        if self.pending_offset >= self.pending.len() {
            self.pending.clear();
            self.pending_offset = 0;
            return 0;
        }
        let remaining = &self.pending[self.pending_offset..];
        let to_copy = remaining.len().min(buf.len());
        buf[..to_copy].copy_from_slice(&remaining[..to_copy]);
        self.pending_offset += to_copy;
        if self.pending_offset == self.pending.len() {
            self.pending.clear();
            self.pending_offset = 0;
        }
        to_copy
    }
}

impl<R: Read> Read for HeadStreamReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let copied = self.take_from_pending(buf);
        if copied > 0 {
            return Ok(copied);
        }
        if self.finished {
            return Ok(0);
        }

        match self.mode {
            StreamingHeadMode::Bytes(ref mut remaining) => {
                if *remaining == 0 {
                    self.finished = true;
                    return Ok(0);
                }
                let to_read = (*remaining).min(buf.len());
                let read = self.inner.read(&mut buf[..to_read])?;
                *remaining = remaining.saturating_sub(read);
                if read == 0 || *remaining == 0 {
                    self.finished = read == 0 || *remaining == 0;
                }
                Ok(read)
            }
            StreamingHeadMode::Lines(limit) => {
                if self.lines_seen >= limit {
                    self.finished = true;
                    return Ok(0);
                }
                let mut produced = 0usize;
                let mut one = [0u8; 1];
                while produced < buf.len() && self.lines_seen < limit {
                    let read = match self.inner.read(&mut one) {
                        Ok(read) => read,
                        Err(err) if err.kind() == ErrorKind::WouldBlock && produced > 0 => {
                            return Ok(produced);
                        }
                        Err(err) => return Err(err),
                    };
                    if read == 0 {
                        self.finished = true;
                        break;
                    }
                    let byte = one[0];
                    buf[produced] = byte;
                    produced += 1;
                    if byte == b'\n' {
                        self.lines_seen += 1;
                    }
                }
                if self.lines_seen >= limit {
                    self.finished = true;
                }
                Ok(produced)
            }
        }
    }
}

impl<R> TailStreamReader<R> {
    fn new(inner: R, mode: StreamingTailMode) -> Self {
        Self {
            inner,
            mode,
            output_pending: Vec::new(),
            output_offset: 0,
            finalized: false,
            byte_ring: VecDeque::new(),
            line_ring: VecDeque::new(),
            current_line: Vec::new(),
        }
    }

    fn push_tail_byte(&mut self, byte: u8) {
        let StreamingTailMode::Bytes(limit) = self.mode else {
            return;
        };
        if limit == 0 {
            return;
        }
        if self.byte_ring.len() == limit {
            self.byte_ring.pop_front();
        }
        self.byte_ring.push_back(byte);
    }

    fn push_tail_line(&mut self, line: Vec<u8>) {
        let StreamingTailMode::Lines(limit) = self.mode else {
            return;
        };
        if limit == 0 {
            return;
        }
        if self.line_ring.len() == limit {
            self.line_ring.pop_front();
        }
        self.line_ring.push_back(line);
    }

    fn process_chunk(&mut self, chunk: &[u8]) {
        match self.mode {
            StreamingTailMode::Bytes(_) => {
                for &byte in chunk {
                    self.push_tail_byte(byte);
                }
            }
            StreamingTailMode::Lines(_) => {
                for &byte in chunk {
                    if byte == b'\n' {
                        let line = std::mem::take(&mut self.current_line);
                        self.push_tail_line(line);
                    } else {
                        self.current_line.push(byte);
                    }
                }
            }
        }
    }

    fn finalize_output(&mut self) {
        if self.finalized {
            return;
        }
        match self.mode {
            StreamingTailMode::Bytes(_) => {
                self.output_pending.extend(self.byte_ring.drain(..));
            }
            StreamingTailMode::Lines(_) => {
                if !self.current_line.is_empty() {
                    let line = std::mem::take(&mut self.current_line);
                    self.push_tail_line(line);
                }
                for line in self.line_ring.drain(..) {
                    self.output_pending.extend_from_slice(&line);
                    self.output_pending.push(b'\n');
                }
            }
        }
        self.finalized = true;
    }
}

impl<R: Read> Read for TailStreamReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let copied = take_pending_output(&mut self.output_pending, &mut self.output_offset, buf);
        if copied > 0 {
            return Ok(copied);
        }
        if self.finalized {
            return Ok(0);
        }
        loop {
            let mut scratch = [0u8; 4096];
            match self.inner.read(&mut scratch) {
                Ok(0) => {
                    self.finalize_output();
                    return Ok(take_pending_output(
                        &mut self.output_pending,
                        &mut self.output_offset,
                        buf,
                    ));
                }
                Ok(read) => self.process_chunk(&scratch[..read]),
                Err(err) => return Err(err),
            }
        }
    }
}

fn streaming_bat_in_range(line_num: usize, range: Option<(Option<usize>, Option<usize>)>) -> bool {
    let Some((start, end)) = range else {
        return true;
    };
    if start.is_some_and(|s| line_num < s) {
        return false;
    }
    end.is_none_or(|e| line_num <= e)
}

fn streaming_make_visible(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if ch == '\t' {
            out.push_str("\\t");
        } else if ch == '\r' {
            out.push_str("\\r");
        } else if ch.is_control() {
            let _ = std::fmt::Write::write_fmt(&mut out, format_args!("\\x{:02x}", ch as u32));
        } else {
            out.push(ch);
        }
    }
    out
}

impl<R> BatStreamReader<R> {
    fn new(inner: R, stage: StreamingBatStage) -> Self {
        Self {
            inner,
            stage,
            input_pending: Vec::new(),
            output_pending: Vec::new(),
            output_offset: 0,
            finished: false,
            header_emitted: false,
            footer_emitted: false,
            line_num: 0,
        }
    }

    fn emit_header(&mut self) {
        if !self.stage.show_header || self.header_emitted {
            return;
        }
        self.header_emitted = true;
        let separator = "\u{2500}";
        let rule_left: String = separator.repeat(7);
        let rule_right: String = separator.repeat(20);
        let top_corner = "\u{252C}";
        let mid_corner = "\u{253C}";
        self.output_pending
            .extend_from_slice(format!("{rule_left}{top_corner}{rule_right}\n").as_bytes());
        self.output_pending
            .extend_from_slice(format!("{rule_left}{mid_corner}{rule_right}\n").as_bytes());
    }

    fn emit_footer(&mut self) {
        if !self.stage.show_header || self.footer_emitted {
            return;
        }
        self.footer_emitted = true;
        let separator = "\u{2500}";
        let rule_left: String = separator.repeat(7);
        let rule_right: String = separator.repeat(20);
        let bot_corner = "\u{2534}";
        self.output_pending
            .extend_from_slice(format!("{rule_left}{bot_corner}{rule_right}\n").as_bytes());
    }
}

impl<R: Read> Read for BatStreamReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            let copied =
                take_pending_output(&mut self.output_pending, &mut self.output_offset, buf);
            if copied > 0 {
                return Ok(copied);
            }
            if self.finished {
                return Ok(0);
            }

            self.emit_header();
            let copied =
                take_pending_output(&mut self.output_pending, &mut self.output_offset, buf);
            if copied > 0 {
                return Ok(copied);
            }

            if let Some((line, _had_newline)) =
                streaming_read_next_line(&mut self.inner, &mut self.input_pending)?
            {
                self.line_num += 1;
                if !streaming_bat_in_range(self.line_num, self.stage.line_range) {
                    continue;
                }
                let display_line = if self.stage.show_all {
                    streaming_make_visible(&line)
                } else {
                    line
                };
                if self.stage.show_numbers {
                    self.output_pending.extend_from_slice(
                        format!("{:>5}   \u{2502} {display_line}\n", self.line_num).as_bytes(),
                    );
                } else {
                    self.output_pending
                        .extend_from_slice(format!("{display_line}\n").as_bytes());
                }
            } else {
                self.emit_footer();
                self.finished = true;
            }
        }
    }
}

fn streaming_simple_grep_match(line: &str, pattern: &str) -> bool {
    if let Some(rest) = pattern.strip_prefix('^') {
        if let Some(mid) = rest.strip_suffix('$') {
            line == mid
        } else {
            line.starts_with(rest)
        }
    } else if let Some(rest) = pattern.strip_suffix('$') {
        line.ends_with(rest)
    } else {
        line.contains(pattern)
    }
}

fn parse_streaming_sed_substitute(expr: &str) -> Option<StreamingSedSubstitute> {
    if !expr.starts_with('s') || expr.len() < 4 {
        return None;
    }
    let delim = expr.as_bytes()[1] as char;
    let rest = &expr[2..];
    let parts: Vec<&str> = rest.split(delim).collect();
    if parts.len() < 2 {
        return None;
    }
    Some(StreamingSedSubstitute {
        pattern: parts[0].to_string(),
        replacement: parts[1].to_string(),
        global: parts.get(2).is_some_and(|flags| flags.contains('g')),
    })
}

fn parse_streaming_sed_addr(s: &str) -> (StreamingSedAddr, &str) {
    if let Some(stripped) = s.strip_prefix('/') {
        if let Some(end) = stripped.find('/') {
            let pat = &stripped[..end];
            let rest = &stripped[end + 1..];
            if let Some(after_comma) = rest.strip_prefix(',') {
                let (addr2, rest2) = parse_streaming_sed_addr(after_comma);
                return (
                    StreamingSedAddr::Range(
                        Box::new(StreamingSedAddr::Regex(pat.to_string())),
                        Box::new(addr2),
                    ),
                    rest2,
                );
            }
            return (StreamingSedAddr::Regex(pat.to_string()), rest);
        }
    }
    if let Some(rest) = s.strip_prefix('$') {
        if let Some(after_comma) = rest.strip_prefix(',') {
            let (addr2, rest2) = parse_streaming_sed_addr(after_comma);
            return (
                StreamingSedAddr::Range(Box::new(StreamingSedAddr::Last), Box::new(addr2)),
                rest2,
            );
        }
        return (StreamingSedAddr::Last, rest);
    }
    let num_end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    if num_end > 0 {
        if let Ok(n) = s[..num_end].parse::<usize>() {
            let rest = &s[num_end..];
            if let Some(after_comma) = rest.strip_prefix(',') {
                let (addr2, rest2) = parse_streaming_sed_addr(after_comma);
                return (
                    StreamingSedAddr::Range(Box::new(StreamingSedAddr::Line(n)), Box::new(addr2)),
                    rest2,
                );
            }
            return (StreamingSedAddr::Line(n), rest);
        }
    }
    (StreamingSedAddr::None, s)
}

fn parse_streaming_sed_script(script: &str) -> Vec<StreamingSedInstruction> {
    let mut instructions = Vec::new();
    for part in script.split(';') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (addr, rest) = parse_streaming_sed_addr(part);
        let rest = rest.trim();
        let cmd = if rest.starts_with('s') {
            if let Some(sub) = parse_streaming_sed_substitute(rest) {
                StreamingSedCmd::Substitute(sub)
            } else {
                continue;
            }
        } else if rest == "d" {
            StreamingSedCmd::Delete
        } else if rest == "p" {
            StreamingSedCmd::Print
        } else if rest == "q" {
            StreamingSedCmd::Quit
        } else if rest.starts_with("y/") || rest.starts_with("y|") {
            let delim = rest.as_bytes()[1] as char;
            let inner = &rest[2..];
            let parts: Vec<&str> = inner.split(delim).collect();
            if parts.len() >= 2 {
                StreamingSedCmd::Transliterate(
                    parts[0].chars().collect(),
                    parts[1].chars().collect(),
                )
            } else {
                continue;
            }
        } else if let Some(text) = rest.strip_prefix("a\\") {
            StreamingSedCmd::AppendText(text.trim_start().to_string())
        } else if let Some(text) = rest.strip_prefix("i\\") {
            StreamingSedCmd::InsertText(text.trim_start().to_string())
        } else if let Some(text) = rest.strip_prefix("c\\") {
            StreamingSedCmd::ChangeText(text.trim_start().to_string())
        } else {
            continue;
        };
        instructions.push(StreamingSedInstruction { addr, cmd });
    }
    instructions
}

fn streaming_sed_addr_matches(
    addr: &StreamingSedAddr,
    line_num: usize,
    is_last: bool,
    line: &str,
    in_range: &mut bool,
) -> bool {
    match addr {
        StreamingSedAddr::None => true,
        StreamingSedAddr::Line(n) => line_num == *n,
        StreamingSedAddr::Last => is_last,
        StreamingSedAddr::Regex(pat) => streaming_simple_grep_match(line, pat),
        StreamingSedAddr::Range(start, end) => {
            if *in_range {
                if streaming_sed_addr_matches(end, line_num, is_last, line, &mut false) {
                    *in_range = false;
                }
                true
            } else if streaming_sed_addr_matches(start, line_num, is_last, line, &mut false) {
                *in_range = true;
                true
            } else {
                false
            }
        }
    }
}

fn streaming_sed_emit_line(output: &mut Vec<u8>, line: &str) {
    output.extend_from_slice(line.as_bytes());
    output.push(b'\n');
}

impl<R> SedStreamReader<R> {
    fn new(inner: R, stage: StreamingSedStage) -> Self {
        let range_states = vec![false; stage.instructions.len()];
        Self {
            inner,
            stage,
            input_pending: Vec::new(),
            output_pending: Vec::new(),
            output_offset: 0,
            initialized: false,
            finished: false,
            current: None,
            next: None,
            line_num: 1,
            range_states,
            input_eof: false,
        }
    }

    fn fill_lookahead(&mut self) -> std::io::Result<()>
    where
        R: Read,
    {
        if self.current.is_none() {
            self.current = streaming_read_next_line(&mut self.inner, &mut self.input_pending)?;
        }
        if self.current.is_some() && self.next.is_none() && !self.input_eof {
            match streaming_read_next_line(&mut self.inner, &mut self.input_pending)? {
                Some(line) => self.next = Some(line),
                None => self.input_eof = true,
            }
        }
        Ok(())
    }

    fn initialize(&mut self) -> std::io::Result<()>
    where
        R: Read,
    {
        if self.initialized {
            return Ok(());
        }
        self.fill_lookahead()?;
        self.initialized = true;
        Ok(())
    }
}

impl<R: Read> Read for SedStreamReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            let copied =
                take_pending_output(&mut self.output_pending, &mut self.output_offset, buf);
            if copied > 0 {
                return Ok(copied);
            }
            if self.finished {
                return Ok(0);
            }

            self.initialize()?;
            self.fill_lookahead()?;
            let Some((line, _had_newline)) = self.current.take() else {
                self.finished = true;
                continue;
            };

            let is_last = self.input_eof && self.next.is_none();
            let mut current_text = line;
            let mut deleted = false;
            let mut printed = false;
            let mut quit = false;

            for (idx, instr) in self.stage.instructions.iter().enumerate() {
                if !streaming_sed_addr_matches(
                    &instr.addr,
                    self.line_num,
                    is_last,
                    &current_text,
                    &mut self.range_states[idx],
                ) {
                    continue;
                }
                match &instr.cmd {
                    StreamingSedCmd::Substitute(sub) => {
                        current_text = if sub.global {
                            current_text.replace(&sub.pattern, &sub.replacement)
                        } else {
                            current_text.replacen(&sub.pattern, &sub.replacement, 1)
                        };
                    }
                    StreamingSedCmd::Delete => {
                        deleted = true;
                        break;
                    }
                    StreamingSedCmd::Print => {
                        streaming_sed_emit_line(&mut self.output_pending, &current_text);
                        printed = true;
                    }
                    StreamingSedCmd::Transliterate(from, to) => {
                        current_text = current_text
                            .chars()
                            .map(|c| {
                                if let Some(pos) = from.iter().position(|&fc| fc == c) {
                                    to.get(pos).or(to.last()).copied().unwrap_or(c)
                                } else {
                                    c
                                }
                            })
                            .collect();
                    }
                    StreamingSedCmd::AppendText(text) => {
                        if !self.stage.suppress_print && !printed {
                            streaming_sed_emit_line(&mut self.output_pending, &current_text);
                            printed = true;
                        }
                        streaming_sed_emit_line(&mut self.output_pending, text);
                    }
                    StreamingSedCmd::InsertText(text) => {
                        streaming_sed_emit_line(&mut self.output_pending, text);
                    }
                    StreamingSedCmd::ChangeText(text) => {
                        streaming_sed_emit_line(&mut self.output_pending, text);
                        deleted = true;
                        printed = true;
                        break;
                    }
                    StreamingSedCmd::Quit => {
                        quit = true;
                        break;
                    }
                }
            }

            if !deleted && !self.stage.suppress_print && !printed {
                streaming_sed_emit_line(&mut self.output_pending, &current_text);
            }

            if quit {
                self.finished = true;
            } else {
                self.current = self.next.take();
                if self.current.is_some() {
                    self.line_num += 1;
                    self.fill_lookahead()?;
                } else {
                    self.finished = true;
                }
            }
        }
    }
}

impl<R> PasteStreamReader<R> {
    fn new(inner: R, stage: StreamingPasteStage) -> Self {
        Self {
            inner,
            stage,
            input_pending: Vec::new(),
            output_pending: Vec::new(),
            output_offset: 0,
            finalized: false,
            ended_with_newline: true,
            serial_first: true,
        }
    }

    fn finalize_serial(&mut self) -> std::io::Result<()>
    where
        R: Read,
    {
        while let Some((line, _had_newline)) =
            streaming_read_next_line(&mut self.inner, &mut self.input_pending)?
        {
            if !self.serial_first {
                self.output_pending
                    .extend_from_slice(self.stage.delimiter.as_bytes());
            }
            self.output_pending.extend_from_slice(line.as_bytes());
            self.serial_first = false;
        }
        self.output_pending.push(b'\n');
        Ok(())
    }
}

impl<R: Read> Read for PasteStreamReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            let copied =
                take_pending_output(&mut self.output_pending, &mut self.output_offset, buf);
            if copied > 0 {
                return Ok(copied);
            }
            if self.finalized {
                return Ok(0);
            }

            if self.stage.serial {
                self.finalize_serial()?;
                self.finalized = true;
                continue;
            }

            let mut scratch = [0u8; 4096];
            let read = self.inner.read(&mut scratch)?;
            if read == 0 {
                if !self.ended_with_newline {
                    self.output_pending.push(b'\n');
                }
                self.finalized = true;
                continue;
            }
            self.ended_with_newline = scratch[read - 1] == b'\n';
            self.output_pending.extend_from_slice(&scratch[..read]);
        }
    }
}

impl<R> ColumnStreamReader<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            output_pending: Vec::new(),
            output_offset: 0,
            finalized: false,
            ended_with_newline: true,
        }
    }
}

impl<R: Read> Read for ColumnStreamReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            let copied =
                take_pending_output(&mut self.output_pending, &mut self.output_offset, buf);
            if copied > 0 {
                return Ok(copied);
            }
            if self.finalized {
                return Ok(0);
            }

            let mut scratch = [0u8; 4096];
            let read = self.inner.read(&mut scratch)?;
            if read == 0 {
                if !self.ended_with_newline {
                    self.output_pending.push(b'\n');
                }
                self.finalized = true;
                continue;
            }
            self.ended_with_newline = scratch[read - 1] == b'\n';
            self.output_pending.extend_from_slice(&scratch[..read]);
        }
    }
}

fn take_pending_output(pending: &mut Vec<u8>, pending_offset: &mut usize, buf: &mut [u8]) -> usize {
    if *pending_offset >= pending.len() {
        pending.clear();
        *pending_offset = 0;
        return 0;
    }
    let remaining = &pending[*pending_offset..];
    let to_copy = remaining.len().min(buf.len());
    buf[..to_copy].copy_from_slice(&remaining[..to_copy]);
    *pending_offset += to_copy;
    if *pending_offset == pending.len() {
        pending.clear();
        *pending_offset = 0;
    }
    to_copy
}

fn streaming_read_next_line(
    reader: &mut dyn Read,
    pending: &mut Vec<u8>,
) -> std::io::Result<Option<(String, bool)>> {
    loop {
        if let Some(pos) = pending.iter().position(|&b| b == b'\n') {
            let mut line = pending.drain(..=pos).collect::<Vec<u8>>();
            let _ = line.pop();
            return Ok(Some((String::from_utf8_lossy(&line).to_string(), true)));
        }

        let mut buffer = [0u8; 4096];
        match reader.read(&mut buffer) {
            Ok(0) => {
                if pending.is_empty() {
                    return Ok(None);
                }
                let line = std::mem::take(pending);
                return Ok(Some((String::from_utf8_lossy(&line).to_string(), false)));
            }
            Ok(read) => pending.extend_from_slice(&buffer[..read]),
            Err(err) => return Err(err),
        }
    }
}

struct RevStreamReader<R> {
    inner: R,
    input_pending: Vec<u8>,
    output_pending: Vec<u8>,
    output_offset: usize,
    finished: bool,
}

impl<R> RevStreamReader<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            input_pending: Vec::new(),
            output_pending: Vec::new(),
            output_offset: 0,
            finished: false,
        }
    }
}

impl<R: Read> Read for RevStreamReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let copied = take_pending_output(&mut self.output_pending, &mut self.output_offset, buf);
        if copied > 0 {
            return Ok(copied);
        }
        if self.finished {
            return Ok(0);
        }

        if let Some((line, _had_newline)) =
            streaming_read_next_line(&mut self.inner, &mut self.input_pending)?
        {
            let reversed: String = line.chars().rev().collect();
            self.output_pending.extend_from_slice(reversed.as_bytes());
            self.output_pending.push(b'\n');
            Ok(take_pending_output(
                &mut self.output_pending,
                &mut self.output_offset,
                buf,
            ))
        } else {
            self.finished = true;
            Ok(0)
        }
    }
}

fn streaming_cut_range_includes(ranges: &[StreamingCutRange], idx: usize) -> bool {
    ranges.iter().any(|range| {
        let start = range.start.unwrap_or(1);
        let end = range.end.unwrap_or(usize::MAX);
        idx >= start && idx <= end
    })
}

fn apply_streaming_cut(line: &str, stage: &StreamingCutStage) -> Option<Vec<u8>> {
    match &stage.mode {
        StreamingCutMode::Fields(ranges) => {
            if stage.only_delimited && !line.contains(stage.delim) {
                return None;
            }
            let parts: Vec<&str> = line.split(stage.delim).collect();
            let selected: Vec<&str> = parts
                .iter()
                .enumerate()
                .filter(|(idx, _)| {
                    let included = streaming_cut_range_includes(ranges, idx + 1);
                    if stage.complement {
                        !included
                    } else {
                        included
                    }
                })
                .map(|(_, part)| *part)
                .collect();
            Some(selected.join(&stage.output_delim).into_bytes())
        }
        StreamingCutMode::Chars(ranges) | StreamingCutMode::Bytes(ranges) => {
            let chars: Vec<char> = line.chars().collect();
            let selected: String = chars
                .iter()
                .enumerate()
                .filter(|(idx, _)| {
                    let included = streaming_cut_range_includes(ranges, idx + 1);
                    if stage.complement {
                        !included
                    } else {
                        included
                    }
                })
                .map(|(_, ch)| *ch)
                .collect();
            Some(selected.into_bytes())
        }
    }
}

struct CutStreamReader<R> {
    inner: R,
    stage: StreamingCutStage,
    input_pending: Vec<u8>,
    output_pending: Vec<u8>,
    output_offset: usize,
    finished: bool,
}

impl<R> CutStreamReader<R> {
    fn new(inner: R, stage: StreamingCutStage) -> Self {
        Self {
            inner,
            stage,
            input_pending: Vec::new(),
            output_pending: Vec::new(),
            output_offset: 0,
            finished: false,
        }
    }
}

impl<R: Read> Read for CutStreamReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            let copied =
                take_pending_output(&mut self.output_pending, &mut self.output_offset, buf);
            if copied > 0 {
                return Ok(copied);
            }
            if self.finished {
                return Ok(0);
            }

            match streaming_read_next_line(&mut self.inner, &mut self.input_pending)? {
                Some((line, _had_newline)) => {
                    if let Some(mut out) = apply_streaming_cut(&line, &self.stage) {
                        out.push(b'\n');
                        self.output_pending.extend_from_slice(&out);
                    }
                }
                None => {
                    self.finished = true;
                }
            }
        }
    }
}

fn streaming_grep_match_single(line: &str, pattern: &str, flags: &StreamingGrepFlags) -> bool {
    if flags.word_match {
        return line
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .any(|word| word == pattern);
    }
    // TODO: the streaming grep path only implements literal substring
    // matching. `flags.fixed` (`-F`) is parsed and accepted but has no effect
    // because the non-fixed branch never had a regex engine wired in. When we
    // add regex support, this branch will need to split on `flags.fixed`.
    line.contains(pattern)
}

fn streaming_grep_match_pattern(line: &str, pattern: &str, flags: &StreamingGrepFlags) -> bool {
    let (line_cmp, pattern_cmp) = if flags.ignore_case {
        (line.to_lowercase(), pattern.to_lowercase())
    } else {
        (line.to_string(), pattern.to_string())
    };
    if flags.extended && pattern_cmp.contains('|') {
        return pattern_cmp
            .split('|')
            .any(|alt| streaming_grep_match_single(&line_cmp, alt.trim(), flags));
    }
    streaming_grep_match_single(&line_cmp, &pattern_cmp, flags)
}

fn streaming_grep_find_match<'a>(
    line: &'a str,
    pattern: &str,
    flags: &StreamingGrepFlags,
) -> Option<&'a str> {
    let (line_cmp, pattern_cmp) = if flags.ignore_case {
        (line.to_lowercase(), pattern.to_lowercase())
    } else {
        (line.to_string(), pattern.to_string())
    };
    if flags.word_match {
        let start = line_cmp.find(&pattern_cmp)?;
        if start > 0 && line_cmp.as_bytes()[start - 1].is_ascii_alphanumeric() {
            return None;
        }
        let end = start + pattern_cmp.len();
        if end < line_cmp.len() && line_cmp.as_bytes()[end].is_ascii_alphanumeric() {
            return None;
        }
        Some(&line[start..start + pattern_cmp.len()])
    } else {
        let idx = line_cmp.find(&pattern_cmp)?;
        Some(&line[idx..idx + pattern_cmp.len()])
    }
}

fn streaming_grep_line_matches(
    line: &str,
    flags: &StreamingGrepFlags,
    patterns: &[String],
) -> bool {
    let matched = patterns
        .iter()
        .any(|pattern| streaming_grep_match_pattern(line, pattern, flags));
    matched != flags.invert
}

fn emit_streaming_grep_one(
    output: &mut Vec<u8>,
    line: &str,
    line_num: usize,
    flags: &StreamingGrepFlags,
    patterns: &[String],
) {
    let mut prefix = String::new();
    if flags.show_filename == Some(true) {
        prefix.push_str("(standard input):");
    }
    if flags.show_line_numbers {
        use std::fmt::Write;
        let _ = write!(prefix, "{line_num}:");
    }
    if flags.only_matching {
        for pattern in patterns {
            if let Some(matched) = streaming_grep_find_match(line, pattern, flags) {
                output.extend_from_slice(prefix.as_bytes());
                output.extend_from_slice(matched.as_bytes());
                output.push(b'\n');
            }
        }
    } else {
        output.extend_from_slice(prefix.as_bytes());
        output.extend_from_slice(line.as_bytes());
        output.push(b'\n');
    }
}

#[allow(clippy::struct_excessive_bools)]
struct GrepStreamReader<R> {
    inner: R,
    stage: StreamingGrepStage,
    status: Rc<RefCell<i32>>,
    input_pending: Vec<u8>,
    output_pending: Vec<u8>,
    output_offset: usize,
    finished: bool,
    match_count: u64,
    found: bool,
    remaining_after: usize,
    printed_separator: bool,
    before_buf: VecDeque<(usize, String)>,
    line_num: usize,
    emitted_count_summary: bool,
}

impl<R> GrepStreamReader<R> {
    fn new(inner: R, stage: StreamingGrepStage, status: Rc<RefCell<i32>>) -> Self {
        Self {
            inner,
            stage,
            status,
            input_pending: Vec::new(),
            output_pending: Vec::new(),
            output_offset: 0,
            finished: false,
            match_count: 0,
            found: false,
            remaining_after: 0,
            printed_separator: false,
            before_buf: VecDeque::new(),
            line_num: 0,
            emitted_count_summary: false,
        }
    }

    fn emit_count_summary(&mut self) {
        if self.stage.flags.count_only && !self.stage.flags.quiet && !self.emitted_count_summary {
            if self.stage.flags.show_filename == Some(true) {
                self.output_pending.extend_from_slice(
                    format!("(standard input):{}\n", self.match_count).as_bytes(),
                );
            } else {
                self.output_pending
                    .extend_from_slice(format!("{}\n", self.match_count).as_bytes());
            }
            self.emitted_count_summary = true;
        }
    }
}

impl<R: Read> Read for GrepStreamReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            let copied =
                take_pending_output(&mut self.output_pending, &mut self.output_offset, buf);
            if copied > 0 {
                return Ok(copied);
            }
            if self.finished {
                return Ok(0);
            }

            if let Some((line, _had_newline)) =
                streaming_read_next_line(&mut self.inner, &mut self.input_pending)?
            {
                self.line_num += 1;
                if streaming_grep_line_matches(&line, &self.stage.flags, &self.stage.patterns) {
                    self.found = true;
                    *self.status.borrow_mut() = 0;
                    self.match_count += 1;

                    if self.stage.flags.quiet || self.stage.flags.files_only {
                        if let Some(max) = self.stage.flags.max_count {
                            if self.match_count >= max as u64 {
                                self.finished = true;
                            }
                        }
                        continue;
                    }

                    if !self.stage.flags.count_only {
                        if self.stage.flags.before_context > 0 && !self.before_buf.is_empty() {
                            if self.printed_separator && self.stage.flags.before_context > 0 {
                                self.output_pending.extend_from_slice(b"--\n");
                            }
                            for (before_line_num, before_line) in &self.before_buf {
                                emit_streaming_grep_one(
                                    &mut self.output_pending,
                                    before_line,
                                    *before_line_num,
                                    &self.stage.flags,
                                    &self.stage.patterns,
                                );
                            }
                        }
                        self.before_buf.clear();
                        emit_streaming_grep_one(
                            &mut self.output_pending,
                            &line,
                            self.line_num,
                            &self.stage.flags,
                            &self.stage.patterns,
                        );
                        self.remaining_after = self.stage.flags.after_context;
                        self.printed_separator = true;
                    }

                    if let Some(max) = self.stage.flags.max_count {
                        if self.match_count >= max as u64 {
                            self.emit_count_summary();
                            self.finished = true;
                        }
                    }
                } else if self.remaining_after > 0 && !self.stage.flags.count_only {
                    emit_streaming_grep_one(
                        &mut self.output_pending,
                        &line,
                        self.line_num,
                        &self.stage.flags,
                        &self.stage.patterns,
                    );
                    self.remaining_after -= 1;
                } else if self.stage.flags.before_context > 0 {
                    self.before_buf.push_back((self.line_num, line));
                    if self.before_buf.len() > self.stage.flags.before_context {
                        self.before_buf.pop_front();
                    }
                }
            } else {
                self.emit_count_summary();
                if !self.found {
                    *self.status.borrow_mut() = 1;
                }
                self.finished = true;
            }
        }
    }
}

fn streaming_uniq_compare_key(line: &str, flags: &StreamingUniqFlags) -> String {
    let mut slice = line;
    for _ in 0..flags.skip_fields {
        slice = slice.trim_start();
        if let Some(pos) = slice.find(char::is_whitespace) {
            slice = &slice[pos..];
        } else {
            slice = "";
            break;
        }
    }
    if flags.skip_chars > 0 {
        let chars: Vec<char> = slice.chars().collect();
        slice = if flags.skip_chars < chars.len() {
            &slice[chars[..flags.skip_chars]
                .iter()
                .map(|ch| ch.len_utf8())
                .sum::<usize>()..]
        } else {
            ""
        };
    }
    let mut key = slice.to_string();
    if let Some(limit) = flags.compare_chars {
        key = key.chars().take(limit).collect();
    }
    if flags.ignore_case {
        key = key.to_lowercase();
    }
    key
}

fn emit_streaming_uniq_line(
    output: &mut Vec<u8>,
    line: &str,
    count: usize,
    flags: &StreamingUniqFlags,
) {
    if flags.duplicates_only && count < 2 {
        return;
    }
    if flags.unique_only && count > 1 {
        return;
    }
    if flags.count {
        output.extend_from_slice(format!("{count:>7} {line}\n").as_bytes());
    } else {
        output.extend_from_slice(line.as_bytes());
        output.push(b'\n');
    }
}

struct UniqStreamReader<R> {
    inner: R,
    flags: StreamingUniqFlags,
    input_pending: Vec<u8>,
    output_pending: Vec<u8>,
    output_offset: usize,
    finished: bool,
    prev: Option<(String, String)>,
    count: usize,
}

impl<R> UniqStreamReader<R> {
    fn new(inner: R, flags: StreamingUniqFlags) -> Self {
        Self {
            inner,
            flags,
            input_pending: Vec::new(),
            output_pending: Vec::new(),
            output_offset: 0,
            finished: false,
            prev: None,
            count: 0,
        }
    }
}

impl<R: Read> Read for UniqStreamReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            let copied =
                take_pending_output(&mut self.output_pending, &mut self.output_offset, buf);
            if copied > 0 {
                return Ok(copied);
            }
            if self.finished {
                return Ok(0);
            }

            if let Some((line, _had_newline)) =
                streaming_read_next_line(&mut self.inner, &mut self.input_pending)?
            {
                let key = streaming_uniq_compare_key(&line, &self.flags);
                if self
                    .prev
                    .as_ref()
                    .is_some_and(|(_, prev_key)| *prev_key == key)
                {
                    self.count += 1;
                } else {
                    if let Some((prev_line, _)) = self.prev.take() {
                        emit_streaming_uniq_line(
                            &mut self.output_pending,
                            &prev_line,
                            self.count,
                            &self.flags,
                        );
                    }
                    self.prev = Some((line, key));
                    self.count = 1;
                }
            } else {
                if let Some((prev_line, _)) = self.prev.take() {
                    emit_streaming_uniq_line(
                        &mut self.output_pending,
                        &prev_line,
                        self.count,
                        &self.flags,
                    );
                }
                self.finished = true;
            }
        }
    }
}

fn streaming_tr_expand_set(s: &str) -> Vec<char> {
    let mut chars = Vec::new();
    let mut iter = s.chars().peekable();
    while let Some(ch) = iter.next() {
        if ch == '[' && iter.peek() == Some(&':') {
            iter.next();
            let class_name: String = iter.by_ref().take_while(|&c| c != ':').collect();
            let _ = iter.next();
            match class_name.as_str() {
                "upper" => chars.extend('A'..='Z'),
                "lower" => chars.extend('a'..='z'),
                "digit" => chars.extend('0'..='9'),
                "alpha" => {
                    chars.extend('A'..='Z');
                    chars.extend('a'..='z');
                }
                "alnum" => {
                    chars.extend('0'..='9');
                    chars.extend('A'..='Z');
                    chars.extend('a'..='z');
                }
                "space" => chars.extend([' ', '\t', '\n', '\r', '\x0b', '\x0c']),
                "blank" => chars.extend([' ', '\t']),
                "punct" => chars.extend("!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~".chars()),
                _ => {}
            }
        } else if iter.peek() == Some(&'-') {
            let saved = iter.clone();
            iter.next();
            if let Some(&end_ch) = iter.peek() {
                if end_ch > ch {
                    chars.extend(ch..=end_ch);
                    iter.next();
                } else {
                    chars.push(ch);
                    iter = saved;
                    iter.next();
                    chars.push('-');
                }
            } else {
                chars.push(ch);
                chars.push('-');
            }
        } else if ch == '\\' {
            match iter.next() {
                Some('n') => chars.push('\n'),
                Some('t') => chars.push('\t'),
                Some('r') => chars.push('\r'),
                Some('\\') | None => chars.push('\\'),
                Some(other) => chars.push(other),
            }
        } else {
            chars.push(ch);
        }
    }
    chars
}

fn streaming_tr_process_utf8_chunk(pending: &mut Vec<u8>, chunk: &[u8], mut f: impl FnMut(char)) {
    pending.extend_from_slice(chunk);
    loop {
        match std::str::from_utf8(pending) {
            Ok(text) => {
                for ch in text.chars() {
                    f(ch);
                }
                pending.clear();
                return;
            }
            Err(err) => {
                let valid = err.valid_up_to();
                if valid > 0 {
                    let text = String::from_utf8_lossy(&pending[..valid]).to_string();
                    for ch in text.chars() {
                        f(ch);
                    }
                    pending.drain(..valid);
                    continue;
                }
                if err.error_len().is_some() {
                    let text = String::from_utf8_lossy(&pending[..1]).to_string();
                    for ch in text.chars() {
                        f(ch);
                    }
                    pending.drain(..1);
                    continue;
                }
                return;
            }
        }
    }
}

fn streaming_tr_flush_pending_lossy(pending: &mut Vec<u8>, mut f: impl FnMut(char)) {
    if pending.is_empty() {
        return;
    }
    let text = String::from_utf8_lossy(pending).to_string();
    pending.clear();
    for ch in text.chars() {
        f(ch);
    }
}

struct TrStreamReader<R> {
    inner: R,
    stage: StreamingTrStage,
    input_pending: Vec<u8>,
    output_pending: Vec<u8>,
    output_offset: usize,
    finished: bool,
    prev: Option<char>,
}

impl<R> TrStreamReader<R> {
    fn new(inner: R, stage: StreamingTrStage) -> Self {
        Self {
            inner,
            stage,
            input_pending: Vec::new(),
            output_pending: Vec::new(),
            output_offset: 0,
            finished: false,
            prev: None,
        }
    }

    fn emit_char(&mut self, ch: char) {
        let mut buffer = [0u8; 4];
        self.output_pending
            .extend_from_slice(ch.encode_utf8(&mut buffer).as_bytes());
        self.prev = Some(ch);
    }

    fn process_char(&mut self, ch: char) {
        if self.stage.delete && self.stage.squeeze && !self.stage.to_chars.is_empty() {
            let in_set = self.stage.from_chars.contains(&ch);
            let keep = if self.stage.complement {
                in_set
            } else {
                !in_set
            };
            if !keep {
                return;
            }
            if self.stage.to_chars.contains(&ch) && self.prev == Some(ch) {
                return;
            }
            self.emit_char(ch);
            return;
        }

        if self.stage.delete {
            let in_set = self.stage.from_chars.contains(&ch);
            let keep = if self.stage.complement {
                in_set
            } else {
                !in_set
            };
            if keep {
                self.emit_char(ch);
            }
            return;
        }

        if self.stage.squeeze && self.stage.to_chars.is_empty() {
            if self.stage.from_chars.contains(&ch) && self.prev == Some(ch) {
                return;
            }
            self.emit_char(ch);
            return;
        }

        let from_set = if self.stage.complement {
            (0u8..=127)
                .map(|b| b as char)
                .filter(|candidate| !self.stage.from_chars.contains(candidate))
                .collect::<Vec<_>>()
        } else {
            self.stage.from_chars.clone()
        };
        let translated = if let Some(pos) = from_set.iter().position(|&source| source == ch) {
            self.stage
                .to_chars
                .get(pos)
                .or(self.stage.to_chars.last())
                .copied()
                .unwrap_or(ch)
        } else {
            ch
        };
        if self.stage.squeeze
            && self.stage.to_chars.contains(&translated)
            && self.prev == Some(translated)
        {
            return;
        }
        self.emit_char(translated);
    }
}

impl<R: Read> Read for TrStreamReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            let copied =
                take_pending_output(&mut self.output_pending, &mut self.output_offset, buf);
            if copied > 0 {
                return Ok(copied);
            }
            if self.finished {
                return Ok(0);
            }

            let mut scratch = [0u8; 4096];
            let read = self.inner.read(&mut scratch)?;
            if read == 0 {
                let mut pending = std::mem::take(&mut self.input_pending);
                let mut chars = Vec::new();
                streaming_tr_flush_pending_lossy(&mut pending, |ch| chars.push(ch));
                self.input_pending = pending;
                for ch in chars {
                    self.process_char(ch);
                }
                self.finished = true;
                continue;
            }
            let mut pending = std::mem::take(&mut self.input_pending);
            let mut chars = Vec::new();
            streaming_tr_process_utf8_chunk(&mut pending, &scratch[..read], |ch| chars.push(ch));
            self.input_pending = pending;
            for ch in chars {
                self.process_char(ch);
            }
        }
    }
}

/// Result from an external command handler.
#[derive(Debug)]
pub struct ExternalCommandResult {
    /// Data written to stdout.
    pub stdout: Vec<u8>,
    /// Data written to stderr.
    pub stderr: Vec<u8>,
    /// Exit code (0 = success).
    pub status: i32,
}

pub struct ExternalCommandStdin<'a> {
    reader: Box<dyn Read + 'a>,
}

impl std::fmt::Debug for ExternalCommandStdin<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExternalCommandStdin")
            .finish_non_exhaustive()
    }
}

impl<'a> ExternalCommandStdin<'a> {
    #[must_use]
    pub fn from_bytes(data: &'a [u8]) -> Self {
        Self {
            reader: Box::new(Cursor::new(data)),
        }
    }

    #[must_use]
    pub fn from_reader<R>(reader: R) -> Self
    where
        R: Read + 'a,
    {
        Self {
            reader: Box::new(reader),
        }
    }

    pub fn read_chunk(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.reader.read(buf)
    }
}

impl Read for ExternalCommandStdin<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.read_chunk(buf)
    }
}

/// Callback type for external (host-provided) commands.
///
/// Called with `(command_name, argv, stdin)`. Returns `Some(result)` if
/// the command was handled, `None` to fall through to "command not found".
pub type ExternalCommandHandler = Box<
    dyn FnMut(&str, &[String], Option<ExternalCommandStdin<'_>>) -> Option<ExternalCommandResult>,
>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RuntimeCommandKind {
    Local,
    Break,
    Continue,
    Exit,
    Eval,
    Source,
    Declare,
    Let,
    Shopt,
    Alias,
    Unalias,
    BuiltinKeyword,
    Mapfile,
    Type,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UtilityCommandKind {
    Plain,
    FindWithExec,
    Xargs,
}

#[derive(Clone, Debug)]
enum ResolvedCommand {
    Runtime(RuntimeCommandKind),
    ShellScript,
    Function(HirCommand),
    Builtin,
    Utility(UtilityCommandKind),
    External,
}

#[derive(Clone, Debug)]
struct ActiveRun {
    input: String,
    hir: HirProgram,
    complete_index: usize,
    and_or_index: usize,
}

impl ActiveRun {
    fn new(input: String, hir: HirProgram) -> Self {
        Self {
            input,
            hir,
            complete_index: 0,
            and_or_index: 0,
        }
    }

    fn is_done(&self) -> bool {
        self.complete_index >= self.hir.items.len()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActiveRunStep {
    Pending,
    Done,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecutionPoll {
    Yield(Vec<WorkerEvent>),
    Done(Vec<WorkerEvent>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum VmSubsetFallbackReason {
    Disabled,
    Lowering(LoweringError),
    AssignmentShape,
    UnsupportedWord,
    ShellExpansion,
    AliasExpansion,
    NonBuiltinCommand,
    CommandEnvPrefixes,
    UnsupportedRedirection,
}

struct RuntimeVmExecutor<'a> {
    fs: &'a mut BackendFs,
    builtins: &'a wasmsh_builtins::BuiltinRegistry,
    current_exec_io: &'a mut Option<ExecIo>,
    proc_subst_out_scopes: &'a mut Vec<Vec<PendingProcessSubstOut>>,
    exec: &'a mut ExecState,
}

impl RuntimeVmExecutor<'_> {
    fn prepare_exec_io(
        &mut self,
        state: &mut ShellState,
        redirections: &[IrRedirection],
    ) -> Result<Option<ExecIo>, String> {
        let mut exec_io = self.current_exec_io.clone().unwrap_or_default();
        let mut handled_any = false;

        for redirection in redirections {
            let fd = redirection.fd.unwrap_or(1);
            let append = matches!(redirection.op, RedirectionOp::Append);
            let target = wasmsh_expand::expand_word(&redirection.target, state);
            let path = resolve_path_from_cwd(&state.cwd, &target);
            let sink = match self.fs.open_write_sink(&path, append) {
                Ok(sink) => sink,
                Err(err) => {
                    return Err(format!("wasmsh: {target}: {err}\n"));
                }
            };
            exec_io.fds_mut().open_output(
                fd,
                OutputTarget::File {
                    path,
                    append,
                    sink: Rc::new(RefCell::new(sink)),
                },
            );
            handled_any = true;
        }

        Ok(handled_any.then_some(exec_io))
    }

    fn with_exec_io_scope<T>(
        current_exec_io: &mut Option<ExecIo>,
        proc_subst_out_scopes: &mut Vec<Vec<PendingProcessSubstOut>>,
        exec: &mut ExecState,
        exec_io: Option<ExecIo>,
        f: impl FnOnce(&mut Option<ExecIo>, &mut Vec<Vec<PendingProcessSubstOut>>, &mut ExecState) -> T,
    ) -> T {
        if let Some(exec_io) = exec_io {
            let saved = current_exec_io.replace(exec_io);
            let result = f(current_exec_io, proc_subst_out_scopes, exec);
            let current = current_exec_io.take();
            *current_exec_io = match (saved, current) {
                (Some(mut saved), Some(mut current)) => {
                    let stdin = current.take_stdin();
                    saved.fds_mut().set_input(stdin);
                    Some(saved)
                }
                (saved, _) => saved,
            };
            result
        } else {
            f(current_exec_io, proc_subst_out_scopes, exec)
        }
    }

    fn write_visible_stderr(&mut self, vm: &mut Vm, data: &[u8]) {
        let mut router = RuntimeOutputRouter {
            exec: self.exec,
            exec_io: self.current_exec_io.as_mut(),
            proc_subst_out_scopes: self.proc_subst_out_scopes,
            vm_stdout: &mut vm.stdout,
            vm_stderr: &mut vm.stderr,
            vm_output_bytes: &mut vm.output_bytes,
            vm_output_limit: vm.limits.output_byte_limit,
            vm_diagnostics: &mut vm.diagnostics,
        };
        router.write_stderr(data);
    }

    fn take_pending_input_reader(
        &mut self,
        cmd_name: &str,
    ) -> Result<Option<Box<dyn Read>>, String> {
        let Some(exec_io) = self.current_exec_io.as_mut() else {
            return Ok(None);
        };
        match exec_io.take_stdin() {
            InputTarget::Inherit | InputTarget::Closed => Ok(None),
            InputTarget::Bytes(data) => Ok(Some(Box::new(Cursor::new(data)))),
            InputTarget::File {
                path,
                remove_after_read,
            } => {
                let handle = self
                    .fs
                    .open(&path, OpenOptions::read())
                    .map_err(|err| format!("wasmsh: {cmd_name}: {err}\n"))?;
                let reader = self
                    .fs
                    .stream_file(handle)
                    .map_err(|err| format!("wasmsh: {cmd_name}: {err}\n"));
                self.fs.close(handle);
                if remove_after_read {
                    let _ = self.fs.remove_file(&path);
                }
                reader.map(Some)
            }
            InputTarget::Pipe(pipe) => Ok(Some(Box::new(PipeReader::new(pipe)))),
        }
    }

    fn take_builtin_stdin(
        &mut self,
        cmd_name: &str,
    ) -> Result<Option<wasmsh_builtins::BuiltinStdin<'static>>, String> {
        let reader = self.take_pending_input_reader(cmd_name)?;
        Ok(reader.map(wasmsh_builtins::BuiltinStdin::from_reader))
    }

    /// Drain a pending nounset error from parameter expansion so the VM-subset
    /// path reports it the same way the fallback interpreter does.
    fn consume_nounset_error(&mut self, vm: &mut Vm) -> bool {
        let Some(var_name) = vm.state.take_nounset_error() else {
            return false;
        };
        let msg = format!("wasmsh: {var_name}: unbound variable\n");
        self.write_visible_stderr(vm, msg.as_bytes());
        vm.state.last_status = 1;
        true
    }
}

impl VmExecutor for RuntimeVmExecutor<'_> {
    fn assign(&mut self, vm: &mut Vm, name: &str, value: Option<&Word>) {
        let value = value.map_or_else(String::new, |word| {
            wasmsh_expand::expand_word(word, &mut vm.state)
        });
        if self.consume_nounset_error(vm) {
            return;
        }
        let trimmed = value.trim();
        if trimmed.starts_with('(') && trimmed.ends_with(')') {
            let inner = &trimmed[1..trimmed.len() - 1];
            let elements = WorkerRuntime::parse_array_elements(inner);
            let name_key = smol_str::SmolStr::from(name);

            if WorkerRuntime::is_assoc_array_assignment(inner, &elements) {
                vm.state.init_assoc_array(name_key.clone());
                for (key, value) in WorkerRuntime::parse_assoc_pairs(inner) {
                    vm.state.set_array_element(
                        name_key.clone(),
                        &key,
                        smol_str::SmolStr::from(value.as_str()),
                    );
                }
            } else {
                vm.state.init_indexed_array(name_key.clone());
                for (idx, element) in elements.iter().enumerate() {
                    vm.state
                        .set_array_element(name_key.clone(), &idx.to_string(), element.clone());
                }
            }
            vm.state.last_status = 0;
            return;
        }

        let assigned = if vm.state.env.get(name).is_some_and(|var| var.integer) {
            wasmsh_expand::eval_arithmetic(trimmed, &mut vm.state).to_string()
        } else {
            value
        };
        vm.state.set_var(name.into(), assigned.into());
        vm.state.last_status = 0;
    }

    fn execute_builtin(
        &mut self,
        vm: &mut Vm,
        name: &str,
        argv: &[Word],
        redirections: &[IrRedirection],
    ) -> i32 {
        let Some(builtin_fn) = self.builtins.get(name) else {
            vm.emit_diagnostic(
                wasmsh_vm::DiagLevel::Error,
                wasmsh_vm::DiagCategory::Builtin,
                format!("unknown builtin: {name}"),
            );
            vm.state.last_status = 127;
            return 127;
        };
        let expanded: Vec<String> = argv
            .iter()
            .map(|word| wasmsh_expand::expand_word(word, &mut vm.state))
            .collect();
        if self.consume_nounset_error(vm) {
            return 1;
        }
        let argv_refs: Vec<&str> = expanded.iter().map(String::as_str).collect();
        let stdin = match self.take_builtin_stdin(name) {
            Ok(stdin) => stdin,
            Err(message) => {
                self.write_visible_stderr(vm, message.as_bytes());
                vm.state.last_status = 1;
                return 1;
            }
        };
        let exec_io = match self.prepare_exec_io(&mut vm.state, redirections) {
            Ok(exec_io) => exec_io,
            Err(message) => {
                self.write_visible_stderr(vm, message.as_bytes());
                vm.state.last_status = 1;
                return 1;
            }
        };

        let fs = &*self.fs;
        Self::with_exec_io_scope(
            &mut *self.current_exec_io,
            &mut *self.proc_subst_out_scopes,
            &mut *self.exec,
            exec_io,
            |current_exec_io, proc_subst_out_scopes, exec| {
                let mut router = RuntimeOutputRouter {
                    exec,
                    exec_io: current_exec_io.as_mut(),
                    proc_subst_out_scopes,
                    vm_stdout: &mut vm.stdout,
                    vm_stderr: &mut vm.stderr,
                    vm_output_bytes: &mut vm.output_bytes,
                    vm_output_limit: vm.limits.output_byte_limit,
                    vm_diagnostics: &mut vm.diagnostics,
                };
                let mut sink = RuntimeBuiltinSink {
                    router: &mut router,
                };
                let status = {
                    let mut ctx = wasmsh_builtins::BuiltinContext {
                        state: &mut vm.state,
                        output: &mut sink,
                        fs: Some(fs),
                        stdin,
                    };
                    builtin_fn(&mut ctx, &argv_refs)
                };
                vm.state.last_status = status;
                status
            },
        )
    }
}

/// The worker-side runtime that processes host commands.
#[allow(missing_debug_implementations)]
pub struct WorkerRuntime {
    config: BrowserConfig,
    vm: Vm,
    fs: BackendFs,
    utils: UtilRegistry,
    builtins: wasmsh_builtins::BuiltinRegistry,
    initialized: bool,
    /// Command-scoped stdin/stdout/stderr routing for the currently executing command.
    current_exec_io: Option<ExecIo>,
    /// Deferred `>(...)` sinks scoped to the currently executing command.
    proc_subst_out_scopes: Vec<Vec<PendingProcessSubstOut>>,
    /// Deferred `<(...)` cleanup and stderr flush scoped to the current command.
    proc_subst_in_scopes: Vec<Vec<PendingProcessSubstIn>>,
    /// Registered shell functions (name → HIR body).
    functions: IndexMap<String, HirCommand>,
    /// Transient execution state (loop control, exit, locals).
    exec: ExecState,
    /// Shell aliases (name → replacement text).
    aliases: IndexMap<String, String>,
    /// Optional handler for external commands (e.g. python3 in Pyodide).
    external_handler: Option<ExternalCommandHandler>,
    /// Optional network backend for curl/wget utilities.
    network: Option<Box<dyn wasmsh_utils::net_types::NetworkBackend>>,
    /// Active top-level execution, if a run has been started and not yet completed.
    active_run: Option<ActiveRun>,
}

/// Action to take for a character during array element parsing.
enum ArrayCharAction {
    Append(char),
    Skip,
    SplitField,
}

enum StreamingPipelineStage {
    Literal(Vec<u8>),
    File(String),
    Yes { line: Vec<u8> },
    BufferedCommand(BufferedPipelineCommand),
    Cat,
    Head(StreamingHeadMode),
    Tail(StreamingTailMode),
    Bat(StreamingBatStage),
    Sed(StreamingSedStage),
    Tee(StreamingTeeStage),
    Paste(StreamingPasteStage),
    Column(StreamingColumnStage),
    Grep(StreamingGrepStage),
    Uniq(StreamingUniqFlags),
    Rev,
    Cut(StreamingCutStage),
    Tr(StreamingTrStage),
    Wc(StreamingWcFlags),
}

#[derive(Clone, Debug)]
enum StreamingCutMode {
    Fields(Vec<StreamingCutRange>),
    Chars(Vec<StreamingCutRange>),
    Bytes(Vec<StreamingCutRange>),
}

#[derive(Clone, Debug)]
struct StreamingCutStage {
    mode: StreamingCutMode,
    delim: char,
    complement: bool,
    only_delimited: bool,
    output_delim: String,
}

#[derive(Clone, Debug)]
struct StreamingCutRange {
    start: Option<usize>,
    end: Option<usize>,
}

#[derive(Clone, Debug)]
struct StreamingTrStage {
    delete: bool,
    squeeze: bool,
    complement: bool,
    from_chars: Vec<char>,
    to_chars: Vec<char>,
}

/// Quoting state for parsing array elements.
#[derive(Default)]
struct ArrayParseState {
    in_single_quote: bool,
    in_double_quote: bool,
    escape_next: bool,
}

impl ArrayParseState {
    fn process_char(&mut self, ch: char) -> ArrayCharAction {
        if self.escape_next {
            self.escape_next = false;
            return ArrayCharAction::Append(ch);
        }
        if ch == '\\' && !self.in_single_quote {
            self.escape_next = true;
            return ArrayCharAction::Skip;
        }
        if ch == '\'' && !self.in_double_quote {
            self.in_single_quote = !self.in_single_quote;
            return ArrayCharAction::Skip;
        }
        if ch == '"' && !self.in_single_quote {
            self.in_double_quote = !self.in_double_quote;
            return ArrayCharAction::Skip;
        }
        if ch.is_ascii_whitespace() && !self.in_single_quote && !self.in_double_quote {
            return ArrayCharAction::SplitField;
        }
        ArrayCharAction::Append(ch)
    }
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
            vm: Vm::with_limits(ShellState::new(), ExecutionLimits::default()),
            fs: BackendFs::new(),
            utils: UtilRegistry::new(),
            builtins: wasmsh_builtins::BuiltinRegistry::new(),
            initialized: false,
            current_exec_io: None,
            proc_subst_out_scopes: Vec::new(),
            proc_subst_in_scopes: Vec::new(),
            functions: IndexMap::new(),
            exec: ExecState::new(),
            aliases: IndexMap::new(),
            external_handler: None,
            network: None,
            active_run: None,
        }
    }

    /// Register a handler for external commands (e.g. `python3` in Pyodide).
    pub fn set_external_handler(&mut self, handler: ExternalCommandHandler) {
        self.external_handler = Some(handler);
    }

    /// Register a network backend for `curl`/`wget` utilities.
    pub fn set_network_backend(
        &mut self,
        backend: Box<dyn wasmsh_utils::net_types::NetworkBackend>,
    ) {
        self.network = Some(backend);
    }

    /// Process a host command and return a list of events to send back.
    pub fn handle_command(&mut self, cmd: HostCommand) -> Vec<WorkerEvent> {
        match cmd {
            HostCommand::Init {
                step_budget,
                allowed_hosts,
            } => {
                self.config.step_budget = step_budget;
                self.config.allowed_hosts = allowed_hosts;
                self.vm = Vm::with_limits(
                    ShellState::new(),
                    ExecutionLimits {
                        step_limit: step_budget,
                        output_byte_limit: self.config.output_byte_limit,
                        pipe_byte_limit: self.config.pipe_byte_limit,
                        recursion_limit: self.config.recursion_limit,
                    },
                );
                self.fs = BackendFs::new();
                self.current_exec_io = None;
                self.proc_subst_out_scopes.clear();
                self.proc_subst_in_scopes.clear();
                self.functions = IndexMap::new();
                self.exec.reset();
                self.aliases = IndexMap::new();
                self.active_run = None;
                self.initialized = true;
                // Set default shopt options (bash defaults)
                self.vm.state.set_var("SHOPT_extglob".into(), "1".into());
                self.vm
                    .state
                    .set_var("SHOPT_expand_aliases".into(), "1".into());
                vec![WorkerEvent::Version(PROTOCOL_VERSION.to_string())]
            }
            HostCommand::Run { input } => {
                if !self.initialized {
                    return vec![WorkerEvent::Diagnostic(
                        DiagnosticLevel::Error,
                        "runtime not initialized".into(),
                    )];
                }
                match self.start_execution(input) {
                    Ok(()) => self.poll_active_run_to_completion(),
                    Err(events) => events,
                }
            }
            HostCommand::StartRun { input } => {
                if !self.initialized {
                    return vec![WorkerEvent::Diagnostic(
                        DiagnosticLevel::Error,
                        "runtime not initialized".into(),
                    )];
                }
                match self.start_execution(input) {
                    Ok(()) => vec![WorkerEvent::Yielded],
                    Err(events) => events,
                }
            }
            HostCommand::PollRun => match self.poll_active_run() {
                Some(ExecutionPoll::Yield(mut events)) => {
                    events.push(WorkerEvent::Yielded);
                    events
                }
                Some(ExecutionPoll::Done(events)) => events,
                None => vec![WorkerEvent::Diagnostic(
                    DiagnosticLevel::Error,
                    "no active run".into(),
                )],
            },
            HostCommand::Cancel => {
                self.cancel_active_execution();
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
                            self.write_stderr(format!("wasmsh: write error: {e}\n").as_bytes());
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

    pub fn start_execution(&mut self, input: String) -> Result<(), Vec<WorkerEvent>> {
        if !self.initialized {
            return Err(vec![WorkerEvent::Diagnostic(
                DiagnosticLevel::Error,
                "runtime not initialized".into(),
            )]);
        }
        if self.active_run.is_some() {
            return Err(vec![WorkerEvent::Diagnostic(
                DiagnosticLevel::Error,
                "execution already active".into(),
            )]);
        }

        let hir = match wasmsh_parse::parse(&input) {
            Ok(ast) => wasmsh_hir::lower(&ast),
            Err(e) => {
                self.vm.state.last_status = 2;
                return Err(vec![
                    WorkerEvent::Stderr(format!("wasmsh: parse error: {e}\n").into_bytes()),
                    WorkerEvent::Exit(2),
                ]);
            }
        };

        self.exec.reset();
        self.current_exec_io = None;
        self.proc_subst_out_scopes.clear();
        self.proc_subst_in_scopes.clear();
        self.vm.steps = 0;
        self.vm.budget.steps = 0;
        self.vm.budget.visible_output_bytes = self.vm.output_bytes;
        self.vm.budget.pipe_bytes = 0;
        self.vm.budget.recursion_depth = 0;
        self.vm.budget.clear_stop_reason();
        self.vm.cancellation_token().reset();
        self.active_run = Some(ActiveRun::new(input, hir));
        Ok(())
    }

    pub fn poll_active_run(&mut self) -> Option<ExecutionPoll> {
        let mut run = self.active_run.take()?;
        let previous_step_limit = self.vm.limits.step_limit;
        self.vm.steps = 0;
        self.vm.budget.steps = 0;
        self.vm.limits.step_limit = 0;

        let mut remaining = if self.config.step_budget == 0 {
            usize::MAX
        } else {
            self.config.step_budget as usize
        };
        let mut finished = run.is_done();

        while !finished && remaining > 0 {
            if self.check_resource_limits() {
                finished = true;
                break;
            }
            if self.exec.exit_requested.is_some() || self.exec.resource_exhausted {
                finished = true;
                break;
            }

            let step_outcome = self.poll_active_run_step(&mut run);
            remaining -= 1;
            finished = matches!(step_outcome, ActiveRunStep::Done);
        }

        self.vm.limits.step_limit = previous_step_limit;

        if finished || self.exec.exit_requested.is_some() || self.exec.resource_exhausted {
            self.ensure_stop_reason();
            let mut events = Vec::new();
            self.run_exit_trap_if_needed(&mut events);
            self.drain_io_events(&mut events);
            self.drain_diagnostic_events(&mut events);
            let exit_status = self.current_run_exit_status();
            events.push(WorkerEvent::Exit(exit_status));
            self.active_run = None;
            Some(ExecutionPoll::Done(events))
        } else {
            let events = self.drain_partial_run_events();
            self.active_run = Some(run);
            Some(ExecutionPoll::Yield(events))
        }
    }

    pub fn cancel_active_execution(&mut self) {
        self.vm.cancellation_token().cancel();
    }

    fn poll_active_run_to_completion(&mut self) -> Vec<WorkerEvent> {
        let mut events = Vec::new();
        while let Some(poll) = self.poll_active_run() {
            match poll {
                ExecutionPoll::Yield(mut batch) => {
                    events.append(&mut batch);
                }
                ExecutionPoll::Done(mut batch) => {
                    events.append(&mut batch);
                    break;
                }
            }
        }
        events
    }

    fn poll_active_run_step(&mut self, run: &mut ActiveRun) -> ActiveRunStep {
        if run.is_done() || self.exec.exit_requested.is_some() || self.exec.resource_exhausted {
            return ActiveRunStep::Done;
        }

        let cc = &run.hir.items[run.complete_index];
        self.vm.state.lineno = Self::line_number_for_offset(&run.input, cc.span.start as usize);
        let and_or = &cc.list[run.and_or_index];
        self.execute_and_or(and_or);
        if self.exec.exit_requested.is_none() && self.should_errexit(and_or) {
            self.exec.exit_requested = Some(self.vm.state.last_status);
        }

        run.and_or_index += 1;
        if run.and_or_index >= cc.list.len() {
            run.complete_index += 1;
            run.and_or_index = 0;
        }

        if run.is_done() || self.exec.exit_requested.is_some() || self.exec.resource_exhausted {
            ActiveRunStep::Done
        } else {
            ActiveRunStep::Pending
        }
    }

    fn drain_partial_run_events(&mut self) -> Vec<WorkerEvent> {
        let mut events = Vec::new();
        self.drain_io_events(&mut events);
        self.drain_diagnostic_events(&mut events);
        events
    }

    fn current_run_exit_status(&self) -> i32 {
        if self.exec.resource_exhausted {
            match self.exec.stop_reason.as_ref() {
                Some(StopReason::Cancelled) => 130,
                _ => 128,
            }
        } else {
            self.exec
                .exit_requested
                .unwrap_or(self.vm.state.last_status)
        }
    }

    fn mark_stop_reason(&mut self, reason: StopReason) {
        self.exec.resource_exhausted = true;
        self.exec.stop_reason = Some(reason);
    }

    fn mark_budget_exhaustion(&mut self, reason: ExhaustionReason) {
        self.mark_stop_reason(StopReason::Exhausted(reason));
    }

    fn ensure_stop_reason(&mut self) {
        if !self.exec.resource_exhausted || self.exec.stop_reason.is_some() {
            return;
        }
        if self.vm.cancellation_token().is_cancelled() {
            self.mark_stop_reason(StopReason::Cancelled);
            return;
        }
        if let Some(reason) = self.vm.stop_reason().cloned() {
            self.mark_stop_reason(reason);
            return;
        }
        let limit = self.vm.limits.output_byte_limit;
        if limit > 0 && self.vm.output_bytes > limit {
            self.mark_budget_exhaustion(ExhaustionReason {
                category: BudgetCategory::VisibleOutputBytes,
                used: self.vm.output_bytes,
                limit,
            });
        }
    }

    fn sync_pipe_budget(&mut self, used: u64) {
        if self.exec.resource_exhausted {
            return;
        }
        let limit = self.vm.limits.pipe_byte_limit;
        if let Err(reason) = self.vm.budget.set_pipe_bytes(used, limit) {
            self.mark_budget_exhaustion(reason.clone());
            self.vm.emit_diagnostic(
                wasmsh_vm::DiagLevel::Error,
                wasmsh_vm::DiagCategory::Budget,
                reason.diagnostic_message(),
            );
        }
    }

    pub fn set_output_byte_limit(&mut self, limit: u64) {
        self.config.output_byte_limit = limit;
        self.vm.limits.output_byte_limit = limit;
    }

    pub fn set_pipe_byte_limit(&mut self, limit: u64) {
        self.config.pipe_byte_limit = limit;
        self.vm.limits.pipe_byte_limit = limit;
    }

    pub fn set_recursion_limit(&mut self, limit: u32) {
        self.config.recursion_limit = limit;
        self.vm.limits.recursion_limit = limit;
    }

    pub fn set_vm_subset_enabled(&mut self, enabled: bool) {
        self.config.vm_subset_enabled = enabled;
    }

    fn execute_and_or(&mut self, and_or: &HirAndOr) {
        if let Ok(program) = self.lower_vm_subset_and_or(and_or) {
            self.execute_ir_program(&program);
            return;
        }
        self.execute_pipeline_chain(and_or);
    }

    fn execute_ir_program(&mut self, program: &IrProgram) {
        let mut executor = RuntimeVmExecutor {
            fs: &mut self.fs,
            builtins: &self.builtins,
            current_exec_io: &mut self.current_exec_io,
            proc_subst_out_scopes: &mut self.proc_subst_out_scopes,
            exec: &mut self.exec,
        };
        let _ = self.vm.run_with_executor(program, &mut executor);
    }

    fn lower_vm_subset_and_or(
        &self,
        and_or: &HirAndOr,
    ) -> Result<IrProgram, VmSubsetFallbackReason> {
        if !self.config.vm_subset_enabled {
            return Err(VmSubsetFallbackReason::Disabled);
        }

        self.validate_vm_subset_and_or(and_or)?;
        lower_supported_and_or(and_or).map_err(VmSubsetFallbackReason::Lowering)
    }

    fn validate_vm_subset_and_or(&self, and_or: &HirAndOr) -> Result<(), VmSubsetFallbackReason> {
        self.validate_vm_subset_pipeline(&and_or.first)?;
        for (_, pipeline) in &and_or.rest {
            self.validate_vm_subset_pipeline(pipeline)?;
        }
        Ok(())
    }

    fn validate_vm_subset_pipeline(
        &self,
        pipeline: &HirPipeline,
    ) -> Result<(), VmSubsetFallbackReason> {
        if pipeline.negated || pipeline.commands.len() != 1 {
            return Err(VmSubsetFallbackReason::Lowering(
                LoweringError::Unsupported("pipeline shape is outside the VM subset"),
            ));
        }
        self.validate_vm_subset_command(&pipeline.commands[0])
    }

    fn validate_vm_subset_command(&self, cmd: &HirCommand) -> Result<(), VmSubsetFallbackReason> {
        match cmd {
            HirCommand::Assign(assign) => {
                if !assign.redirections.is_empty()
                    || assign
                        .assignments
                        .iter()
                        .any(|assignment| !Self::vm_supported_assignment_name(&assignment.name))
                    || assign
                        .assignments
                        .iter()
                        .filter_map(|assignment| assignment.value.as_ref())
                        .any(|word| !Self::vm_supported_word(word))
                {
                    return Err(VmSubsetFallbackReason::AssignmentShape);
                }
                Ok(())
            }
            HirCommand::Exec(exec) => {
                if !exec.env.is_empty() {
                    return Err(VmSubsetFallbackReason::CommandEnvPrefixes);
                }
                if exec.argv.is_empty()
                    || exec.argv.iter().any(|word| !Self::vm_supported_word(word))
                {
                    return Err(VmSubsetFallbackReason::UnsupportedWord);
                }
                if exec
                    .redirections
                    .iter()
                    .any(|redir| !Self::vm_supported_redirection(redir))
                {
                    return Err(VmSubsetFallbackReason::UnsupportedRedirection);
                }
                if self.vm.state.get_var("SHOPT_x").as_deref() == Some("1")
                    || exec
                        .argv
                        .iter()
                        .any(Self::vm_word_requires_full_shell_execution)
                {
                    return Err(VmSubsetFallbackReason::ShellExpansion);
                }
                let Some(name) = Self::literal_word_text(&exec.argv[0]) else {
                    return Err(VmSubsetFallbackReason::UnsupportedWord);
                };
                if self.get_shopt_value("expand_aliases")
                    && self.aliases.contains_key(name.as_str())
                {
                    return Err(VmSubsetFallbackReason::AliasExpansion);
                }
                let argv = vec![name.to_string()];
                if !matches!(
                    self.resolve_command(name.as_str(), &argv),
                    ResolvedCommand::Builtin
                ) {
                    return Err(VmSubsetFallbackReason::NonBuiltinCommand);
                }
                Ok(())
            }
            _ => Err(VmSubsetFallbackReason::Lowering(
                LoweringError::Unsupported("command kind is outside the VM subset"),
            )),
        }
    }

    fn vm_supported_assignment_name(name: &smol_str::SmolStr) -> bool {
        !name.as_str().contains('[') && !name.as_str().ends_with('+')
    }

    fn vm_supported_redirection(redirection: &HirRedirection) -> bool {
        matches!(
            redirection.op,
            RedirectionOp::Output | RedirectionOp::Append
        ) && redirection.fd.unwrap_or(1) == 1
            && redirection.here_doc_body.is_none()
            && Self::vm_supported_word(&redirection.target)
    }

    fn vm_supported_word(word: &Word) -> bool {
        word.parts.iter().all(Self::vm_supported_word_part)
    }

    fn vm_word_requires_full_shell_execution(word: &Word) -> bool {
        word.parts
            .iter()
            .any(Self::vm_word_part_requires_full_shell_execution)
    }

    fn vm_word_part_requires_full_shell_execution(part: &WordPart) -> bool {
        match part {
            WordPart::Literal(text) => Self::text_has_brace_or_glob_literal(text),
            WordPart::SingleQuoted(_)
            | WordPart::DoubleQuoted(_)
            | WordPart::Parameter(_)
            | WordPart::Arithmetic(_) => false,
            WordPart::CommandSubstitution(_)
            | WordPart::ProcessSubstIn(_)
            | WordPart::ProcessSubstOut(_)
            | _ => true,
        }
    }

    fn vm_supported_word_part(part: &WordPart) -> bool {
        match part {
            WordPart::Literal(_)
            | WordPart::SingleQuoted(_)
            | WordPart::Parameter(_)
            | WordPart::Arithmetic(_) => true,
            WordPart::DoubleQuoted(parts) => parts.iter().all(Self::vm_supported_word_part),
            WordPart::CommandSubstitution(_)
            | WordPart::ProcessSubstIn(_)
            | WordPart::ProcessSubstOut(_)
            | _ => false,
        }
    }

    fn literal_word_text(word: &Word) -> Option<smol_str::SmolStr> {
        fn append_literal(part: &WordPart, out: &mut String) -> Option<()> {
            match part {
                WordPart::Literal(text) | WordPart::SingleQuoted(text) => {
                    out.push_str(text);
                    Some(())
                }
                WordPart::DoubleQuoted(parts) => {
                    for part in parts {
                        append_literal(part, out)?;
                    }
                    Some(())
                }
                _ => None,
            }
        }

        let mut text = String::new();
        for part in &word.parts {
            append_literal(part, &mut text)?;
        }
        Some(text.into())
    }

    fn line_number_for_offset(input: &str, offset: usize) -> u32 {
        input
            .as_bytes()
            .iter()
            .take(offset)
            .filter(|&&b| b == b'\n')
            .count() as u32
            + 1
    }

    /// Execute input and return collected events (used by eval/source).
    fn execute_input_inner(&mut self, input: &str) -> Vec<WorkerEvent> {
        self.exec.recursion_depth += 1;
        if let Err(reason) = self
            .vm
            .budget
            .enter_recursion(self.vm.limits.recursion_limit)
        {
            self.exec.recursion_depth -= 1;
            self.mark_budget_exhaustion(reason);
            return vec![WorkerEvent::Stderr(
                b"wasmsh: maximum recursion depth exceeded\n".to_vec(),
            )];
        }
        let result = self.execute_input_inner_impl(input);
        self.exec.recursion_depth -= 1;
        self.vm.budget.exit_recursion();
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
                self.execute_and_or(and_or);
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

    fn run_exit_trap_if_needed(&mut self, events: &mut Vec<WorkerEvent>) {
        let Some(exit_code) = self.exec.exit_requested else {
            return;
        };
        let Some(handler_str) = self.take_exit_trap_handler() else {
            return;
        };
        self.exec.exit_requested = None;
        events.extend(self.execute_input_inner(&handler_str));
        self.exec.exit_requested = Some(exit_code);
    }

    fn take_exit_trap_handler(&mut self) -> Option<String> {
        let handler = self.vm.state.get_var("_TRAP_EXIT")?;
        if handler.is_empty() {
            return None;
        }
        let handler_str = handler.to_string();
        self.vm.state.set_var(
            smol_str::SmolStr::from("_TRAP_EXIT"),
            smol_str::SmolStr::default(),
        );
        Some(handler_str)
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

    fn push_output_capture(&mut self, capture_stdout: bool, capture_stderr: bool) {
        self.exec.output_captures.push(OutputCapture {
            capture_stdout,
            capture_stderr,
            ..OutputCapture::default()
        });
    }

    fn pop_output_capture(&mut self) -> CapturedOutput {
        let capture = self
            .exec
            .output_captures
            .pop()
            .expect("output capture stack underflow");
        CapturedOutput {
            stdout: capture.stdout,
            stderr: capture.stderr,
        }
    }

    fn with_output_capture<T>(
        &mut self,
        capture_stdout: bool,
        capture_stderr: bool,
        f: impl FnOnce(&mut Self) -> T,
    ) -> (T, CapturedOutput) {
        self.push_output_capture(capture_stdout, capture_stderr);
        let result = f(self);
        let captured = self.pop_output_capture();
        (result, captured)
    }

    fn with_exec_io_scope<T>(
        &mut self,
        exec_io: Option<ExecIo>,
        f: impl FnOnce(&mut Self) -> T,
    ) -> T {
        if let Some(exec_io) = exec_io {
            let saved = self.current_exec_io.replace(exec_io);
            let result = f(self);
            let current = self.current_exec_io.take();
            self.current_exec_io = match (saved, current) {
                (Some(mut saved), Some(mut current)) => {
                    let stdin = current.take_stdin();
                    saved.fds_mut().set_input(stdin);
                    Some(saved)
                }
                (saved, _) => saved,
            };
            result
        } else {
            f(self)
        }
    }

    fn append_visible_output_direct(&mut self, data: &[u8], stdout: bool) {
        if stdout {
            self.vm.stdout.extend_from_slice(data);
        } else {
            self.vm.stderr.extend_from_slice(data);
        }
    }

    fn write_output_destination_direct(&mut self, destination: &OutputTarget, data: &[u8]) -> bool {
        match destination {
            OutputTarget::InheritStdout => {
                self.append_visible_output_direct(data, true);
                true
            }
            OutputTarget::InheritStderr => {
                self.append_visible_output_direct(data, false);
                true
            }
            OutputTarget::File { path, sink, .. } => {
                if let Err(err) = sink.borrow_mut().write(data) {
                    let msg = format!("wasmsh: write error: {err}\n");
                    self.emit_visible_stderr_direct(msg.as_bytes());
                    self.vm.diagnostics.push(wasmsh_vm::DiagnosticEvent {
                        level: wasmsh_vm::DiagLevel::Error,
                        category: wasmsh_vm::DiagCategory::Filesystem,
                        message: format!("write failed for {path}: {err}"),
                    });
                }
                false
            }
            OutputTarget::ProcessSubst { path } => {
                if let Some(sink) = self.process_subst_out_sink_mut(path) {
                    sink.write(data);
                } else {
                    let msg = format!("wasmsh: {path}: process substitution sink not found\n");
                    self.emit_visible_stderr_direct(msg.as_bytes());
                }
                false
            }
            OutputTarget::Pipe(pipe) => {
                pipe.borrow_mut().write_all(data);
                false
            }
            OutputTarget::Closed => false,
        }
    }

    fn emit_visible_stderr_direct(&mut self, data: &[u8]) {
        self.append_visible_output_direct(data, false);
        self.account_output(data.len());
    }

    fn route_output(&mut self, data: &[u8], stdout: bool) -> bool {
        let mut routed_stdout = stdout;
        if let Some(exec_io) = self.current_exec_io.as_ref() {
            let destination = exec_io.output_target(stdout);
            match destination {
                OutputTarget::InheritStdout => {
                    routed_stdout = true;
                }
                OutputTarget::InheritStderr => {
                    routed_stdout = false;
                }
                OutputTarget::File { .. }
                | OutputTarget::ProcessSubst { .. }
                | OutputTarget::Pipe(_)
                | OutputTarget::Closed => {
                    return self.write_output_destination_direct(&destination, data);
                }
            }
        }

        for capture in self.exec.output_captures.iter_mut().rev() {
            let should_capture = if routed_stdout {
                capture.capture_stdout
            } else {
                capture.capture_stderr
            };
            if !should_capture {
                continue;
            }
            if routed_stdout {
                capture.stdout.extend_from_slice(data);
            } else {
                capture.stderr.extend_from_slice(data);
            }
            return false;
        }

        if routed_stdout {
            self.vm.stdout.extend_from_slice(data);
        } else {
            self.vm.stderr.extend_from_slice(data);
        }
        true
    }

    fn account_output(&mut self, bytes: usize) {
        self.vm.track_output(bytes as u64);
        self.flag_output_limit_if_needed();
    }

    fn write_stdout(&mut self, data: &[u8]) {
        if self.route_output(data, true) {
            self.account_output(data.len());
        }
    }

    fn write_stderr(&mut self, data: &[u8]) {
        if self.route_output(data, false) {
            self.account_output(data.len());
        }
    }

    fn write_streams(&mut self, stdout: &[u8], stderr: &[u8]) {
        let visible_stdout = self.route_output(stdout, true);
        let visible_stderr = self.route_output(stderr, false);
        let visible_bytes =
            usize::from(visible_stdout) * stdout.len() + usize::from(visible_stderr) * stderr.len();
        if visible_bytes > 0 {
            self.account_output(visible_bytes);
        }
    }

    fn flag_output_limit_if_needed(&mut self) {
        if self.exec.resource_exhausted {
            return;
        }
        if self.vm.check_output_limit().is_err() {
            self.exec.resource_exhausted = true;
        }
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
        self.execute_scheduled_pipeline(cmds, pipeline);
        if pipeline.negated {
            self.vm.state.last_status = i32::from(self.vm.state.last_status == 0);
        }
    }

    fn execute_scheduled_pipeline(&mut self, cmds: &[HirCommand], pipeline: &HirPipeline) {
        self.execute_scheduled_pipeline_with_source_reader(cmds, pipeline, None);
    }

    fn execute_scheduled_pipeline_with_source_reader(
        &mut self,
        cmds: &[HirCommand],
        pipeline: &HirPipeline,
        source_reader: Option<Box<dyn Read>>,
    ) {
        let pipefail = self.vm.state.get_var("SHOPT_o_pipefail").as_deref() == Some("1");
        let stages: Vec<StreamingPipelineStage> = cmds
            .iter()
            .enumerate()
            .map(|(idx, cmd)| self.compile_pipeline_stage(cmd, idx == 0 && source_reader.is_none()))
            .collect();
        if source_reader.is_none() && stages.len() == 1 {
            if self.command_needs_full_single_stage_execution(&cmds[0]) {
                self.execute_command(&cmds[0]);
                let status = self.vm.state.last_status;
                self.set_pipestatus(&[status]);
                return;
            }
            if !matches!(stages[0], StreamingPipelineStage::BufferedCommand(_))
                && !Self::command_requires_runtime_expansion(&cmds[0])
            {
                if let Some(argv) = self.resolve_streaming_pipeline_argv(&cmds[0]) {
                    self.trace_command(&argv);
                }
            }
            let status = self.execute_scheduled_single_stage(&stages[0]);
            self.set_pipestatus(&[status]);
            if !self.exec.resource_exhausted {
                self.vm.state.last_status = status;
            }
            return;
        }
        let stage_statuses: Vec<Rc<RefCell<i32>>> = stages
            .iter()
            .map(|stage| {
                Rc::new(RefCell::new(i32::from(matches!(
                    stage,
                    StreamingPipelineStage::Grep(_)
                ))))
            })
            .collect();
        let stage_stderr: Vec<Rc<RefCell<Vec<u8>>>> = stages
            .iter()
            .map(|_| Rc::new(RefCell::new(Vec::new())))
            .collect();
        let stage_pipe_stderr: Vec<bool> = (0..stages.len())
            .map(|idx| pipeline.pipe_stderr.get(idx).copied().unwrap_or(false))
            .collect();

        self.execute_pipebuffer_streaming_pipeline(
            source_reader,
            &stages,
            &stage_pipe_stderr,
            &stage_statuses,
            &stage_stderr,
        );

        let statuses: Vec<i32> = stage_statuses
            .iter()
            .map(|status| *status.borrow())
            .collect();
        self.set_pipestatus(&statuses);
        if !self.exec.resource_exhausted {
            if pipefail {
                self.vm.state.last_status = statuses
                    .iter()
                    .rev()
                    .copied()
                    .find(|status| *status != 0)
                    .unwrap_or(0);
            } else {
                self.vm.state.last_status = statuses.last().copied().unwrap_or(0);
            }
        }
    }

    fn execute_scheduled_single_stage(&mut self, stage: &StreamingPipelineStage) -> i32 {
        match stage {
            StreamingPipelineStage::Literal(data) => {
                self.write_stdout(data);
                0
            }
            StreamingPipelineStage::File(path) => {
                let resolved = self.resolve_cwd_path(path);
                let Ok(mut reader) = self.open_streaming_file_reader(&resolved, "cat") else {
                    return self.vm.state.last_status;
                };
                let mut buffer = [0u8; 4096];
                loop {
                    match reader.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(read) => {
                            self.write_stdout(&buffer[..read]);
                            if self.exec.resource_exhausted {
                                return 1;
                            }
                        }
                        Err(err) => {
                            self.write_stderr(
                                format!("wasmsh: cat: stdin read error: {err}\n").as_bytes(),
                            );
                            return 1;
                        }
                    }
                }
                0
            }
            StreamingPipelineStage::Yes { line } => {
                for _ in 0..STREAMING_YES_MAX_LINES {
                    self.write_stdout(line);
                    if self.exec.resource_exhausted {
                        return 1;
                    }
                }
                0
            }
            StreamingPipelineStage::BufferedCommand(BufferedPipelineCommand::Argv(argv)) => {
                self.trace_command(argv);
                self.execute_argv_command(argv);
                self.vm.state.last_status
            }
            StreamingPipelineStage::BufferedCommand(BufferedPipelineCommand::Hir(cmd)) => {
                self.execute_command(cmd);
                self.vm.state.last_status
            }
            _ => {
                self.vm.state.last_status = 1;
                self.write_stderr(b"wasmsh: unsupported single-stage scheduler node\n");
                1
            }
        }
    }

    fn compile_pipeline_stage(
        &mut self,
        cmd: &HirCommand,
        is_first: bool,
    ) -> StreamingPipelineStage {
        if let Some(argv) = self.resolve_streaming_pipeline_argv(cmd) {
            if self.get_shopt_value("expand_aliases")
                && argv
                    .first()
                    .is_some_and(|name| self.aliases.contains_key(name))
            {
                return StreamingPipelineStage::BufferedCommand(BufferedPipelineCommand::Hir(
                    cmd.clone(),
                ));
            }
            if argv
                .first()
                .is_some_and(|name| self.functions.contains_key(name))
            {
                return StreamingPipelineStage::BufferedCommand(BufferedPipelineCommand::Argv(
                    argv,
                ));
            }
            if let Some(stage) = self.parse_streaming_stage(&argv, is_first) {
                if Self::uses_native_pipe_scheduler(&stage) {
                    return stage;
                }
                return StreamingPipelineStage::BufferedCommand(BufferedPipelineCommand::Argv(
                    argv,
                ));
            }
            return StreamingPipelineStage::BufferedCommand(BufferedPipelineCommand::Hir(
                cmd.clone(),
            ));
        }
        StreamingPipelineStage::BufferedCommand(BufferedPipelineCommand::Hir(cmd.clone()))
    }

    fn uses_native_pipe_scheduler(stage: &StreamingPipelineStage) -> bool {
        !matches!(stage, StreamingPipelineStage::BufferedCommand(_))
    }

    fn execute_pipebuffer_streaming_pipeline(
        &mut self,
        source_reader: Option<Box<dyn Read>>,
        stages: &[StreamingPipelineStage],
        stage_pipe_stderr: &[bool],
        stage_statuses: &[Rc<RefCell<i32>>],
        stage_stderr: &[Rc<RefCell<Vec<u8>>>],
    ) -> bool {
        let mut processes = Vec::new();
        let output_pipes: Vec<Rc<RefCell<PipeBuffer>>> = (0..stages.len())
            .map(|_| Rc::new(RefCell::new(PipeBuffer::new(PIPEBUFFER_STREAMING_CAPACITY))))
            .collect();
        if let Some(source_reader) = source_reader {
            let source_pipe = Rc::new(RefCell::new(PipeBuffer::new(PIPEBUFFER_STREAMING_CAPACITY)));
            let source_stderr = Rc::new(RefCell::new(Vec::new()));
            let source_status = Rc::new(RefCell::new(0));
            processes.push(StreamingPipeProcess::Read(PipeReadProcess::new(
                source_reader,
                source_pipe.clone(),
                source_stderr,
                source_status,
                "source",
                false,
            )));
            match &stages[0] {
                StreamingPipelineStage::Tee(stage) => {
                    let reader = Box::new(PipeReader::new(source_pipe)) as Box<dyn Read>;
                    processes.push(StreamingPipeProcess::Tee(TeePipeProcess::new(
                        reader,
                        output_pipes[0].clone(),
                        &mut self.fs,
                        self.vm.state.cwd.as_str(),
                        stage,
                        stage_stderr[0].clone(),
                        stage_statuses[0].clone(),
                        stage_pipe_stderr[0],
                    )));
                }
                StreamingPipelineStage::BufferedCommand(argv) => {
                    processes.push(StreamingPipeProcess::Buffered(BufferedPipeProcess::new(
                        Some(source_pipe),
                        output_pipes[0].clone(),
                        argv.clone(),
                        stage_pipe_stderr[0],
                        stage_stderr[0].clone(),
                        stage_statuses[0].clone(),
                    )));
                }
                _ => {
                    let reader = Box::new(PipeReader::new(source_pipe)) as Box<dyn Read>;
                    let Some(stage_reader) =
                        Self::wrap_non_tee_streaming_stage(reader, &stages[0], 0, stage_statuses)
                    else {
                        return false;
                    };
                    processes.push(StreamingPipeProcess::Read(PipeReadProcess::new(
                        stage_reader,
                        output_pipes[0].clone(),
                        stage_stderr[0].clone(),
                        stage_statuses[0].clone(),
                        "stage",
                        stage_pipe_stderr[0],
                    )));
                }
            }
        } else {
            match &stages[0] {
                StreamingPipelineStage::Literal(data) => {
                    let first_reader: Box<dyn Read + '_> = Box::new(Cursor::new(data.clone()));
                    processes.push(StreamingPipeProcess::Read(PipeReadProcess::new(
                        first_reader,
                        output_pipes[0].clone(),
                        stage_stderr[0].clone(),
                        stage_statuses[0].clone(),
                        "source",
                        stage_pipe_stderr[0],
                    )));
                }
                StreamingPipelineStage::File(path) => {
                    let resolved = self.resolve_cwd_path(path);
                    let Ok(first_reader) = self.open_streaming_file_reader(&resolved, "cat") else {
                        *stage_statuses[0].borrow_mut() = self.vm.state.last_status;
                        return true;
                    };
                    processes.push(StreamingPipeProcess::Read(PipeReadProcess::new(
                        first_reader,
                        output_pipes[0].clone(),
                        stage_stderr[0].clone(),
                        stage_statuses[0].clone(),
                        "source",
                        stage_pipe_stderr[0],
                    )));
                }
                StreamingPipelineStage::Yes { line } => {
                    let first_reader: Box<dyn Read + '_> =
                        Box::new(YesStreamReader::new(line.clone(), STREAMING_YES_MAX_LINES));
                    processes.push(StreamingPipeProcess::Read(PipeReadProcess::new(
                        first_reader,
                        output_pipes[0].clone(),
                        stage_stderr[0].clone(),
                        stage_statuses[0].clone(),
                        "source",
                        stage_pipe_stderr[0],
                    )));
                }
                StreamingPipelineStage::BufferedCommand(argv) => {
                    processes.push(StreamingPipeProcess::Buffered(BufferedPipeProcess::new(
                        None,
                        output_pipes[0].clone(),
                        argv.clone(),
                        stage_pipe_stderr[0],
                        stage_stderr[0].clone(),
                        stage_statuses[0].clone(),
                    )));
                }
                _ => unreachable!("unexpected first pipeline stage"),
            }
        }

        for idx in 1..stages.len() {
            match &stages[idx] {
                StreamingPipelineStage::Head(mode) => {
                    processes.push(StreamingPipeProcess::Head(HeadPipeProcess::new(
                        output_pipes[idx - 1].clone(),
                        output_pipes[idx].clone(),
                        *mode,
                    )));
                }
                StreamingPipelineStage::Tee(stage) => {
                    let reader =
                        Box::new(PipeReader::new(output_pipes[idx - 1].clone())) as Box<dyn Read>;
                    processes.push(StreamingPipeProcess::Tee(TeePipeProcess::new(
                        reader,
                        output_pipes[idx].clone(),
                        &mut self.fs,
                        self.vm.state.cwd.as_str(),
                        stage,
                        stage_stderr[idx].clone(),
                        stage_statuses[idx].clone(),
                        stage_pipe_stderr[idx],
                    )));
                }
                StreamingPipelineStage::BufferedCommand(argv) => {
                    processes.push(StreamingPipeProcess::Buffered(BufferedPipeProcess::new(
                        Some(output_pipes[idx - 1].clone()),
                        output_pipes[idx].clone(),
                        argv.clone(),
                        stage_pipe_stderr[idx],
                        stage_stderr[idx].clone(),
                        stage_statuses[idx].clone(),
                    )));
                }
                _ => {
                    let reader =
                        Box::new(PipeReader::new(output_pipes[idx - 1].clone())) as Box<dyn Read>;
                    let Some(stage_reader) = Self::wrap_non_tee_streaming_stage(
                        reader,
                        &stages[idx],
                        idx,
                        stage_statuses,
                    ) else {
                        return false;
                    };
                    processes.push(StreamingPipeProcess::Read(PipeReadProcess::new(
                        stage_reader,
                        output_pipes[idx].clone(),
                        stage_stderr[idx].clone(),
                        stage_statuses[idx].clone(),
                        "stage",
                        stage_pipe_stderr[idx],
                    )));
                }
            }
        }
        let final_pipe = output_pipes
            .last()
            .cloned()
            .expect("final pipe missing for streaming pipeline");

        let mut finished = vec![false; processes.len()];
        loop {
            if self.check_resource_limits() {
                final_pipe.borrow_mut().close_read();
                break;
            }

            let mut progressed = false;
            for idx in (0..processes.len()).rev() {
                if finished[idx] {
                    continue;
                }
                match processes[idx].poll(self) {
                    PipeProcessPoll::Ready => progressed = true,
                    PipeProcessPoll::PendingRead | PipeProcessPoll::PendingWrite => {}
                    PipeProcessPoll::Exited => {
                        finished[idx] = true;
                        progressed = true;
                    }
                }
            }

            let buffered_pipe_bytes = output_pipes
                .iter()
                .map(|pipe| pipe.borrow().len() as u64)
                .sum();
            self.sync_pipe_budget(buffered_pipe_bytes);
            if self.exec.resource_exhausted {
                final_pipe.borrow_mut().close_read();
                break;
            }

            loop {
                let mut buffer = [0u8; 4096];
                let read_result = {
                    let mut pipe = final_pipe.borrow_mut();
                    pipe.read(&mut buffer)
                };
                match read_result {
                    ReadResult::Read(read) => {
                        self.write_stdout(&buffer[..read]);
                        progressed = true;
                        if self.exec.resource_exhausted {
                            final_pipe.borrow_mut().close_read();
                            break;
                        }
                    }
                    ReadResult::WouldBlock | ReadResult::Eof => break,
                }
            }

            if self.exec.resource_exhausted {
                break;
            }

            if finished.iter().all(|done| *done) {
                break;
            }
            if !progressed {
                break;
            }
        }

        for process in &mut processes {
            process.close(self);
        }

        for (idx, stderr) in stage_stderr.iter().enumerate() {
            if stage_pipe_stderr[idx] {
                continue;
            }
            let data = stderr.borrow();
            if !data.is_empty() {
                self.write_stderr(&data);
            }
        }
        true
    }

    fn wrap_non_tee_streaming_stage<'a>(
        reader: Box<dyn Read + 'a>,
        stage: &StreamingPipelineStage,
        idx: usize,
        stage_statuses: &[Rc<RefCell<i32>>],
    ) -> Option<Box<dyn Read + 'a>> {
        match stage {
            StreamingPipelineStage::Cat => Some(reader),
            StreamingPipelineStage::Head(mode) => Some(match mode {
                StreamingHeadMode::Lines(limit) => Box::new(HeadStreamReader::new(
                    reader,
                    StreamingHeadMode::Lines(*limit),
                )),
                StreamingHeadMode::Bytes(limit) => Box::new(HeadStreamReader::new(
                    reader,
                    StreamingHeadMode::Bytes(*limit),
                )),
            }),
            StreamingPipelineStage::Tail(mode) => Some(match mode {
                StreamingTailMode::Lines(limit) => Box::new(TailStreamReader::new(
                    reader,
                    StreamingTailMode::Lines(*limit),
                )),
                StreamingTailMode::Bytes(limit) => Box::new(TailStreamReader::new(
                    reader,
                    StreamingTailMode::Bytes(*limit),
                )),
            }),
            StreamingPipelineStage::Bat(stage) => {
                Some(Box::new(BatStreamReader::new(reader, *stage)))
            }
            StreamingPipelineStage::Sed(stage) => {
                Some(Box::new(SedStreamReader::new(reader, stage.clone())))
            }
            StreamingPipelineStage::Paste(stage) => {
                Some(Box::new(PasteStreamReader::new(reader, stage.clone())))
            }
            StreamingPipelineStage::Column(_) => Some(Box::new(ColumnStreamReader::new(reader))),
            StreamingPipelineStage::Grep(stage) => Some(Box::new(GrepStreamReader::new(
                reader,
                stage.clone(),
                stage_statuses[idx].clone(),
            ))),
            StreamingPipelineStage::Uniq(flags) => {
                Some(Box::new(UniqStreamReader::new(reader, flags.clone())))
            }
            StreamingPipelineStage::Rev => Some(Box::new(RevStreamReader::new(reader))),
            StreamingPipelineStage::Cut(stage) => {
                Some(Box::new(CutStreamReader::new(reader, stage.clone())))
            }
            StreamingPipelineStage::Tr(stage) => {
                Some(Box::new(TrStreamReader::new(reader, stage.clone())))
            }
            StreamingPipelineStage::Wc(flags) => {
                Some(Box::new(WcStreamReader::new(reader, *flags)))
            }
            StreamingPipelineStage::Tee(_)
            | StreamingPipelineStage::Literal(_)
            | StreamingPipelineStage::File(_)
            | StreamingPipelineStage::Yes { .. }
            | StreamingPipelineStage::BufferedCommand(_) => None,
        }
    }

    fn resolve_streaming_pipeline_argv(&mut self, cmd: &HirCommand) -> Option<Vec<String>> {
        let HirCommand::Exec(exec) = cmd else {
            return None;
        };
        if !exec.env.is_empty()
            || !exec.redirections.is_empty()
            || Self::command_requires_runtime_expansion(cmd)
        {
            return None;
        }
        let resolved = self.resolve_command_subst(&exec.argv);
        if self.exec.expansion_failed {
            return None;
        }
        let expanded = expand_words_argv(&resolved, &mut self.vm.state);
        if self.check_nounset_error() || expanded.is_empty() {
            return None;
        }
        let tagged: Vec<(String, bool)> = expanded
            .into_iter()
            .flat_map(|ew| {
                if ew.was_quoted {
                    vec![(ew.text, true)]
                } else {
                    wasmsh_expand::expand_braces(&ew.text)
                        .into_iter()
                        .map(|s| (s, false))
                        .collect()
                }
            })
            .collect();
        Some(self.expand_globs_tagged(tagged))
    }

    fn parse_streaming_stage(
        &self,
        argv: &[String],
        is_first: bool,
    ) -> Option<StreamingPipelineStage> {
        let cmd_name = argv.first()?.as_str();
        match cmd_name {
            "echo" if is_first => Some(StreamingPipelineStage::Literal(
                Self::streaming_echo_bytes(&argv[1..]),
            )),
            "yes" if is_first => {
                let text = if argv.len() > 1 {
                    argv[1..].join(" ")
                } else {
                    "y".to_string()
                };
                Some(StreamingPipelineStage::Yes {
                    line: format!("{text}\n").into_bytes(),
                })
            }
            "cat" => Self::parse_streaming_cat_stage(&argv[1..], is_first),
            "head" if !is_first => Self::parse_streaming_head_stage(&argv[1..]),
            "tail" if !is_first => Self::parse_streaming_tail_stage(&argv[1..]),
            "bat" if !is_first => Self::parse_streaming_bat_stage(&argv[1..]),
            "sed" if !is_first => Self::parse_streaming_sed_stage(&argv[1..]),
            "tee" if !is_first => Self::parse_streaming_tee_stage(&argv[1..]),
            "paste" if !is_first => Self::parse_streaming_paste_stage(&argv[1..]),
            "column" if !is_first => Self::parse_streaming_column_stage(&argv[1..]),
            "grep" if !is_first => Self::parse_streaming_grep_stage(&argv[1..]),
            "uniq" if !is_first => Self::parse_streaming_uniq_stage(&argv[1..]),
            "rev" if !is_first => Self::parse_streaming_rev_stage(&argv[1..]),
            "cut" if !is_first => Self::parse_streaming_cut_stage(&argv[1..]),
            "tr" if !is_first => Self::parse_streaming_tr_stage(&argv[1..]),
            "wc" if !is_first => Self::parse_streaming_wc_stage(&argv[1..]),
            _ if cmd_name == "bash"
                || cmd_name == "sh"
                || cmd_name == "builtin"
                || self.functions.contains_key(cmd_name)
                || self.builtins.is_builtin(cmd_name)
                || self.utils.is_utility(cmd_name)
                || self.external_handler.is_some() =>
            {
                Some(StreamingPipelineStage::BufferedCommand(
                    BufferedPipelineCommand::Argv(argv.to_vec()),
                ))
            }
            _ => None,
        }
    }

    fn streaming_echo_bytes(args: &[String]) -> Vec<u8> {
        let mut suppress_newline = false;
        let mut interpret_escapes = false;
        let mut start = 0usize;

        for (i, arg) in args.iter().enumerate() {
            let bytes = arg.as_bytes();
            if bytes.first() != Some(&b'-') || bytes.len() < 2 {
                break;
            }
            if !bytes[1..].iter().all(|b| matches!(b, b'n' | b'e')) {
                break;
            }
            for &byte in &bytes[1..] {
                match byte {
                    b'n' => suppress_newline = true,
                    b'e' => interpret_escapes = true,
                    _ => {}
                }
            }
            start = i + 1;
        }

        let text = args[start..].join(" ");
        let rendered = if interpret_escapes {
            Self::process_streaming_echo_escapes(&text)
        } else {
            text
        };
        let mut output = rendered.into_bytes();
        if !suppress_newline {
            output.push(b'\n');
        }
        output
    }

    fn process_streaming_echo_escapes(text: &str) -> String {
        let bytes = text.as_bytes();
        let mut output = String::new();
        let mut i = 0usize;
        while i < bytes.len() {
            if bytes[i] == b'\\' && i + 1 < bytes.len() {
                match bytes[i + 1] {
                    b'n' => output.push('\n'),
                    b't' => output.push('\t'),
                    b'r' => output.push('\r'),
                    b'\\' => output.push('\\'),
                    other => {
                        output.push('\\');
                        output.push(other as char);
                    }
                }
                i += 2;
            } else {
                output.push(bytes[i] as char);
                i += 1;
            }
        }
        output
    }

    fn parse_streaming_cat_stage(
        args: &[String],
        is_first: bool,
    ) -> Option<StreamingPipelineStage> {
        let non_separator: Vec<&String> = args.iter().filter(|arg| arg.as_str() != "--").collect();
        if non_separator.iter().any(|arg| arg.starts_with('-')) {
            return None;
        }
        if is_first {
            if non_separator.len() == 1 {
                return Some(StreamingPipelineStage::File(non_separator[0].clone()));
            }
            return None;
        }
        Some(StreamingPipelineStage::Cat)
    }

    fn parse_streaming_head_stage(args: &[String]) -> Option<StreamingPipelineStage> {
        let mut mode = StreamingHeadMode::Lines(10);
        let mut files = Vec::new();
        let mut i = 0usize;
        while i < args.len() {
            let arg = args[i].as_str();
            if arg == "-c" && i + 1 < args.len() {
                mode = StreamingHeadMode::Bytes(args[i + 1].parse().ok()?);
                i += 2;
            } else if arg == "-n" && i + 1 < args.len() {
                mode = StreamingHeadMode::Lines(args[i + 1].parse().ok()?);
                i += 2;
            } else if arg.starts_with('-') && arg.len() > 1 && arg != "--" {
                if let Ok(lines) = arg[1..].parse::<usize>() {
                    mode = StreamingHeadMode::Lines(lines);
                } else {
                    return None;
                }
                i += 1;
            } else if arg == "--" {
                i += 1;
            } else {
                files.push(arg);
                i += 1;
            }
        }
        if files.is_empty() {
            Some(StreamingPipelineStage::Head(mode))
        } else {
            None
        }
    }

    fn parse_streaming_tail_stage(args: &[String]) -> Option<StreamingPipelineStage> {
        let mut mode = StreamingTailMode::Lines(10);
        let mut files = Vec::new();
        let mut i = 0usize;
        while i < args.len() {
            let arg = args[i].as_str();
            if arg == "-c" && i + 1 < args.len() {
                mode = StreamingTailMode::Bytes(args[i + 1].parse().ok()?);
                i += 2;
            } else if arg == "-n" && i + 1 < args.len() {
                let value = args[i + 1].as_str();
                if value.starts_with('+') {
                    return None;
                }
                mode = StreamingTailMode::Lines(value.parse().ok()?);
                i += 2;
            } else if arg == "-f" {
                return None;
            } else if arg.starts_with('-') && arg.len() > 1 && arg != "--" {
                if let Ok(lines) = arg[1..].parse::<usize>() {
                    mode = StreamingTailMode::Lines(lines);
                } else {
                    return None;
                }
                i += 1;
            } else if arg == "--" {
                i += 1;
            } else {
                files.push(arg);
                i += 1;
            }
        }
        if files.is_empty() {
            Some(StreamingPipelineStage::Tail(mode))
        } else {
            None
        }
    }

    fn parse_streaming_bat_stage(args: &[String]) -> Option<StreamingPipelineStage> {
        let mut stage = StreamingBatStage {
            show_numbers: true,
            show_header: true,
            line_range: None,
            show_all: false,
        };
        let mut i = 0usize;
        while i < args.len() {
            let arg = args[i].as_str();
            match arg {
                "-n" | "--number" => {
                    stage.show_numbers = true;
                    i += 1;
                }
                "-p" | "--plain" | "--style=plain" => {
                    stage.show_numbers = false;
                    stage.show_header = false;
                    i += 1;
                }
                "-A" | "--show-all" => {
                    stage.show_all = true;
                    i += 1;
                }
                "-r" | "--line-range" if i + 1 < args.len() => {
                    stage.line_range = Self::parse_streaming_bat_range(&args[i + 1]);
                    i += 2;
                }
                "-l" | "--language" | "--paging" if i + 1 < args.len() => {
                    i += 2;
                }
                "--style=numbers" => {
                    stage.show_numbers = true;
                    stage.show_header = false;
                    i += 1;
                }
                "--style=header" => {
                    stage.show_numbers = false;
                    stage.show_header = true;
                    i += 1;
                }
                value if value.starts_with("--style=") => {
                    stage.show_numbers = true;
                    stage.show_header = true;
                    i += 1;
                }
                value if value.starts_with("--line-range=") => {
                    stage.line_range =
                        Self::parse_streaming_bat_range(&value["--line-range=".len()..]);
                    i += 1;
                }
                value if value.starts_with("--paging=") || value.starts_with("--language=") => {
                    i += 1;
                }
                value if value.starts_with('-') && value.len() > 1 && !value.starts_with("--") => {
                    for ch in value[1..].chars() {
                        match ch {
                            'n' => stage.show_numbers = true,
                            'p' => {
                                stage.show_numbers = false;
                                stage.show_header = false;
                            }
                            'A' => stage.show_all = true,
                            _ => return None,
                        }
                    }
                    i += 1;
                }
                "--" => {
                    if i + 1 != args.len() {
                        return None;
                    }
                    i += 1;
                }
                _ => return None,
            }
        }
        Some(StreamingPipelineStage::Bat(stage))
    }

    fn parse_streaming_bat_range(s: &str) -> Option<(Option<usize>, Option<usize>)> {
        if let Some((start, end)) = s.split_once(':') {
            let start = if start.is_empty() {
                None
            } else {
                start.parse().ok()
            };
            let end = if end.is_empty() {
                None
            } else {
                end.parse().ok()
            };
            Some((start, end))
        } else {
            let n = s.parse().ok()?;
            Some((Some(n), Some(n)))
        }
    }

    fn parse_streaming_sed_stage(args: &[String]) -> Option<StreamingPipelineStage> {
        let mut suppress_print = false;
        let mut expressions = Vec::new();
        let mut i = 0usize;
        while i < args.len() {
            let arg = args[i].as_str();
            if arg == "-n" {
                suppress_print = true;
                i += 1;
            } else if arg == "-e" && i + 1 < args.len() {
                expressions.push(args[i + 1].clone());
                i += 2;
            } else if arg == "-E" || arg == "-r" {
                i += 1;
            } else if arg == "-f"
                || arg == "-i"
                || arg.starts_with("-i")
                || (arg.starts_with('-') && arg.len() > 1 && arg != "--")
            {
                return None;
            } else if arg == "--" {
                if i + 1 >= args.len() {
                    break;
                }
                if expressions.is_empty() {
                    expressions.push(args[i + 1].clone());
                    i += 2;
                } else {
                    return None;
                }
            } else if expressions.is_empty() {
                expressions.push(args[i].clone());
                i += 1;
            } else {
                return None;
            }
        }
        if expressions.is_empty() {
            return None;
        }
        let script = expressions.join(";");
        let instructions = parse_streaming_sed_script(&script);
        if instructions.is_empty() {
            return None;
        }
        Some(StreamingPipelineStage::Sed(StreamingSedStage {
            suppress_print,
            instructions,
        }))
    }

    fn parse_streaming_paste_stage(args: &[String]) -> Option<StreamingPipelineStage> {
        let mut delimiter = "\t".to_string();
        let mut serial = false;
        let mut i = 0usize;
        while i < args.len() {
            let arg = args[i].as_str();
            if arg == "-d" && i + 1 < args.len() {
                delimiter.clone_from(&args[i + 1]);
                i += 2;
            } else if arg == "-s" {
                serial = true;
                i += 1;
            } else if arg.starts_with('-') && arg.len() > 1 {
                for ch in arg[1..].chars() {
                    match ch {
                        's' => serial = true,
                        'd' => {
                            if i + 1 < args.len() {
                                delimiter.clone_from(&args[i + 1]);
                                i += 1;
                            } else {
                                return None;
                            }
                        }
                        _ => return None,
                    }
                }
                i += 1;
            } else if arg == "--" {
                if i + 1 != args.len() {
                    return None;
                }
                i += 1;
            } else {
                return None;
            }
        }
        Some(StreamingPipelineStage::Paste(StreamingPasteStage {
            delimiter,
            serial,
        }))
    }

    fn parse_streaming_tee_stage(args: &[String]) -> Option<StreamingPipelineStage> {
        let mut append = false;
        let mut paths = Vec::new();
        let mut i = 0usize;
        while i < args.len() {
            let arg = args[i].as_str();
            if arg == "-a" {
                append = true;
                i += 1;
            } else if arg == "-i" {
                i += 1;
            } else if arg == "--" {
                paths.extend(args[i + 1..].iter().cloned());
                break;
            } else if arg.starts_with('-') && arg.len() > 1 {
                for ch in arg[1..].chars() {
                    match ch {
                        'a' => append = true,
                        'i' => {}
                        _ => return None,
                    }
                }
                i += 1;
            } else {
                paths.push(args[i].clone());
                i += 1;
            }
        }
        Some(StreamingPipelineStage::Tee(StreamingTeeStage {
            append,
            paths,
        }))
    }

    fn parse_streaming_column_stage(args: &[String]) -> Option<StreamingPipelineStage> {
        let mut i = 0usize;
        while i < args.len() {
            let arg = args[i].as_str();
            if arg == "-t" {
                return None;
            }
            if arg == "-s" && i + 1 < args.len() {
                return None;
            }
            if arg.starts_with('-') && arg.len() > 1 {
                i += 1;
            } else if arg == "--" {
                if i + 1 != args.len() {
                    return None;
                }
                i += 1;
            } else {
                return None;
            }
        }
        Some(StreamingPipelineStage::Column(StreamingColumnStage))
    }

    fn parse_streaming_rev_stage(args: &[String]) -> Option<StreamingPipelineStage> {
        if args.iter().all(|arg| arg == "--") {
            Some(StreamingPipelineStage::Rev)
        } else {
            None
        }
    }

    fn parse_streaming_grep_stage(args: &[String]) -> Option<StreamingPipelineStage> {
        let mut flags = StreamingGrepFlags {
            ignore_case: false,
            invert: false,
            count_only: false,
            show_line_numbers: false,
            files_only: false,
            word_match: false,
            only_matching: false,
            quiet: false,
            extended: false,
            fixed: false,
            after_context: 0,
            before_context: 0,
            max_count: None,
            show_filename: None,
        };
        let mut patterns = Vec::new();
        let mut rest = Vec::new();
        let mut i = 0usize;
        while i < args.len() {
            let arg = args[i].as_str();
            if arg == "--" {
                rest.extend(args[i + 1..].iter().cloned());
                break;
            }
            if arg.starts_with("--include=")
                || arg.starts_with("--exclude=")
                || arg == "--color"
                || arg.starts_with("--color=")
                || arg == "-r"
                || arg == "-R"
                || arg == "--recursive"
            {
                return None;
            }
            if arg == "-e" && i + 1 < args.len() {
                patterns.push(args[i + 1].clone());
                i += 2;
                continue;
            }
            if arg == "-f" && i + 1 < args.len() {
                return None;
            }
            if arg == "-A" && i + 1 < args.len() {
                flags.after_context = args[i + 1].parse().ok()?;
                i += 2;
                continue;
            }
            if arg == "-B" && i + 1 < args.len() {
                flags.before_context = args[i + 1].parse().ok()?;
                i += 2;
                continue;
            }
            if arg == "-C" && i + 1 < args.len() {
                let n = args[i + 1].parse().ok()?;
                flags.before_context = n;
                flags.after_context = n;
                i += 2;
                continue;
            }
            if arg == "-m" && i + 1 < args.len() {
                flags.max_count = args[i + 1].parse().ok();
                i += 2;
                continue;
            }
            if arg.starts_with('-') && arg.len() > 1 {
                for ch in arg[1..].chars() {
                    match ch {
                        'i' => flags.ignore_case = true,
                        'v' => flags.invert = true,
                        'c' => flags.count_only = true,
                        'n' => flags.show_line_numbers = true,
                        'l' => flags.files_only = true,
                        'E' | 'P' => flags.extended = true,
                        'F' => flags.fixed = true,
                        'w' => flags.word_match = true,
                        'o' => flags.only_matching = true,
                        'q' => flags.quiet = true,
                        'h' => flags.show_filename = Some(false),
                        'H' => flags.show_filename = Some(true),
                        'z' => {}
                        _ => return None,
                    }
                }
                i += 1;
            } else {
                rest.push(args[i].clone());
                i += 1;
            }
        }

        let (patterns, file_args) = if patterns.is_empty() {
            let first = rest.first()?.clone();
            (vec![first], rest[1..].to_vec())
        } else {
            (patterns, rest)
        };
        if !file_args.is_empty() {
            return None;
        }
        Some(StreamingPipelineStage::Grep(StreamingGrepStage {
            flags,
            patterns,
        }))
    }

    fn parse_streaming_uniq_stage(args: &[String]) -> Option<StreamingPipelineStage> {
        let mut flags = StreamingUniqFlags {
            count: false,
            duplicates_only: false,
            unique_only: false,
            ignore_case: false,
            skip_fields: 0,
            skip_chars: 0,
            compare_chars: None,
        };
        let mut i = 0usize;
        while i < args.len() {
            let arg = args[i].as_str();
            if arg == "-f" && i + 1 < args.len() {
                flags.skip_fields = args[i + 1].parse().ok()?;
                i += 2;
            } else if arg == "-s" && i + 1 < args.len() {
                flags.skip_chars = args[i + 1].parse().ok()?;
                i += 2;
            } else if arg == "-w" && i + 1 < args.len() {
                flags.compare_chars = args[i + 1].parse().ok();
                i += 2;
            } else if arg.starts_with('-') && arg.len() > 1 && arg != "--" {
                for ch in arg[1..].chars() {
                    match ch {
                        'c' => flags.count = true,
                        'd' => flags.duplicates_only = true,
                        'u' => flags.unique_only = true,
                        'i' => flags.ignore_case = true,
                        'z' => {}
                        _ => return None,
                    }
                }
                i += 1;
            } else if arg == "--" {
                i += 1;
            } else {
                return None;
            }
        }
        Some(StreamingPipelineStage::Uniq(flags))
    }

    fn parse_streaming_cut_ranges(spec: &str) -> Vec<StreamingCutRange> {
        spec.split(',')
            .filter_map(|part| {
                if let Some((start, end)) = part.split_once('-') {
                    Some(StreamingCutRange {
                        start: if start.is_empty() {
                            None
                        } else {
                            start.parse().ok()
                        },
                        end: if end.is_empty() {
                            None
                        } else {
                            end.parse().ok()
                        },
                    })
                } else {
                    let n: usize = part.parse().ok()?;
                    Some(StreamingCutRange {
                        start: Some(n),
                        end: Some(n),
                    })
                }
            })
            .collect()
    }

    fn parse_streaming_cut_stage(args: &[String]) -> Option<StreamingPipelineStage> {
        let mut delim = '\t';
        let mut mode = None;
        let mut complement = false;
        let mut only_delimited = false;
        let mut output_delim = None;
        let mut i = 0usize;

        while i < args.len() {
            let arg = args[i].as_str();
            if arg == "-d" && i + 1 < args.len() {
                delim = args[i + 1].chars().next().unwrap_or('\t');
                i += 2;
            } else if arg.starts_with("-d") && arg.len() > 2 {
                delim = arg[2..].chars().next().unwrap_or('\t');
                i += 1;
            } else if arg == "-f" && i + 1 < args.len() {
                mode = Some(StreamingCutMode::Fields(Self::parse_streaming_cut_ranges(
                    &args[i + 1],
                )));
                i += 2;
            } else if let Some(spec) = arg.strip_prefix("-f") {
                mode = Some(StreamingCutMode::Fields(Self::parse_streaming_cut_ranges(
                    spec,
                )));
                i += 1;
            } else if arg == "-c" && i + 1 < args.len() {
                mode = Some(StreamingCutMode::Chars(Self::parse_streaming_cut_ranges(
                    &args[i + 1],
                )));
                i += 2;
            } else if let Some(spec) = arg.strip_prefix("-c") {
                mode = Some(StreamingCutMode::Chars(Self::parse_streaming_cut_ranges(
                    spec,
                )));
                i += 1;
            } else if arg == "-b" && i + 1 < args.len() {
                mode = Some(StreamingCutMode::Bytes(Self::parse_streaming_cut_ranges(
                    &args[i + 1],
                )));
                i += 2;
            } else if let Some(spec) = arg.strip_prefix("-b") {
                mode = Some(StreamingCutMode::Bytes(Self::parse_streaming_cut_ranges(
                    spec,
                )));
                i += 1;
            } else if arg == "--complement" {
                complement = true;
                i += 1;
            } else if arg == "-s" {
                only_delimited = true;
                i += 1;
            } else if let Some(out) = arg.strip_prefix("--output-delimiter=") {
                output_delim = Some(out.to_string());
                i += 1;
            } else if arg == "-z" || arg == "--" {
                i += 1;
            } else {
                return None;
            }
        }

        Some(StreamingPipelineStage::Cut(StreamingCutStage {
            mode: mode?,
            delim,
            complement,
            only_delimited,
            output_delim: output_delim.unwrap_or_else(|| delim.to_string()),
        }))
    }

    fn parse_streaming_tr_stage(args: &[String]) -> Option<StreamingPipelineStage> {
        let mut delete = false;
        let mut squeeze = false;
        let mut complement = false;
        let mut set_args = Vec::new();

        for arg in args {
            if arg.starts_with('-') && arg.len() > 1 {
                for ch in arg[1..].chars() {
                    match ch {
                        'd' => delete = true,
                        's' => squeeze = true,
                        'c' | 'C' => complement = true,
                        't' => {}
                        _ => return None,
                    }
                }
            } else {
                set_args.push(arg.as_str());
            }
        }

        if set_args.is_empty() {
            return None;
        }
        let from_chars = streaming_tr_expand_set(set_args[0]);
        if delete {
            let to_chars = if squeeze && set_args.len() >= 2 {
                streaming_tr_expand_set(set_args[1])
            } else {
                Vec::new()
            };
            return Some(StreamingPipelineStage::Tr(StreamingTrStage {
                delete,
                squeeze,
                complement,
                from_chars,
                to_chars,
            }));
        }
        if squeeze && set_args.len() < 2 {
            return Some(StreamingPipelineStage::Tr(StreamingTrStage {
                delete,
                squeeze,
                complement,
                from_chars,
                to_chars: Vec::new(),
            }));
        }
        if set_args.len() < 2 {
            return None;
        }
        Some(StreamingPipelineStage::Tr(StreamingTrStage {
            delete,
            squeeze,
            complement,
            from_chars,
            to_chars: streaming_tr_expand_set(set_args[1]),
        }))
    }

    fn parse_streaming_wc_stage(args: &[String]) -> Option<StreamingPipelineStage> {
        let mut flags = StreamingWcFlags {
            lines: false,
            words: false,
            bytes: false,
            max_line_length: false,
        };
        let mut parsing_flags = true;

        for arg in args {
            if parsing_flags && arg == "--" {
                parsing_flags = false;
                continue;
            }
            if parsing_flags && arg.starts_with('-') && arg.len() > 1 {
                for ch in arg[1..].chars() {
                    match ch {
                        'l' => flags.lines = true,
                        'w' => flags.words = true,
                        'c' | 'm' => flags.bytes = true,
                        'L' => flags.max_line_length = true,
                        _ => return None,
                    }
                }
                continue;
            }
            return None;
        }

        if !flags.lines && !flags.words && !flags.bytes && !flags.max_line_length {
            flags.lines = true;
            flags.words = true;
            flags.bytes = true;
        }
        Some(StreamingPipelineStage::Wc(flags))
    }

    fn set_pipestatus(&mut self, statuses: &[i32]) {
        let status_key = smol_str::SmolStr::from("PIPESTATUS");
        self.vm.state.init_indexed_array(status_key.clone());
        for (i, s) in statuses.iter().enumerate() {
            self.vm.state.set_array_element(
                status_key.clone(),
                &i.to_string(),
                smol_str::SmolStr::from(s.to_string()),
            );
        }
    }

    fn open_streaming_file_reader(
        &mut self,
        path: &str,
        cmd_name: &str,
    ) -> Result<Box<dyn Read>, ()> {
        let resolved = self.resolve_cwd_path(path);
        match Self::open_streaming_file_reader_in_fs(&mut self.fs, &resolved) {
            Ok(reader) => Ok(reader),
            Err(err) => {
                let msg =
                    format!("wasmsh: {cmd_name}: failed to open stdin source {resolved}: {err}\n");
                self.write_stderr(msg.as_bytes());
                self.vm.state.last_status = 1;
                Err(())
            }
        }
    }

    fn open_streaming_file_reader_in_fs(
        fs: &mut BackendFs,
        resolved: &str,
    ) -> Result<Box<dyn Read>, String> {
        let handle = fs
            .open(resolved, OpenOptions::read())
            .map_err(|err| err.to_string())?;
        let reader_result = fs.stream_file(handle).map_err(|err| err.to_string());
        fs.close(handle);
        reader_result
    }

    fn execute_inner_capture_stdout(&mut self, input: &str) -> Vec<u8> {
        let events = self.execute_isolated_input_events(input, None);
        let mut stdout = Vec::new();
        for event in events {
            match event {
                WorkerEvent::Stdout(data) => stdout.extend_from_slice(&data),
                WorkerEvent::Stderr(data) => self.write_stderr(&data),
                WorkerEvent::Diagnostic(level, msg) => self.vm.emit_diagnostic(
                    convert_diag_level(level),
                    wasmsh_vm::DiagCategory::Runtime,
                    msg,
                ),
                _ => {}
            }
        }
        stdout
    }

    fn execute_isolated_input_events(
        &mut self,
        input: &str,
        pending_input: Option<InputTarget>,
    ) -> Vec<WorkerEvent> {
        let saved_state = self.vm.state.clone();
        let saved_functions = self.functions.clone();
        let saved_aliases = self.aliases.clone();
        let saved_exec = self.exec.clone();
        let saved_exec_io = self.current_exec_io.take();
        let saved_stdout = std::mem::take(&mut self.vm.stdout);
        let saved_stderr = std::mem::take(&mut self.vm.stderr);
        let saved_diagnostics = std::mem::take(&mut self.vm.diagnostics);
        let saved_output_bytes = self.vm.output_bytes;
        let saved_proc_subst_out_scopes = std::mem::take(&mut self.proc_subst_out_scopes);
        let saved_proc_subst_in_scopes = std::mem::take(&mut self.proc_subst_in_scopes);

        self.current_exec_io = pending_input.map(|target| {
            let mut exec_io = ExecIo::default();
            exec_io.fds_mut().set_input(target);
            exec_io
        });
        let (mut inner_events, captured) =
            self.with_output_capture(true, true, |runtime| runtime.execute_input_inner(input));
        let inner_resource_exhausted = self.exec.resource_exhausted;
        let inner_diagnostics = self
            .vm
            .diagnostics
            .drain(..)
            .map(|diag| {
                WorkerEvent::Diagnostic(Self::to_protocol_diag_level(diag.level), diag.message)
            })
            .collect::<Vec<_>>();
        self.clear_pending_input();
        for scope in self.proc_subst_out_scopes.drain(..) {
            for sink in scope {
                let _ = self.fs.remove_file(&sink.path);
            }
        }
        for scope in self.proc_subst_in_scopes.drain(..) {
            for sink in scope {
                let _ = self.fs.remove_file(&sink.path);
            }
        }

        self.vm.state = saved_state;
        self.functions = saved_functions;
        self.aliases = saved_aliases;
        self.exec = saved_exec;
        self.exec.resource_exhausted |= inner_resource_exhausted;
        self.current_exec_io = saved_exec_io;
        self.vm.stdout = saved_stdout;
        self.vm.stderr = saved_stderr;
        self.vm.diagnostics = saved_diagnostics;
        self.vm.output_bytes = saved_output_bytes;
        self.vm.budget.visible_output_bytes = saved_output_bytes;
        self.proc_subst_out_scopes = saved_proc_subst_out_scopes;
        self.proc_subst_in_scopes = saved_proc_subst_in_scopes;

        let mut events = Vec::new();
        if !captured.stdout.is_empty() {
            events.push(WorkerEvent::Stdout(captured.stdout));
        }
        if !captured.stderr.is_empty() {
            events.push(WorkerEvent::Stderr(captured.stderr));
        }
        for event in inner_events.drain(..) {
            match &event {
                WorkerEvent::Stdout(_)
                    if !events.iter().any(|e| matches!(e, WorkerEvent::Stdout(_))) =>
                {
                    events.push(event);
                }
                WorkerEvent::Stderr(_)
                    if !events.iter().any(|e| matches!(e, WorkerEvent::Stderr(_))) =>
                {
                    events.push(event);
                }
                WorkerEvent::Stdout(_) | WorkerEvent::Stderr(_) => {}
                _ => events.push(event),
            }
        }
        events.extend(inner_diagnostics);
        events
    }

    fn execute_isolated_scheduled_pipeline_events_from_reader(
        &mut self,
        pipeline: &HirPipeline,
        reader: Box<dyn Read>,
    ) -> Vec<WorkerEvent> {
        let saved_state = self.vm.state.clone();
        let saved_functions = self.functions.clone();
        let saved_aliases = self.aliases.clone();
        let saved_exec = self.exec.clone();
        let saved_exec_io = self.current_exec_io.take();
        let saved_stdout = std::mem::take(&mut self.vm.stdout);
        let saved_stderr = std::mem::take(&mut self.vm.stderr);
        let saved_diagnostics = std::mem::take(&mut self.vm.diagnostics);
        let saved_output_bytes = self.vm.output_bytes;
        let saved_proc_subst_out_scopes = std::mem::take(&mut self.proc_subst_out_scopes);
        let saved_proc_subst_in_scopes = std::mem::take(&mut self.proc_subst_in_scopes);

        self.current_exec_io = None;
        self.proc_subst_out_scopes.clear();
        self.proc_subst_in_scopes.clear();
        self.exec.recursion_depth += 1;
        if let Err(reason) = self
            .vm
            .budget
            .enter_recursion(self.vm.limits.recursion_limit)
        {
            self.exec.recursion_depth -= 1;
            self.vm.state = saved_state;
            self.functions = saved_functions;
            self.aliases = saved_aliases;
            self.exec = saved_exec;
            self.current_exec_io = saved_exec_io;
            self.vm.stdout = saved_stdout;
            self.vm.stderr = saved_stderr;
            self.vm.diagnostics = saved_diagnostics;
            self.vm.output_bytes = saved_output_bytes;
            self.vm.budget.visible_output_bytes = saved_output_bytes;
            self.proc_subst_out_scopes = saved_proc_subst_out_scopes;
            self.proc_subst_in_scopes = saved_proc_subst_in_scopes;
            self.mark_budget_exhaustion(reason);
            return vec![WorkerEvent::Stderr(
                b"wasmsh: maximum recursion depth exceeded\n".to_vec(),
            )];
        }

        let ((), captured) = self.with_output_capture(true, true, |runtime| {
            runtime.execute_scheduled_pipeline_with_source_reader(
                &pipeline.commands,
                pipeline,
                Some(reader),
            );
        });
        self.exec.recursion_depth -= 1;
        self.vm.budget.exit_recursion();
        let inner_resource_exhausted = self.exec.resource_exhausted;
        let inner_diagnostics = self
            .vm
            .diagnostics
            .drain(..)
            .map(|diag| {
                WorkerEvent::Diagnostic(Self::to_protocol_diag_level(diag.level), diag.message)
            })
            .collect::<Vec<_>>();
        self.clear_pending_input();
        let pending_scopes: Vec<Vec<PendingProcessSubstOut>> =
            self.proc_subst_out_scopes.drain(..).collect();
        for scope in pending_scopes {
            for sink in scope {
                self.flush_process_subst_out(sink);
            }
        }
        let pending_in_scopes: Vec<Vec<PendingProcessSubstIn>> =
            self.proc_subst_in_scopes.drain(..).collect();
        for scope in pending_in_scopes {
            self.flush_process_subst_in_scope(scope);
        }

        self.vm.state = saved_state;
        self.functions = saved_functions;
        self.aliases = saved_aliases;
        self.exec = saved_exec;
        self.exec.resource_exhausted |= inner_resource_exhausted;
        self.current_exec_io = saved_exec_io;
        self.vm.stdout = saved_stdout;
        self.vm.stderr = saved_stderr;
        self.vm.diagnostics = saved_diagnostics;
        self.vm.output_bytes = saved_output_bytes;
        self.vm.budget.visible_output_bytes = saved_output_bytes;
        self.proc_subst_out_scopes = saved_proc_subst_out_scopes;
        self.proc_subst_in_scopes = saved_proc_subst_in_scopes;

        let mut events = Vec::new();
        if !captured.stdout.is_empty() {
            events.push(WorkerEvent::Stdout(captured.stdout));
        }
        if !captured.stderr.is_empty() {
            events.push(WorkerEvent::Stderr(captured.stderr));
        }
        events.extend(inner_diagnostics);
        events
    }

    /// Execute a command substitution and return the trimmed output.
    fn execute_subst(&mut self, inner: &str) -> smol_str::SmolStr {
        let stdout = self.execute_inner_capture_stdout(inner);
        let result = String::from_utf8_lossy(&stdout).to_string();
        smol_str::SmolStr::from(result.trim_end_matches('\n'))
    }

    fn word_parts_require_runtime_expansion(parts: &[WordPart]) -> bool {
        parts.iter().any(|part| match part {
            WordPart::Literal(_) | WordPart::SingleQuoted(_) => false,
            WordPart::DoubleQuoted(inner) => Self::word_parts_require_runtime_expansion(inner),
            WordPart::Parameter(_)
            | WordPart::Arithmetic(_)
            | WordPart::CommandSubstitution(_)
            | WordPart::ProcessSubstIn(_)
            | WordPart::ProcessSubstOut(_)
            | _ => true,
        })
    }

    fn command_requires_runtime_expansion(cmd: &HirCommand) -> bool {
        let HirCommand::Exec(exec) = cmd else {
            return false;
        };
        exec.argv
            .iter()
            .any(|word| Self::word_parts_require_runtime_expansion(&word.parts))
    }

    fn command_needs_full_single_stage_execution(&self, cmd: &HirCommand) -> bool {
        if self.vm.state.get_var("SHOPT_x").as_deref() == Some("1") {
            return true;
        }
        let HirCommand::Exec(exec) = cmd else {
            return false;
        };
        exec.argv.iter().any(Self::word_has_brace_or_glob_literal)
    }

    fn word_has_brace_or_glob_literal(word: &Word) -> bool {
        word.parts
            .iter()
            .any(Self::word_part_has_brace_or_glob_literal)
    }

    fn word_part_has_brace_or_glob_literal(part: &WordPart) -> bool {
        match part {
            WordPart::Literal(text) | WordPart::SingleQuoted(text) | WordPart::Parameter(text) => {
                Self::text_has_brace_or_glob_literal(text)
            }
            WordPart::DoubleQuoted(parts) => {
                parts.iter().any(Self::word_part_has_brace_or_glob_literal)
            }
            WordPart::Arithmetic(_) => false,
            WordPart::CommandSubstitution(_)
            | WordPart::ProcessSubstIn(_)
            | WordPart::ProcessSubstOut(_)
            | _ => true,
        }
    }

    fn text_has_brace_or_glob_literal(text: &str) -> bool {
        text.contains('{')
            || text.contains('}')
            || text.contains('*')
            || text.contains('?')
            || text.contains('[')
    }

    fn parse_single_pipeline_input(input: &str) -> Option<HirPipeline> {
        let ast = wasmsh_parse::parse(input).ok()?;
        let hir = wasmsh_hir::lower(&ast);
        let cc = hir.items.first()?;
        if hir.items.len() != 1 || cc.list.len() != 1 {
            return None;
        }
        let and_or = cc.list.first()?;
        if !and_or.rest.is_empty() {
            return None;
        }
        Some(and_or.first.clone())
    }

    /// Counter for generating unique temp file paths for process substitution.
    fn next_proc_subst_id() -> u64 {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    fn next_pending_input_id() -> u64 {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    fn set_pending_input_bytes(&mut self, data: Vec<u8>) {
        self.current_exec_io
            .get_or_insert_with(ExecIo::default)
            .fds_mut()
            .set_input(InputTarget::Bytes(data));
    }

    fn set_pending_input_file(&mut self, path: String, remove_after_read: bool) {
        self.current_exec_io
            .get_or_insert_with(ExecIo::default)
            .fds_mut()
            .set_input(InputTarget::File {
                path,
                remove_after_read,
            });
    }

    fn clear_pending_input(&mut self) {
        let Some(exec_io) = self.current_exec_io.as_mut() else {
            return;
        };
        if let InputTarget::File {
            path,
            remove_after_read: true,
        } = exec_io.take_stdin()
        {
            let _ = self.fs.remove_file(&path);
        }
    }

    fn take_pending_input_reader(&mut self, cmd_name: &str) -> Result<Option<Box<dyn Read>>, ()> {
        let Some(exec_io) = self.current_exec_io.as_mut() else {
            return Ok(None);
        };
        match exec_io.take_stdin() {
            InputTarget::Inherit | InputTarget::Closed => Ok(None),
            InputTarget::Bytes(data) => Ok(Some(Box::new(Cursor::new(data)))),
            InputTarget::File {
                path,
                remove_after_read,
            } => {
                let reader_result = self.open_streaming_file_reader(&path, cmd_name);
                if remove_after_read {
                    let _ = self.fs.remove_file(&path);
                }
                reader_result.map(Some)
            }
            InputTarget::Pipe(pipe) => Ok(Some(Box::new(PipeReader::new(pipe)))),
        }
    }

    fn take_builtin_stdin(
        &mut self,
        cmd_name: &str,
    ) -> Result<Option<wasmsh_builtins::BuiltinStdin<'static>>, ()> {
        let reader = self.take_pending_input_reader(cmd_name)?;
        Ok(reader.map(wasmsh_builtins::BuiltinStdin::from_reader))
    }

    fn take_util_stdin(
        &mut self,
        cmd_name: &str,
    ) -> Result<Option<wasmsh_utils::UtilStdin<'static>>, ()> {
        let reader = self.take_pending_input_reader(cmd_name)?;
        Ok(reader.map(wasmsh_utils::UtilStdin::from_reader))
    }

    fn take_external_stdin(
        &mut self,
        cmd_name: &str,
    ) -> Result<Option<ExternalCommandStdin<'static>>, ()> {
        let reader = self.take_pending_input_reader(cmd_name)?;
        Ok(reader.map(ExternalCommandStdin::from_reader))
    }

    fn can_use_isolated_process_subst_runtime(&self) -> bool {
        self.external_handler.is_none() && self.network.is_none()
    }

    fn clone_for_isolated_process_subst(&self) -> Option<Self> {
        if !self.can_use_isolated_process_subst_runtime() {
            return None;
        }
        let mut exec = ExecState::new();
        exec.recursion_depth = self.exec.recursion_depth;
        Some(Self {
            config: self.config.clone(),
            vm: Vm::with_limits(self.vm.state.clone(), self.vm.limits.clone()),
            fs: self.fs.clone(),
            utils: UtilRegistry::new(),
            builtins: wasmsh_builtins::BuiltinRegistry::new(),
            initialized: self.initialized,
            current_exec_io: None,
            proc_subst_out_scopes: Vec::new(),
            proc_subst_in_scopes: Vec::new(),
            functions: self.functions.clone(),
            exec,
            aliases: self.aliases.clone(),
            external_handler: None,
            network: None,
            active_run: None,
        })
    }

    fn build_live_process_subst_pipeline(
        &mut self,
        pipeline: &HirPipeline,
        source_pipe: Option<Rc<RefCell<PipeBuffer>>>,
    ) -> Option<(
        Vec<StreamingPipeProcess<'static>>,
        Vec<Rc<RefCell<Vec<u8>>>>,
        Vec<bool>,
        Rc<RefCell<PipeBuffer>>,
        Vec<Rc<RefCell<i32>>>,
    )> {
        let stages: Vec<StreamingPipelineStage> = pipeline
            .commands
            .iter()
            .enumerate()
            .map(|(idx, cmd)| self.compile_pipeline_stage(cmd, idx == 0 && source_pipe.is_none()))
            .collect();
        let stage_statuses: Vec<Rc<RefCell<i32>>> = stages
            .iter()
            .map(|stage| {
                Rc::new(RefCell::new(i32::from(matches!(
                    stage,
                    StreamingPipelineStage::Grep(_)
                ))))
            })
            .collect();
        let stage_stderr: Vec<Rc<RefCell<Vec<u8>>>> = stages
            .iter()
            .map(|_| Rc::new(RefCell::new(Vec::new())))
            .collect();
        let stage_pipe_stderr = vec![false; stages.len()];
        let output_pipes: Vec<Rc<RefCell<PipeBuffer>>> = (0..stages.len())
            .map(|_| Rc::new(RefCell::new(PipeBuffer::new(PIPEBUFFER_STREAMING_CAPACITY))))
            .collect();
        let mut processes = Vec::new();

        if let Some(source_pipe) = source_pipe {
            match &stages[0] {
                StreamingPipelineStage::Tee(stage) => {
                    let reader = Box::new(PipeReader::new(source_pipe)) as Box<dyn Read>;
                    processes.push(StreamingPipeProcess::Tee(TeePipeProcess::new(
                        reader,
                        output_pipes[0].clone(),
                        &mut self.fs,
                        self.vm.state.cwd.as_str(),
                        stage,
                        stage_stderr[0].clone(),
                        stage_statuses[0].clone(),
                        false,
                    )));
                }
                StreamingPipelineStage::BufferedCommand(argv) => {
                    processes.push(StreamingPipeProcess::Buffered(BufferedPipeProcess::new(
                        Some(source_pipe),
                        output_pipes[0].clone(),
                        argv.clone(),
                        false,
                        stage_stderr[0].clone(),
                        stage_statuses[0].clone(),
                    )));
                }
                _ => {
                    let reader = Box::new(PipeReader::new(source_pipe)) as Box<dyn Read>;
                    let stage_reader =
                        Self::wrap_non_tee_streaming_stage(reader, &stages[0], 0, &stage_statuses)?;
                    processes.push(StreamingPipeProcess::Read(PipeReadProcess::new(
                        stage_reader,
                        output_pipes[0].clone(),
                        stage_stderr[0].clone(),
                        stage_statuses[0].clone(),
                        "process-subst",
                        false,
                    )));
                }
            }
        } else {
            match &stages[0] {
                StreamingPipelineStage::Literal(data) => {
                    let reader: Box<dyn Read> = Box::new(Cursor::new(data.clone()));
                    processes.push(StreamingPipeProcess::Read(PipeReadProcess::new(
                        reader,
                        output_pipes[0].clone(),
                        stage_stderr[0].clone(),
                        stage_statuses[0].clone(),
                        "process-subst",
                        false,
                    )));
                }
                StreamingPipelineStage::File(path) => {
                    let resolved = self.resolve_cwd_path(path);
                    let reader = self.open_streaming_file_reader(&resolved, "cat").ok()?;
                    processes.push(StreamingPipeProcess::Read(PipeReadProcess::new(
                        reader,
                        output_pipes[0].clone(),
                        stage_stderr[0].clone(),
                        stage_statuses[0].clone(),
                        "process-subst",
                        false,
                    )));
                }
                StreamingPipelineStage::Yes { line } => {
                    let reader: Box<dyn Read> =
                        Box::new(YesStreamReader::new(line.clone(), STREAMING_YES_MAX_LINES));
                    processes.push(StreamingPipeProcess::Read(PipeReadProcess::new(
                        reader,
                        output_pipes[0].clone(),
                        stage_stderr[0].clone(),
                        stage_statuses[0].clone(),
                        "process-subst",
                        false,
                    )));
                }
                StreamingPipelineStage::BufferedCommand(argv) => {
                    processes.push(StreamingPipeProcess::Buffered(BufferedPipeProcess::new(
                        None,
                        output_pipes[0].clone(),
                        argv.clone(),
                        false,
                        stage_stderr[0].clone(),
                        stage_statuses[0].clone(),
                    )));
                }
                _ => return None,
            }
        }

        for idx in 1..stages.len() {
            match &stages[idx] {
                StreamingPipelineStage::Head(mode) => {
                    processes.push(StreamingPipeProcess::Head(HeadPipeProcess::new(
                        output_pipes[idx - 1].clone(),
                        output_pipes[idx].clone(),
                        *mode,
                    )));
                }
                StreamingPipelineStage::Tee(stage) => {
                    let reader =
                        Box::new(PipeReader::new(output_pipes[idx - 1].clone())) as Box<dyn Read>;
                    processes.push(StreamingPipeProcess::Tee(TeePipeProcess::new(
                        reader,
                        output_pipes[idx].clone(),
                        &mut self.fs,
                        self.vm.state.cwd.as_str(),
                        stage,
                        stage_stderr[idx].clone(),
                        stage_statuses[idx].clone(),
                        false,
                    )));
                }
                StreamingPipelineStage::BufferedCommand(argv) => {
                    processes.push(StreamingPipeProcess::Buffered(BufferedPipeProcess::new(
                        Some(output_pipes[idx - 1].clone()),
                        output_pipes[idx].clone(),
                        argv.clone(),
                        false,
                        stage_stderr[idx].clone(),
                        stage_statuses[idx].clone(),
                    )));
                }
                _ => {
                    let reader =
                        Box::new(PipeReader::new(output_pipes[idx - 1].clone())) as Box<dyn Read>;
                    let stage_reader = Self::wrap_non_tee_streaming_stage(
                        reader,
                        &stages[idx],
                        idx,
                        &stage_statuses,
                    )?;
                    processes.push(StreamingPipeProcess::Read(PipeReadProcess::new(
                        stage_reader,
                        output_pipes[idx].clone(),
                        stage_stderr[idx].clone(),
                        stage_statuses[idx].clone(),
                        "process-subst",
                        false,
                    )));
                }
            }
        }

        Some((
            processes,
            stage_stderr,
            stage_pipe_stderr,
            output_pipes.last().cloned()?,
            stage_statuses,
        ))
    }

    fn try_build_live_process_subst_in_reader(
        &mut self,
        inner: &str,
    ) -> Option<(
        Box<dyn Read>,
        Rc<RefCell<Vec<u8>>>,
        Rc<RefCell<Vec<wasmsh_vm::DiagnosticEvent>>>,
    )> {
        let pipeline = Self::parse_single_pipeline_input(inner)?;
        let requires_runtime = pipeline.commands.iter().enumerate().any(|(idx, cmd)| {
            matches!(
                self.compile_pipeline_stage(cmd, idx == 0),
                StreamingPipelineStage::BufferedCommand(_)
            )
        });
        let mut isolated_runtime = if requires_runtime {
            self.clone_for_isolated_process_subst().map(Box::new)
        } else {
            None
        };
        let (processes, stage_stderr, stage_pipe_stderr, final_pipe, _) =
            if let Some(runtime) = isolated_runtime.as_mut() {
                runtime.build_live_process_subst_pipeline(&pipeline, None)?
            } else {
                if requires_runtime {
                    return None;
                }
                self.build_live_process_subst_pipeline(&pipeline, None)?
            };

        let flushed_stderr = Rc::new(RefCell::new(Vec::new()));
        let flushed_diagnostics = Rc::new(RefCell::new(Vec::new()));
        let reader = LiveProcessSubstInReader {
            isolated_runtime,
            processes,
            finished: vec![false; stage_stderr.len()],
            final_pipe,
            stage_stderr,
            stage_pipe_stderr,
            flushed_stderr: flushed_stderr.clone(),
            flushed_diagnostics: flushed_diagnostics.clone(),
            done: false,
        };
        Some((Box::new(reader), flushed_stderr, flushed_diagnostics))
    }

    /// Execute `<(cmd)` by registering a command-scoped readable path.
    fn execute_process_subst_in(&mut self, inner: &str) -> smol_str::SmolStr {
        let path = format!("/tmp/_proc_subst_{}", Self::next_proc_subst_id());
        if self.proc_subst_in_scopes.is_empty() {
            self.proc_subst_in_scopes.push(Vec::new());
        }

        if let Some((reader, stderr, diagnostics)) =
            self.try_build_live_process_subst_in_reader(inner)
        {
            if self.fs.install_stream_reader(&path, reader).is_ok() {
                self.proc_subst_in_scopes
                    .last_mut()
                    .expect("process substitution input scope stack is empty")
                    .push(PendingProcessSubstIn {
                        path: path.clone(),
                        stderr: Some(stderr),
                        diagnostics: Some(diagnostics),
                    });
                return smol_str::SmolStr::from(path);
            }
        }

        let output = self.execute_inner_capture_stdout(inner);
        if let Ok(h) = self.fs.open(&path, OpenOptions::write()) {
            let _ = self.fs.write_file(h, &output);
            self.fs.close(h);
        }
        self.proc_subst_in_scopes
            .last_mut()
            .expect("process substitution input scope stack is empty")
            .push(PendingProcessSubstIn {
                path: path.clone(),
                stderr: None,
                diagnostics: None,
            });
        smol_str::SmolStr::from(path)
    }

    fn try_build_live_process_subst_runner(
        &mut self,
        inner: &str,
    ) -> Option<LiveProcessSubstRunner> {
        let pipeline = Self::parse_single_pipeline_input(inner)?;
        let source_pipe = Rc::new(RefCell::new(PipeBuffer::new(PIPEBUFFER_STREAMING_CAPACITY)));
        let mut isolated_runtime = self.clone_for_isolated_process_subst();
        let (processes, stage_stderr, stage_pipe_stderr, final_pipe, _) =
            if let Some(runtime) = isolated_runtime.as_mut() {
                runtime.build_live_process_subst_pipeline(&pipeline, Some(source_pipe.clone()))?
            } else {
                self.build_live_process_subst_pipeline(&pipeline, Some(source_pipe.clone()))?
            };

        Some(LiveProcessSubstRunner {
            isolated_runtime: isolated_runtime.map(Box::new),
            source_pipe,
            processes,
            finished: vec![false; stage_stderr.len()],
            final_pipe,
            stage_stderr,
            stage_pipe_stderr,
            captured_stdout: Vec::new(),
            captured_stderr: Vec::new(),
            captured_diagnostics: Vec::new(),
            done: false,
            synced_steps: self.vm.steps,
        })
    }

    fn register_process_subst_out(&mut self, inner: &str) -> String {
        if self.proc_subst_out_scopes.is_empty() {
            self.proc_subst_out_scopes.push(Vec::new());
        }
        let path = format!("/tmp/_proc_subst_{}", Self::next_proc_subst_id());
        let mode = if let Some(runner) = self.try_build_live_process_subst_runner(inner) {
            PendingProcessSubstOutMode::Live { runner }
        } else {
            PendingProcessSubstOutMode::Buffered { data: Vec::new() }
        };
        self.proc_subst_out_scopes
            .last_mut()
            .expect("process substitution scope stack is empty")
            .push(PendingProcessSubstOut {
                path: path.clone(),
                inner: inner.to_string(),
                mode,
            });
        path
    }

    fn flush_process_subst_out_scope(&mut self, scope: Vec<PendingProcessSubstOut>) {
        for sink in scope {
            self.flush_process_subst_out(sink);
        }
    }

    fn flush_process_subst_in_scope(&mut self, scope: Vec<PendingProcessSubstIn>) {
        for sink in scope {
            if let Some(stderr) = sink.stderr {
                let data = stderr.borrow();
                if !data.is_empty() {
                    self.write_stderr(&data);
                }
            }
            if let Some(diagnostics) = sink.diagnostics {
                let mut diagnostics = diagnostics.borrow_mut();
                for event in diagnostics.drain(..) {
                    self.vm
                        .emit_diagnostic(event.level, event.category, event.message);
                }
            }
            let _ = self.fs.remove_file(&sink.path);
        }
    }

    fn flush_process_subst_out(&mut self, sink: PendingProcessSubstOut) {
        let saved_status = self.vm.state.last_status;
        match sink.mode {
            PendingProcessSubstOutMode::Buffered { data } => {
                let events = if let Some(pipeline) = Self::parse_single_pipeline_input(&sink.inner)
                {
                    self.execute_isolated_scheduled_pipeline_events_from_reader(
                        &pipeline,
                        Box::new(Cursor::new(data.clone())),
                    )
                } else {
                    self.execute_isolated_input_events(&sink.inner, Some(InputTarget::Bytes(data)))
                };
                for event in events {
                    match event {
                        WorkerEvent::Stdout(data) => self.write_stdout(&data),
                        WorkerEvent::Stderr(data) => self.write_stderr(&data),
                        WorkerEvent::Diagnostic(level, msg) => self.vm.emit_diagnostic(
                            convert_diag_level(level),
                            wasmsh_vm::DiagCategory::Runtime,
                            msg,
                        ),
                        _ => {}
                    }
                }
            }
            PendingProcessSubstOutMode::Live { mut runner } => {
                if runner.isolated_runtime.is_some() {
                    runner.finish_with_parent(self);
                } else {
                    runner.finish();
                }
                if !runner.captured_stdout.is_empty() {
                    self.write_stdout(&runner.captured_stdout);
                }
                if !runner.captured_stderr.is_empty() {
                    self.write_stderr(&runner.captured_stderr);
                }
                for diag in runner.captured_diagnostics {
                    self.vm
                        .emit_diagnostic(diag.level, diag.category, diag.message);
                }
            }
        }
        self.vm.state.last_status = saved_status;
    }

    /// Execute `>(cmd)` by creating a writable temp path and scheduling the
    /// consumer command to run once the enclosing command finishes writing to it.
    fn execute_process_subst_out(&mut self, inner: &str) -> smol_str::SmolStr {
        smol_str::SmolStr::from(self.register_process_subst_out(inner))
    }

    /// Resolve command substitutions in a list of words by executing them.
    fn resolve_command_subst(&mut self, words: &[Word]) -> Vec<Word> {
        words
            .iter()
            .map(|w| {
                let parts: Vec<WordPart> = w
                    .parts
                    .iter()
                    .map(|p| match p {
                        WordPart::CommandSubstitution(inner) => {
                            WordPart::Literal(self.execute_subst(inner))
                        }
                        WordPart::ProcessSubstIn(inner) => {
                            WordPart::Literal(self.execute_process_subst_in(inner))
                        }
                        WordPart::ProcessSubstOut(inner) => {
                            WordPart::Literal(self.execute_process_subst_out(inner))
                        }
                        WordPart::DoubleQuoted(inner_parts) => {
                            let resolved: Vec<WordPart> = inner_parts
                                .iter()
                                .map(|ip| match ip {
                                    WordPart::CommandSubstitution(inner) => {
                                        WordPart::Literal(self.execute_subst(inner))
                                    }
                                    WordPart::ProcessSubstIn(inner) => {
                                        WordPart::Literal(self.execute_process_subst_in(inner))
                                    }
                                    WordPart::ProcessSubstOut(inner) => {
                                        WordPart::Literal(self.execute_process_subst_out(inner))
                                    }
                                    other => other.clone(),
                                })
                                .collect();
                            WordPart::DoubleQuoted(resolved)
                        }
                        other => other.clone(),
                    })
                    .collect();
                Word {
                    parts,
                    span: w.span,
                }
            })
            .collect()
    }

    fn execute_command(&mut self, cmd: &HirCommand) {
        self.proc_subst_out_scopes.push(Vec::new());
        self.proc_subst_in_scopes.push(Vec::new());
        self.execute_command_body(cmd);
        let in_scope = self
            .proc_subst_in_scopes
            .pop()
            .expect("process substitution input scope stack underflow");
        let scope = self
            .proc_subst_out_scopes
            .pop()
            .expect("process substitution scope stack underflow");
        self.flush_process_subst_out_scope(scope);
        self.flush_process_subst_in_scope(in_scope);
    }

    fn execute_command_body(&mut self, cmd: &HirCommand) {
        match cmd {
            HirCommand::Exec(exec) => self.execute_exec(exec),
            HirCommand::Assign(assign) => {
                for a in &assign.assignments {
                    self.execute_assignment(&a.name, a.value.as_ref());
                }
                let stdout_before = self.current_stdout_len();
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
                let stdout_before = self.current_stdout_len();
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
        if self.exec.expansion_failed {
            return;
        }
        let expanded = expand_words_argv(&resolved, &mut self.vm.state);

        if self.check_nounset_error() {
            return;
        }
        if expanded.is_empty() {
            return;
        }

        // Brace and glob expansion must be suppressed for quoted words (POSIX + bash).
        let tagged: Vec<(String, bool)> = expanded
            .into_iter()
            .flat_map(|ew| {
                if ew.was_quoted {
                    vec![(ew.text, true)]
                } else {
                    wasmsh_expand::expand_braces(&ew.text)
                        .into_iter()
                        .map(|s| (s, false))
                        .collect()
                }
            })
            .collect();
        let argv = self.expand_globs_tagged(tagged);

        for assignment in &exec.env {
            self.execute_assignment(&assignment.name, assignment.value.as_ref());
        }

        if self.try_alias_expansion(&argv) {
            return;
        }

        let Ok(exec_io) = self.prepare_exec_io(&exec.redirections) else {
            return;
        };
        self.with_exec_io_scope(exec_io, |runtime| {
            runtime.trace_command(&argv);
            runtime.execute_argv_command(&argv);
        });
    }

    /// Drain a pending nounset error from parameter expansion and report it
    /// through the fallback interpreter's stderr sink.
    fn check_nounset_error(&mut self) -> bool {
        let Some(var_name) = self.vm.state.take_nounset_error() else {
            return false;
        };
        let msg = format!("wasmsh: {var_name}: unbound variable\n");
        self.write_stderr(msg.as_bytes());
        self.vm.state.last_status = 1;
        true
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
                        self.set_pending_input_bytes(expanded.into_bytes());
                    }
                }
                RedirectionOp::HereString => {
                    let resolved = self.resolve_command_subst(std::slice::from_ref(&redir.target));
                    let resolved_target = resolved.first().unwrap_or(&redir.target);
                    let content = wasmsh_expand::expand_word(resolved_target, &mut self.vm.state);
                    let mut data = content.into_bytes();
                    data.push(b'\n');
                    self.set_pending_input_bytes(data);
                }
                RedirectionOp::Input => {
                    let resolved = self.resolve_command_subst(std::slice::from_ref(&redir.target));
                    let resolved_target = resolved.first().unwrap_or(&redir.target);
                    let target = wasmsh_expand::expand_word(resolved_target, &mut self.vm.state);
                    let path = self.resolve_cwd_path(&target);
                    match self.fs.stat(&path) {
                        Ok(metadata) if !metadata.is_dir => {
                            self.set_pending_input_file(path, false);
                        }
                        Ok(_) => {
                            let msg = format!("wasmsh: {target}: Is a directory\n");
                            self.write_stderr(msg.as_bytes());
                            self.vm.state.last_status = 1;
                            return true;
                        }
                        Err(_) => {
                            let msg = format!("wasmsh: {target}: No such file or directory\n");
                            self.write_stderr(msg.as_bytes());
                            self.vm.state.last_status = 1;
                            return true;
                        }
                    }
                }
                _ => {}
            }
        }
        false
    }

    fn pending_input_first_line(&mut self, cmd_name: &str) -> Result<String, ()> {
        let data = self.read_pending_input_bytes(cmd_name)?.unwrap_or_default();
        let input = String::from_utf8_lossy(&data);
        Ok(input.lines().next().unwrap_or("").to_string())
    }

    fn read_pending_input_bytes(&mut self, cmd_name: &str) -> Result<Option<Vec<u8>>, ()> {
        let Some(mut reader) = self.take_pending_input_reader(cmd_name)? else {
            return Ok(None);
        };
        let mut data = Vec::new();
        match reader.read_to_end(&mut data) {
            Ok(_) => Ok(Some(data)),
            Err(err) => {
                let msg = format!("wasmsh: {cmd_name}: stdin read error: {err}\n");
                self.write_stderr(msg.as_bytes());
                self.vm.state.last_status = 1;
                Err(())
            }
        }
    }

    /// Try alias expansion for the command. Returns true if an alias was expanded.
    fn try_alias_expansion(&mut self, argv: &[String]) -> bool {
        if !self.get_shopt_value("expand_aliases") {
            return false;
        }
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
            self.write_stderr(trace_line.as_bytes());
        }
    }

    fn resolve_runtime_command(cmd_name: &str) -> Option<RuntimeCommandKind> {
        match cmd_name {
            CMD_LOCAL => Some(RuntimeCommandKind::Local),
            CMD_BREAK => Some(RuntimeCommandKind::Break),
            CMD_CONTINUE => Some(RuntimeCommandKind::Continue),
            CMD_EXIT => Some(RuntimeCommandKind::Exit),
            CMD_EVAL => Some(RuntimeCommandKind::Eval),
            CMD_SOURCE | CMD_DOT => Some(RuntimeCommandKind::Source),
            CMD_DECLARE | CMD_TYPESET => Some(RuntimeCommandKind::Declare),
            CMD_LET => Some(RuntimeCommandKind::Let),
            CMD_SHOPT => Some(RuntimeCommandKind::Shopt),
            CMD_ALIAS => Some(RuntimeCommandKind::Alias),
            CMD_UNALIAS => Some(RuntimeCommandKind::Unalias),
            CMD_BUILTIN => Some(RuntimeCommandKind::BuiltinKeyword),
            CMD_MAPFILE | CMD_READARRAY => Some(RuntimeCommandKind::Mapfile),
            CMD_TYPE => Some(RuntimeCommandKind::Type),
            _ => None,
        }
    }

    fn resolve_command(&self, cmd_name: &str, argv: &[String]) -> ResolvedCommand {
        if let Some(kind) = Self::resolve_runtime_command(cmd_name) {
            return ResolvedCommand::Runtime(kind);
        }
        if cmd_name == "bash" || cmd_name == "sh" {
            return ResolvedCommand::ShellScript;
        }
        if let Some(body) = self.functions.get(cmd_name).cloned() {
            return ResolvedCommand::Function(body);
        }
        if self.builtins.is_builtin(cmd_name) {
            return ResolvedCommand::Builtin;
        }
        if self.utils.is_utility(cmd_name) {
            let kind = if cmd_name == "find" && argv.iter().any(|arg| arg == "-exec") {
                UtilityCommandKind::FindWithExec
            } else if cmd_name == "xargs" {
                UtilityCommandKind::Xargs
            } else {
                UtilityCommandKind::Plain
            };
            return ResolvedCommand::Utility(kind);
        }
        ResolvedCommand::External
    }

    fn execute_argv_command(&mut self, argv: &[String]) {
        if self.check_resource_limits() || argv.is_empty() {
            return;
        }
        let resolved = self.resolve_command(&argv[0], argv);
        self.execute_resolved_command(resolved, argv);
    }

    fn execute_resolved_command(&mut self, resolved: ResolvedCommand, argv: &[String]) {
        match resolved {
            ResolvedCommand::Runtime(kind) => self.execute_runtime_command(kind, argv),
            ResolvedCommand::ShellScript => self.call_shell_script(argv),
            ResolvedCommand::Function(body) => self.call_shell_function(&argv[0], argv, &body),
            ResolvedCommand::Builtin => self.call_builtin(&argv[0], argv),
            ResolvedCommand::Utility(kind) => match kind {
                UtilityCommandKind::Plain => self.call_utility(&argv[0], argv),
                UtilityCommandKind::FindWithExec => self.call_find_with_exec(argv),
                UtilityCommandKind::Xargs => self.call_xargs_with_exec(argv),
            },
            ResolvedCommand::External => self.call_external(argv),
        }
    }

    fn execute_runtime_command(&mut self, kind: RuntimeCommandKind, argv: &[String]) {
        match kind {
            RuntimeCommandKind::Local => self.execute_local(argv),
            RuntimeCommandKind::Break => {
                self.exec.break_depth = argv.get(1).and_then(|s| s.parse().ok()).unwrap_or(1);
                self.vm.state.last_status = 0;
            }
            RuntimeCommandKind::Continue => {
                self.exec.loop_continue = true;
                self.vm.state.last_status = 0;
            }
            RuntimeCommandKind::Exit => {
                let code = argv
                    .get(1)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(self.vm.state.last_status);
                self.exec.exit_requested = Some(code);
                self.vm.state.last_status = code;
            }
            RuntimeCommandKind::Eval => {
                let code = argv[1..].join(" ");
                let sub_events = self.execute_input_inner(&code);
                self.merge_sub_events_with_diagnostics(sub_events);
            }
            RuntimeCommandKind::Source => self.execute_source(argv),
            RuntimeCommandKind::Declare => self.execute_declare(argv),
            RuntimeCommandKind::Let => self.execute_let(argv),
            RuntimeCommandKind::Shopt => self.execute_shopt(argv),
            RuntimeCommandKind::Alias => self.execute_alias(argv),
            RuntimeCommandKind::Unalias => self.execute_unalias(argv),
            RuntimeCommandKind::BuiltinKeyword => self.execute_builtin_keyword(argv),
            RuntimeCommandKind::Mapfile => self.execute_mapfile(argv),
            RuntimeCommandKind::Type => self.execute_type(argv),
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
            self.write_stderr(msg.as_bytes());
            self.vm.state.last_status = 1;
            return;
        };
        let Ok(h) = self.fs.open(&full, OpenOptions::read()) else {
            let msg = format!("source: {path}: not found\n");
            self.write_stderr(msg.as_bytes());
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
                self.write_stderr(msg.as_bytes());
                self.vm.state.last_status = 1;
            }
        }
    }

    /// Merge sub-events (stdout/stderr only) into the current VM buffers.
    fn merge_sub_events(&mut self, events: Vec<WorkerEvent>) {
        for e in events {
            match e {
                WorkerEvent::Stdout(d) => self.write_stdout(&d),
                WorkerEvent::Stderr(d) => self.write_stderr(&d),
                _ => {}
            }
        }
    }

    /// Merge sub-events including diagnostics into the current VM buffers.
    fn merge_sub_events_with_diagnostics(&mut self, events: Vec<WorkerEvent>) {
        for e in events {
            match e {
                WorkerEvent::Stdout(d) => self.write_stdout(&d),
                WorkerEvent::Stderr(d) => self.write_stderr(&d),
                WorkerEvent::Diagnostic(level, msg) => self.vm.emit_diagnostic(
                    convert_diag_level(level),
                    wasmsh_vm::DiagCategory::Runtime,
                    msg,
                ),
                _ => {}
            }
        }
    }

    /// Handle `bash`/`sh` commands by reading the script and executing it.
    fn call_shell_script(&mut self, argv: &[String]) {
        if argv.len() < 2 {
            // Interactive shell not supported — just return
            return;
        }

        // Check for -c flag (inline script)
        if argv[1] == "-c" {
            if let Some(script) = argv.get(2) {
                let sub_events = self.execute_input_inner(script);
                self.merge_sub_events_with_diagnostics(sub_events);
            }
            return;
        }

        // Read script file from VFS
        let path = if argv[1].starts_with('/') {
            argv[1].clone()
        } else {
            format!("{}/{}", self.vm.state.cwd, argv[1])
        };
        let Ok(h) = self.fs.open(&path, OpenOptions::read()) else {
            let msg = format!("{}: {}: No such file or directory\n", argv[0], argv[1]);
            self.write_stderr(msg.as_bytes());
            self.vm.state.last_status = 127;
            return;
        };
        let data = self.fs.read_file(h).unwrap_or_default();
        self.fs.close(h);
        let content = String::from_utf8_lossy(&data).to_string();

        // Set positional parameters from remaining argv
        let old_positional = std::mem::take(&mut self.vm.state.positional);
        self.vm.state.positional = argv[1..]
            .iter()
            .map(|s| smol_str::SmolStr::from(s.as_str()))
            .collect();

        let sub_events = self.execute_input_inner(&content);
        self.merge_sub_events_with_diagnostics(sub_events);

        self.vm.state.positional = old_positional;
    }

    fn call_external(&mut self, argv: &[String]) {
        let cmd_name = &argv[0];
        let Ok(stdin) = self.take_external_stdin(cmd_name) else {
            return;
        };
        if let Some(ref mut handler) = self.external_handler {
            if let Some(result) = handler(cmd_name, argv, stdin) {
                self.write_streams(&result.stdout, &result.stderr);
                self.vm.state.last_status = result.status;
            } else {
                let msg = format!("wasmsh: {cmd_name}: command not found\n");
                self.write_stderr(msg.as_bytes());
                self.vm.state.last_status = 127;
            }
        } else {
            let msg = format!("wasmsh: {cmd_name}: command not found\n");
            self.write_stderr(msg.as_bytes());
            self.vm.state.last_status = 127;
        }
    }

    /// Invoke a shell function.
    fn call_shell_function(&mut self, cmd_name: &str, argv: &[String], body: &HirCommand) {
        self.exec.recursion_depth += 1;
        if let Err(reason) = self
            .vm
            .budget
            .enter_recursion(self.vm.limits.recursion_limit)
        {
            self.exec.recursion_depth -= 1;
            self.mark_budget_exhaustion(reason);
            self.write_stderr(b"wasmsh: maximum recursion depth exceeded\n");
            self.vm.state.last_status = 1;
            return;
        }
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
        self.vm.budget.exit_recursion();
        self.exec.recursion_depth -= 1;
    }

    /// Invoke a builtin command.
    fn call_builtin(&mut self, cmd_name: &str, argv: &[String]) {
        let builtin_fn = self.builtins.get(cmd_name).unwrap();
        let Ok(stdin) = self.take_builtin_stdin(cmd_name) else {
            return;
        };
        let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
        let status = {
            let mut router = RuntimeOutputRouter {
                exec: &mut self.exec,
                exec_io: self.current_exec_io.as_mut(),
                proc_subst_out_scopes: &mut self.proc_subst_out_scopes,
                vm_stdout: &mut self.vm.stdout,
                vm_stderr: &mut self.vm.stderr,
                vm_output_bytes: &mut self.vm.output_bytes,
                vm_output_limit: self.vm.limits.output_byte_limit,
                vm_diagnostics: &mut self.vm.diagnostics,
            };
            let mut sink = RuntimeBuiltinSink {
                router: &mut router,
            };
            let mut ctx = wasmsh_builtins::BuiltinContext {
                state: &mut self.vm.state,
                output: &mut sink,
                fs: Some(&self.fs),
                stdin,
            };
            builtin_fn(&mut ctx, &argv_refs)
        };
        self.vm.state.last_status = status;
    }

    /// Extract `-exec CMD [args...] {} \;` from find argv.
    /// Returns `(exec_template, cleaned_argv)` or `None` if no `-exec` present.
    fn extract_find_exec(argv: &[String]) -> Option<(Vec<String>, Vec<String>)> {
        let exec_pos = argv.iter().position(|a| a == "-exec")?;
        // Find the terminator: \; or ;
        let term_pos = argv[exec_pos + 1..]
            .iter()
            .position(|a| a == "\\;" || a == ";")
            .map(|p| p + exec_pos + 1)?;
        let template: Vec<String> = argv[exec_pos + 1..term_pos].to_vec();
        if template.is_empty() {
            return None;
        }
        let mut cleaned: Vec<String> = argv[..exec_pos].to_vec();
        cleaned.extend_from_slice(&argv[term_pos + 1..]);
        Some((template, cleaned))
    }

    /// Shell-quote a path for safe interpolation into a command string.
    fn shell_quote(s: &str) -> String {
        if s.chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '/' | '.' | '_' | '-'))
        {
            s.to_string()
        } else {
            format!("'{}'", s.replace('\'', "'\\''"))
        }
    }

    /// Handle `find ... -exec CMD {} \;` by running find for paths, then executing
    /// the command for each matched path via the shell.
    fn call_find_with_exec(&mut self, argv: &[String]) {
        let Some((template, cleaned_argv)) = Self::extract_find_exec(argv) else {
            // Malformed -exec (missing \;), fall through to normal find
            self.call_utility("find", argv);
            return;
        };

        // Phase 1: run find with cleaned argv, capturing stdout
        let ((), captured) = self.with_output_capture(true, false, |runtime| {
            runtime.call_utility("find", &cleaned_argv);
        });
        let find_output = captured.stdout;

        // Phase 2: parse matched paths
        let paths_str = String::from_utf8_lossy(&find_output);
        let paths: Vec<&str> = paths_str.lines().filter(|l| !l.is_empty()).collect();

        // Phase 3: execute the command for each path
        let mut last_status = 0i32;
        for path in paths {
            let cmd_line: String = template
                .iter()
                .map(|t| {
                    if t == "{}" {
                        Self::shell_quote(path)
                    } else {
                        t.clone()
                    }
                })
                .collect::<Vec<_>>()
                .join(" ");
            let sub_events = self.execute_input_inner(&cmd_line);
            self.merge_sub_events(sub_events);
            if self.vm.state.last_status != 0 {
                last_status = self.vm.state.last_status;
            }
        }
        self.vm.state.last_status = last_status;
    }

    /// Handle `xargs` with actual command execution for non-echo commands.
    /// The existing xargs utility already formats correct command lines for
    /// non-echo; we capture those and execute them via the shell.
    fn call_xargs_with_exec(&mut self, argv: &[String]) {
        // Determine if xargs has a non-echo command by scanning past flags
        let mut has_non_echo = false;
        let mut i = 1;
        while i < argv.len() {
            let arg = &argv[i];
            if matches!(arg.as_str(), "-I" | "-n" | "-d" | "-P" | "-L") && i + 1 < argv.len() {
                i += 2;
            } else if matches!(arg.as_str(), "-0" | "--null" | "-t" | "-p") || arg.starts_with('-')
            {
                i += 1;
            } else {
                // First non-flag arg is the command
                if arg != "echo" {
                    has_non_echo = true;
                }
                break;
            }
        }

        if !has_non_echo {
            self.call_utility("xargs", argv);
            return;
        }

        // Run xargs utility — it outputs formatted command lines for non-echo
        let ((), captured) = self.with_output_capture(true, false, |runtime| {
            runtime.call_utility("xargs", argv);
        });
        let xargs_output = captured.stdout;

        // Execute each output line as a command
        let output_str = String::from_utf8_lossy(&xargs_output);
        let mut last_status = 0i32;
        for line in output_str.lines().filter(|l| !l.is_empty()) {
            let sub_events = self.execute_input_inner(line);
            self.merge_sub_events(sub_events);
            if self.vm.state.last_status != 0 {
                last_status = self.vm.state.last_status;
            }
        }
        self.vm.state.last_status = last_status;
    }

    /// Invoke a utility command.
    fn call_utility(&mut self, cmd_name: &str, argv: &[String]) {
        let Ok(stdin) = self.take_util_stdin(cmd_name) else {
            return;
        };
        let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
        let cwd = self.vm.state.cwd.clone();
        let status = {
            let mut router = RuntimeOutputRouter {
                exec: &mut self.exec,
                exec_io: self.current_exec_io.as_mut(),
                proc_subst_out_scopes: &mut self.proc_subst_out_scopes,
                vm_stdout: &mut self.vm.stdout,
                vm_stderr: &mut self.vm.stderr,
                vm_output_bytes: &mut self.vm.output_bytes,
                vm_output_limit: self.vm.limits.output_byte_limit,
                vm_diagnostics: &mut self.vm.diagnostics,
            };
            let mut output = RuntimeUtilSink {
                router: &mut router,
            };
            let util_fn = self.utils.get(cmd_name).unwrap();
            let mut ctx = UtilContext {
                fs: &mut self.fs,
                output: &mut output,
                cwd: &cwd,
                stdin,
                state: Some(&self.vm.state),
                network: self.network.as_deref(),
            };
            util_fn(&mut ctx, &argv_refs)
        };
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
            if self.check_resource_limits() {
                break;
            }
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
            if self.check_resource_limits() {
                break;
            }
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
            if self.check_resource_limits() {
                break;
            }
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
    fn expand_for_words(&mut self, words: Option<&[Word]>) -> Vec<String> {
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
            if self.check_resource_limits() {
                break;
            }
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
            self.write_stderr(line.as_bytes());
        }

        let Ok(first_line) = self.pending_input_first_line("select") else {
            return;
        };

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
    fn dbl_bracket_expand(&mut self, word: &Word) -> String {
        let resolved = self.resolve_command_subst(std::slice::from_ref(word));
        wasmsh_expand::expand_word(&resolved[0], &mut self.vm.state)
    }

    /// Evaluate a `[[ expression ]]` command. Returns true for exit-status 0.
    fn eval_double_bracket(&mut self, words: &[Word]) -> bool {
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
            let alias_lines: Vec<String> = self
                .aliases
                .iter()
                .map(|(name, value)| format!("alias {name}='{value}'\n"))
                .collect();
            for line in alias_lines {
                self.write_stdout(line.as_bytes());
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
                    self.write_stdout(line.as_bytes());
                } else {
                    let msg = format!("alias: {arg}: not found\n");
                    self.write_stderr(msg.as_bytes());
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
            self.write_stderr(b"unalias: usage: unalias [-a] name ...\n");
            self.vm.state.last_status = 1;
            return;
        }
        for arg in args {
            if arg == "-a" {
                self.aliases.clear();
            } else if self.aliases.shift_remove(arg.as_str()).is_none() {
                let msg = format!("unalias: {arg}: not found\n");
                self.write_stderr(msg.as_bytes());
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
                self.write_stdout(msg.as_bytes());
            } else if self.functions.contains_key(name.as_str()) {
                let msg = format!("{name} is a function\n");
                self.write_stdout(msg.as_bytes());
            } else if self.builtins.is_builtin(name) {
                let msg = format!("{name} is a shell builtin\n");
                self.write_stdout(msg.as_bytes());
            } else if self.utils.is_utility(name) {
                let msg = format!("{name} is a shell utility\n");
                self.write_stdout(msg.as_bytes());
            } else {
                let msg = format!("wasmsh: type: {name}: not found\n");
                self.write_stderr(msg.as_bytes());
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
        let builtin_argv: Vec<String> = argv[1..].to_vec();
        let cmd_name = &builtin_argv[0];
        if self.builtins.is_builtin(cmd_name) {
            self.execute_resolved_command(ResolvedCommand::Builtin, &builtin_argv);
        } else {
            let msg = format!("builtin: {cmd_name}: not a shell builtin\n");
            self.write_stderr(msg.as_bytes());
            self.vm.state.last_status = 1;
        }
    }

    /// Execute `mapfile`/`readarray` — read stdin lines into an indexed array.
    /// Supports `-t` (strip trailing newline). Default array: MAPFILE.
    fn execute_mapfile(&mut self, argv: &[String]) {
        let (strip_newline, array_name) = Self::parse_mapfile_args(&argv[1..]);
        let name_key = smol_str::SmolStr::from(array_name.as_str());
        self.vm.state.init_indexed_array(name_key.clone());
        let Ok(reader) = self.take_pending_input_reader("mapfile") else {
            return;
        };
        if let Some(mut reader) = reader {
            if self
                .populate_mapfile_array_from_reader(&name_key, &mut reader, strip_newline)
                .is_err()
            {
                return;
            }
        } else {
            self.populate_mapfile_array(&name_key, "", strip_newline);
        }
        self.vm.state.last_status = 0;
    }

    fn parse_mapfile_args(args: &[String]) -> (bool, String) {
        let mut strip_newline = false;
        let mut positional: Vec<&str> = Vec::new();
        for arg in args {
            match arg.as_str() {
                "-t" => strip_newline = true,
                _ => positional.push(arg),
            }
        }
        let array_name = positional
            .last()
            .map_or("MAPFILE".to_string(), ToString::to_string);
        (strip_newline, array_name)
    }

    fn populate_mapfile_array(
        &mut self,
        name_key: &smol_str::SmolStr,
        text: &str,
        strip_newline: bool,
    ) {
        let mut idx = 0;
        for line in text.split('\n') {
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
    }

    fn populate_mapfile_array_from_reader(
        &mut self,
        name_key: &smol_str::SmolStr,
        reader: &mut dyn Read,
        strip_newline: bool,
    ) -> Result<(), ()> {
        let mut buffer = [0u8; 4096];
        let mut current = Vec::new();
        let mut idx = 0usize;
        let mut saw_any = false;

        loop {
            let read = match reader.read(&mut buffer) {
                Ok(read) => read,
                Err(err) => {
                    let msg = format!("wasmsh: mapfile: stdin read error: {err}\n");
                    self.write_stderr(msg.as_bytes());
                    self.vm.state.last_status = 1;
                    return Err(());
                }
            };
            if read == 0 {
                break;
            }
            saw_any = true;
            for &byte in &buffer[..read] {
                if byte == b'\n' {
                    let value = if strip_newline {
                        String::from_utf8_lossy(&current).to_string()
                    } else {
                        let mut line = current.clone();
                        line.push(b'\n');
                        String::from_utf8_lossy(&line).to_string()
                    };
                    self.vm.state.set_array_element(
                        name_key.clone(),
                        &idx.to_string(),
                        smol_str::SmolStr::from(value.as_str()),
                    );
                    idx += 1;
                    current.clear();
                } else {
                    current.push(byte);
                }
            }
        }

        if !current.is_empty() || !saw_any {
            let value = if strip_newline {
                String::from_utf8_lossy(&current).to_string()
            } else {
                let mut line = current;
                line.push(b'\n');
                String::from_utf8_lossy(&line).to_string()
            };
            self.vm.state.set_array_element(
                name_key.clone(),
                &idx.to_string(),
                smol_str::SmolStr::from(value.as_str()),
            );
        }

        Ok(())
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
            self.write_stdout(line.as_bytes());
        }
        self.vm.state.last_status = 0;
    }

    fn reject_invalid_shopt_name(&mut self, name: &str) -> bool {
        if Self::SHOPT_OPTIONS.contains(&name) {
            return false;
        }

        let msg = format!("shopt: {name}: invalid shell option name\n");
        self.write_stderr(msg.as_bytes());
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
                self.write_stdout(line.as_bytes());
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
                    self.write_stdout(line.as_bytes());
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
        let trimmed = val.trim();
        if trimmed.starts_with('(') && trimmed.ends_with(')') {
            self.declare_assign_compound(name, &trimmed[1..trimmed.len() - 1], flags);
            return;
        }
        let final_val = Self::transform_declare_scalar(trimmed, flags, &mut self.vm.state);
        self.vm.state.set_var(
            smol_str::SmolStr::from(name),
            smol_str::SmolStr::from(final_val.as_str()),
        );
    }

    fn declare_assign_compound(&mut self, name: &str, inner: &str, flags: &DeclareFlags) {
        let name_key = smol_str::SmolStr::from(name);
        if flags.is_assoc || inner.contains("]=") {
            self.declare_assign_assoc_compound(&name_key, inner);
        } else {
            self.declare_assign_indexed_compound(&name_key, inner);
        }
    }

    fn declare_assign_assoc_compound(&mut self, name_key: &smol_str::SmolStr, inner: &str) {
        self.vm.state.init_assoc_array(name_key.clone());
        for pair in Self::parse_assoc_pairs(inner) {
            self.vm.state.set_array_element(
                name_key.clone(),
                &pair.0,
                smol_str::SmolStr::from(pair.1.as_str()),
            );
        }
    }

    fn declare_assign_indexed_compound(&mut self, name_key: &smol_str::SmolStr, inner: &str) {
        let elements = Self::parse_array_elements(inner);
        self.vm.state.init_indexed_array(name_key.clone());
        for (i, elem) in elements.iter().enumerate() {
            self.vm
                .state
                .set_array_element(name_key.clone(), &i.to_string(), elem.clone());
        }
    }

    fn transform_declare_scalar(val: &str, flags: &DeclareFlags, state: &mut ShellState) -> String {
        if flags.is_integer {
            wasmsh_expand::eval_arithmetic(val, state).to_string()
        } else if flags.is_lower {
            val.to_lowercase()
        } else if flags.is_upper {
            val.to_uppercase()
        } else {
            val.to_string()
        }
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
        self.exec.break_depth > 0
            || self.exec.loop_continue
            || self.exec.exit_requested.is_some()
            || self.exec.resource_exhausted
    }

    /// Check resource limits (step budget, output limit, cancellation).
    /// Returns true if execution should stop. Emits a diagnostic on first violation.
    fn check_resource_limits(&mut self) -> bool {
        if self.exec.resource_exhausted {
            return true;
        }
        if self.vm.begin_step().is_err() {
            self.exec.resource_exhausted = true;
            self.exec.stop_reason = self.vm.stop_reason().cloned();
            return true;
        }
        false
    }

    fn execute_body(&mut self, body: &[HirCompleteCommand]) {
        for cc in body {
            if self.should_stop_execution() || self.check_resource_limits() {
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
            self.execute_and_or(and_or);
            if self.should_errexit(and_or) {
                self.exec.exit_requested = Some(self.vm.state.last_status);
            }
        }
    }

    /// Expand a word value via command substitution and word expansion.
    fn expand_assignment_value(&mut self, value: Option<&Word>) -> String {
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
    fn execute_assignment(&mut self, raw_name: &smol_str::SmolStr, value: Option<&Word>) {
        let (name_str, is_append) = Self::split_assignment_name(raw_name.as_str());
        if self.try_assign_array_element(name_str, value) {
            return;
        }

        let val_str = self.expand_assignment_value(value);
        let trimmed = val_str.trim();
        if trimmed.starts_with('(') && trimmed.ends_with(')') {
            self.assign_compound_array(name_str, trimmed, is_append);
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

    fn try_assign_array_element(&mut self, name: &str, value: Option<&Word>) -> bool {
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
            self.assign_assoc_array(&name_key, inner);
            return;
        }
        self.assign_indexed_array(&name_key, &elements);
    }

    fn is_assoc_array_assignment(inner: &str, elements: &[smol_str::SmolStr]) -> bool {
        !elements.is_empty() && inner.contains('[') && inner.contains("]=")
    }

    fn assign_assoc_array(&mut self, name_key: &smol_str::SmolStr, inner: &str) {
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
        name_key: &smol_str::SmolStr,
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
        let mut state = ArrayParseState::default();

        for ch in inner.chars() {
            match state.process_char(ch) {
                ArrayCharAction::Append(c) => current.push(c),
                ArrayCharAction::Skip => {}
                ArrayCharAction::SplitField => {
                    Self::push_array_element(&mut elements, &mut current);
                }
            }
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
    /// Expand globs in argv, skipping entries tagged as quoted.
    fn expand_globs_tagged(&mut self, argv: Vec<(String, bool)>) -> Vec<String> {
        if self.vm.state.get_var("SHOPT_f").as_deref() == Some("1") {
            return argv.into_iter().map(|(s, _)| s).collect();
        }
        let nullglob = self.get_shopt_value("nullglob");
        let dotglob = self.get_shopt_value("dotglob");
        let globstar = self.get_shopt_value("globstar");
        let extglob = self.get_shopt_value("extglob");

        let mut result = Vec::new();
        for (arg, quoted) in argv {
            if quoted {
                result.push(arg);
            } else {
                result.extend(self.expand_glob_arg(arg, nullglob, dotglob, globstar, extglob));
            }
        }
        result.truncate(Self::MAX_GLOB_RESULTS);
        result
    }

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

    #[allow(clippy::fn_params_excessive_bools)]
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

    #[allow(clippy::unused_self)]
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
            self.globstar_handle_matched_entry(
                dir,
                segments,
                seg_idx,
                prefix,
                dotglob,
                extglob,
                matches,
                &entry.name,
                is_last,
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn globstar_handle_matched_entry(
        &self,
        dir: &str,
        segments: &[&str],
        seg_idx: usize,
        prefix: &str,
        dotglob: bool,
        extglob: bool,
        matches: &mut Vec<String>,
        name: &str,
        is_last: bool,
    ) {
        let (child_path, child_prefix) = Self::globstar_child_paths(dir, prefix, name);
        if is_last {
            matches.push(child_prefix);
            return;
        }
        let is_dir = self.fs.stat(&child_path).map(|m| m.is_dir).unwrap_or(false);
        if is_dir {
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
                    self.write_stderr(format!("wasmsh: write error: {e}\n").as_bytes());
                }
                self.fs.close(h);
            }
            Err(e) => {
                self.write_stderr(format!("wasmsh: {target}: {e}\n").as_bytes());
            }
        }
    }

    fn current_stdout_len(&self) -> usize {
        for capture in self.exec.output_captures.iter().rev() {
            if capture.capture_stdout {
                return capture.stdout.len();
            }
        }
        self.vm.stdout.len()
    }

    /// Capture stdout data from the given position, truncating the active stdout buffer.
    fn capture_stdout(&mut self, from: usize) -> Vec<u8> {
        for capture in self.exec.output_captures.iter_mut().rev() {
            if capture.capture_stdout {
                let data = capture.stdout[from..].to_vec();
                capture.stdout.truncate(from);
                return data;
            }
        }

        let data = self.vm.stdout[from..].to_vec();
        self.vm.stdout.truncate(from);
        data
    }

    /// Drain the active stderr buffer.
    fn take_stderr(&mut self) -> Vec<u8> {
        for capture in self.exec.output_captures.iter_mut().rev() {
            if capture.capture_stderr {
                return std::mem::take(&mut capture.stderr);
            }
        }
        std::mem::take(&mut self.vm.stderr)
    }

    fn process_subst_out_sink_mut(&mut self, path: &str) -> Option<&mut PendingProcessSubstOut> {
        for scope in self.proc_subst_out_scopes.iter_mut().rev() {
            if let Some(index) = scope.iter().position(|sink| sink.path == path) {
                return scope.get_mut(index);
            }
        }
        None
    }

    fn write_process_subst_out_with_parent(
        &mut self,
        path: &str,
        data: &[u8],
        clear: bool,
    ) -> bool {
        for scope_index in (0..self.proc_subst_out_scopes.len()).rev() {
            let maybe_index = self.proc_subst_out_scopes[scope_index]
                .iter()
                .position(|sink| sink.path == path);
            if let Some(index) = maybe_index {
                let mut sink = self.proc_subst_out_scopes[scope_index].remove(index);
                if clear {
                    sink.clear();
                }
                sink.write_with_parent(self, data);
                self.proc_subst_out_scopes[scope_index].insert(index, sink);
                return true;
            }
        }
        false
    }

    fn prepare_exec_io(&mut self, redirections: &[HirRedirection]) -> Result<Option<ExecIo>, ()> {
        let mut exec_io = self.current_exec_io.clone().unwrap_or_default();
        let mut handled_any = false;

        for redir in redirections {
            match redir.op {
                RedirectionOp::HereDoc | RedirectionOp::HereDocStrip => {
                    handled_any = true;
                    if let Some(body) = &redir.here_doc_body {
                        let expanded =
                            wasmsh_expand::expand_string(&body.content, &mut self.vm.state);
                        exec_io
                            .fds_mut()
                            .set_input(InputTarget::Bytes(expanded.into_bytes()));
                    }
                }
                RedirectionOp::HereString => {
                    handled_any = true;
                    let resolved = self.resolve_command_subst(std::slice::from_ref(&redir.target));
                    let resolved_target = resolved.first().unwrap_or(&redir.target);
                    let content = wasmsh_expand::expand_word(resolved_target, &mut self.vm.state);
                    let mut data = content.into_bytes();
                    data.push(b'\n');
                    exec_io.fds_mut().set_input(InputTarget::Bytes(data));
                }
                RedirectionOp::Input => {
                    handled_any = true;
                    let resolved = self.resolve_command_subst(std::slice::from_ref(&redir.target));
                    let resolved_target = resolved.first().unwrap_or(&redir.target);
                    let target = wasmsh_expand::expand_word(resolved_target, &mut self.vm.state);
                    let path = self.resolve_cwd_path(&target);
                    match self.fs.stat(&path) {
                        Ok(metadata) if !metadata.is_dir => {
                            exec_io.fds_mut().set_input(InputTarget::File {
                                path,
                                remove_after_read: false,
                            });
                        }
                        Ok(_) => {
                            let msg = format!("wasmsh: {target}: Is a directory\n");
                            self.write_stderr(msg.as_bytes());
                            self.vm.state.last_status = 1;
                            return Err(());
                        }
                        Err(_) => {
                            let msg = format!("wasmsh: {target}: No such file or directory\n");
                            self.write_stderr(msg.as_bytes());
                            self.vm.state.last_status = 1;
                            return Err(());
                        }
                    }
                }
                RedirectionOp::Output | RedirectionOp::Append => {
                    handled_any = true;
                    let resolved = self.resolve_command_subst(std::slice::from_ref(&redir.target));
                    let resolved_target = resolved.first().unwrap_or(&redir.target);
                    let target = wasmsh_expand::expand_word(resolved_target, &mut self.vm.state);
                    let path = self.resolve_cwd_path(&target);
                    let destination = if self.process_subst_out_sink_mut(&path).is_some() {
                        if matches!(redir.op, RedirectionOp::Output) {
                            if let Some(sink) = self.process_subst_out_sink_mut(&path) {
                                sink.clear();
                            }
                        }
                        OutputTarget::ProcessSubst { path }
                    } else {
                        match self
                            .fs
                            .open_write_sink(&path, matches!(redir.op, RedirectionOp::Append))
                        {
                            Ok(sink) => OutputTarget::File {
                                path,
                                append: matches!(redir.op, RedirectionOp::Append),
                                sink: Rc::new(RefCell::new(sink)),
                            },
                            Err(err) => {
                                let msg = format!("wasmsh: {target}: {err}\n");
                                self.write_stderr(msg.as_bytes());
                                self.vm.state.last_status = 1;
                                return Err(());
                            }
                        }
                    };
                    match redir.fd.unwrap_or(1) {
                        FD_BOTH => {
                            exec_io.fds_mut().open_output(1, destination.clone());
                            exec_io.fds_mut().open_output(2, destination);
                        }
                        2 => exec_io.fds_mut().open_output(2, destination),
                        _ => exec_io.fds_mut().open_output(1, destination),
                    }
                }
                RedirectionOp::DupOutput => {
                    handled_any = true;
                    let resolved = self.resolve_command_subst(std::slice::from_ref(&redir.target));
                    let resolved_target = resolved.first().unwrap_or(&redir.target);
                    let target = wasmsh_expand::expand_word(resolved_target, &mut self.vm.state);
                    let target_fd = target.parse().unwrap_or(1);
                    let source_fd = redir.fd.unwrap_or(1);
                    exec_io.fds_mut().dup_output(source_fd, target_fd);
                }
                _ => {}
            }
        }

        Ok(handled_any.then_some(exec_io))
    }

    /// Apply redirections: for `>` and `>>`, write captured stdout/stderr to file.
    /// For `<`, read file content (handled pre-execution).
    /// Supports fd-specific redirections (2>, 2>>) and &> (both stdout and stderr).
    fn apply_redirections(&mut self, redirections: &[HirRedirection], stdout_before: usize) {
        for redir in redirections {
            // Resolve command substitutions in redirect targets before expansion
            let resolved = self.resolve_command_subst(std::slice::from_ref(&redir.target));
            let resolved_target = resolved.first().unwrap_or(&redir.target);
            let target = wasmsh_expand::expand_word(resolved_target, &mut self.vm.state);
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
                        let stderr_data = self.take_stderr();
                        self.write_stdout(&stderr_data);
                    } else if source_fd == 1 && target_fd == 2 {
                        let stdout_data = self.capture_stdout(stdout_before);
                        self.write_stderr(&stdout_data);
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
            combined.extend_from_slice(&self.take_stderr());
            combined
        } else if fd == 2 {
            self.take_stderr()
        } else {
            self.capture_stdout(stdout_before)
        };
        if self.write_process_subst_out_with_parent(path, &data, true) {
            return;
        }
        self.write_to_file(path, target, &data, OpenOptions::write());
    }

    /// Apply `>>` append redirection for a specific fd.
    fn apply_append_redir(&mut self, path: &str, target: &str, fd: u32, stdout_before: usize) {
        let data = if fd == 2 {
            self.take_stderr()
        } else {
            self.capture_stdout(stdout_before)
        };
        if self.write_process_subst_out_with_parent(path, &data, false) {
            return;
        }
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
    fs: &BackendFs,
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
    fs: &BackendFs,
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
    fs: &BackendFs,
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
    fs: &BackendFs,
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
    fs: &BackendFs,
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
fn dbl_bracket_try_unary(tokens: &[String], pos: &mut usize, fs: &BackendFs) -> Option<bool> {
    if *pos + 1 >= tokens.len() {
        return None;
    }
    let flag = dbl_bracket_parse_unary_flag(&tokens[*pos])?;
    match flag {
        b'z' | b'n' => Some(dbl_bracket_eval_string_test(tokens, pos, flag)),
        b'f' | b'd' | b'e' | b's' | b'r' | b'w' | b'x' => {
            dbl_bracket_eval_file_test(tokens, pos, flag, fs)
        }
        _ => None,
    }
}

fn dbl_bracket_parse_unary_flag(op: &str) -> Option<u8> {
    if !op.starts_with('-') || op.len() != 2 {
        return None;
    }
    Some(op.as_bytes()[1])
}

fn dbl_bracket_eval_string_test(tokens: &[String], pos: &mut usize, flag: u8) -> bool {
    *pos += 1;
    let arg = &tokens[*pos];
    *pos += 1;
    if flag == b'z' {
        arg.is_empty()
    } else {
        !arg.is_empty()
    }
}

fn dbl_bracket_eval_file_test(
    tokens: &[String],
    pos: &mut usize,
    flag: u8,
    fs: &BackendFs,
) -> Option<bool> {
    if *pos + 2 < tokens.len() && is_binary_op(&tokens[*pos + 2]) {
        return None;
    }
    *pos += 1;
    let path_str = &tokens[*pos];
    *pos += 1;
    Some(eval_file_test(flag, path_str, fs))
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

    let rhs = dbl_bracket_collect_rhs(tokens, pos, &op);
    Some(eval_binary_op(&lhs, &op, &rhs, state))
}

/// Collect the right-hand side for a binary operator. For `=~`, the RHS extends
/// until `&&`, `||`, or end of tokens.
fn dbl_bracket_collect_rhs(tokens: &[String], pos: &mut usize, op: &str) -> String {
    if *pos >= tokens.len() {
        return String::new();
    }
    if op == "=~" {
        return dbl_bracket_collect_regex_rhs(tokens, pos);
    }
    let rhs = tokens[*pos].clone();
    *pos += 1;
    rhs
}

fn dbl_bracket_collect_regex_rhs(tokens: &[String], pos: &mut usize) -> String {
    let mut rhs = String::new();
    while *pos < tokens.len() && tokens[*pos] != "&&" && tokens[*pos] != "||" {
        rhs.push_str(&tokens[*pos]);
        *pos += 1;
    }
    rhs
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
    let a: i64 = lhs.trim().parse().unwrap_or(0);
    let b: i64 = rhs.trim().parse().unwrap_or(0);
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
fn eval_file_test(flag: u8, path: &str, fs: &BackendFs) -> bool {
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

    regex_find_first_match(text, core, anchored_start, anchored_end)
}

fn regex_find_first_match(
    text: &str,
    core: &str,
    anchored_start: bool,
    anchored_end: bool,
) -> Option<Vec<String>> {
    let end = if anchored_start { 0 } else { text.len() };
    for start in 0..=end {
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
        return regex_check_end(ti, text.len(), must_end);
    }

    if pat[pi] == b'(' {
        return regex_match_group(text, ti, pat, pi, must_end, captures);
    }

    regex_match_elem(text, ti, pat, pi, must_end, captures)
}

/// Check if end-of-pattern is valid given anchoring.
fn regex_check_end(ti: usize, text_len: usize, must_end: bool) -> Option<usize> {
    if must_end && ti < text_len {
        None
    } else {
        Some(ti)
    }
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
    let (quant, after_quant_offset) = parse_group_quantifier(pat, close);
    let after_quant = &pat[after_quant_offset..];
    let alternatives = split_alternatives_bytes(inner);

    regex_dispatch_group_quant(
        text,
        ti,
        rest,
        after_quant,
        must_end,
        captures,
        &alternatives,
        quant,
    )
}

fn parse_group_quantifier(pat: &[u8], close: usize) -> (u8, usize) {
    if close + 1 < pat.len() {
        match pat[close + 1] {
            q @ (b'*' | b'+' | b'?') => (q, close + 2),
            _ => (0, close + 1),
        }
    } else {
        (0, close + 1)
    }
}

#[allow(clippy::too_many_arguments)]
fn regex_dispatch_group_quant(
    text: &[u8],
    ti: usize,
    rest: &[u8],
    after_quant: &[u8],
    must_end: bool,
    captures: &mut Vec<(usize, usize)>,
    alternatives: &[Vec<u8>],
    quant: u8,
) -> Option<usize> {
    match quant {
        b'+' => regex_match_group_rep(text, ti, after_quant, must_end, captures, alternatives, 1),
        b'*' => regex_match_group_rep(text, ti, after_quant, must_end, captures, alternatives, 0),
        b'?' => regex_match_group_opt(text, ti, after_quant, must_end, captures, alternatives),
        _ => regex_match_group_exact(text, ti, rest, must_end, captures, alternatives),
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
        if let Some(result) = regex_try_group_rep_at(
            text,
            ti,
            end_pos,
            after,
            must_end,
            captures,
            alternatives,
            min_reps,
            save,
        ) {
            return Some(result);
        }
    }
    captures.truncate(save);
    None
}

#[allow(clippy::too_many_arguments)]
fn regex_try_group_rep_at(
    text: &[u8],
    ti: usize,
    end_pos: usize,
    after: &[u8],
    must_end: bool,
    captures: &mut Vec<(usize, usize)>,
    alternatives: &[Vec<u8>],
    min_reps: usize,
    save: usize,
) -> Option<usize> {
    if !regex_match_group_repeated(text, ti, end_pos, alternatives, min_reps) {
        return None;
    }
    let final_end = regex_match_capturing(text, end_pos, after, 0, must_end, captures)?;
    captures.insert(save, (ti, end_pos));
    Some(final_end)
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
    if let Some(result) =
        regex_try_group_one_alt(text, ti, after, must_end, captures, alternatives, save)
    {
        return Some(result);
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

fn regex_try_group_one_alt(
    text: &[u8],
    ti: usize,
    after: &[u8],
    must_end: bool,
    captures: &mut Vec<(usize, usize)>,
    alternatives: &[Vec<u8>],
    save: usize,
) -> Option<usize> {
    for alt in alternatives {
        captures.truncate(save);
        if let Some(result) =
            regex_try_alt_then_continue(text, ti, alt, after, must_end, captures, save)
        {
            return Some(result);
        }
        captures.truncate(save);
    }
    None
}

fn regex_try_alt_then_continue(
    text: &[u8],
    ti: usize,
    alt: &[u8],
    after: &[u8],
    must_end: bool,
    captures: &mut Vec<(usize, usize)>,
    save: usize,
) -> Option<usize> {
    let end = regex_try_match_at(text, ti, alt)?;
    let final_end = regex_match_capturing(text, end, after, 0, must_end, captures)?;
    captures.insert(save, (ti, end));
    Some(final_end)
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
    regex_try_group_one_alt(
        text,
        ti,
        rest,
        must_end,
        captures,
        alternatives,
        captures.len(),
    )
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
        if let Some(end) = regex_try_alt_and_rest(text, ti, alt, rest) {
            return Some(end);
        }
    }
    None
}

fn regex_try_alt_and_rest(text: &[u8], ti: usize, alt: &[u8], rest: &[u8]) -> Option<usize> {
    let after_alt = regex_try_match_inner(text, ti, alt, 0)?;
    regex_try_match_inner(text, after_alt, rest, 0)
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
    let mut depth = 0i32;
    let mut i = 0;
    while i < pat.len() {
        if pat[i] == b'\\' && i + 1 < pat.len() {
            current.push(pat[i]);
            current.push(pat[i + 1]);
            i += 2;
            continue;
        }
        split_alt_classify_byte(pat[i], &mut depth, &mut current, &mut alternatives);
        i += 1;
    }
    alternatives.push(current);
    alternatives
}

fn split_alt_classify_byte(
    byte: u8,
    depth: &mut i32,
    current: &mut Vec<u8>,
    alternatives: &mut Vec<Vec<u8>>,
) {
    match byte {
        b'(' => {
            *depth += 1;
            current.push(byte);
        }
        b')' => {
            *depth -= 1;
            current.push(byte);
        }
        b'|' if *depth == 0 => {
            alternatives.push(std::mem::take(current));
        }
        _ => {
            current.push(byte);
        }
    }
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
            GlobPatternStep::Star => {
                star_pi = pi;
                star_ni = ni;
                pi += 1;
            }
            GlobPatternStep::Consume(new_pi) | GlobPatternStep::Class(new_pi, true) => {
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
pub fn extglob_match(pattern: &str, name: &str) -> bool {
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
    if extglob_match_recursive(&build_combined(prefix, &[], suffix), name) {
        return true;
    }
    // Try one repetition followed by zero or more
    extglob_try_extend(prefix, alternatives, suffix, name, depth)
}

fn extglob_try_extend(
    prefix: &[u8],
    alternatives: &[Vec<u8>],
    suffix: &[u8],
    name: &[u8],
    depth: u32,
) -> bool {
    let prefix_len = prefix.len();
    for alt in alternatives {
        let new_prefix = build_combined(prefix, alt, &[]);
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
    for alt in alternatives {
        let new_prefix = build_combined(prefix, alt, &[]);
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

    fn first_and_or(source: &str) -> HirAndOr {
        let ast = wasmsh_parse::parse(source).unwrap();
        let hir = wasmsh_hir::lower(&ast);
        hir.items[0].list[0].clone()
    }

    fn get_stdout(events: &[WorkerEvent]) -> String {
        let mut out = Vec::new();
        for event in events {
            if let WorkerEvent::Stdout(data) = event {
                out.extend_from_slice(data);
            }
        }
        String::from_utf8(out).unwrap_or_default()
    }

    fn get_stderr(events: &[WorkerEvent]) -> String {
        let mut out = Vec::new();
        for event in events {
            if let WorkerEvent::Stderr(data) = event {
                out.extend_from_slice(data);
            }
        }
        String::from_utf8(out).unwrap_or_default()
    }

    fn get_exit(events: &[WorkerEvent]) -> i32 {
        events
            .iter()
            .find_map(|event| match event {
                WorkerEvent::Exit(status) => Some(*status),
                _ => None,
            })
            .unwrap_or(-1)
    }

    fn has_output_limit_diagnostic(events: &[WorkerEvent]) -> bool {
        events.iter().any(|event| {
            matches!(
                event,
                WorkerEvent::Diagnostic(_, message) if message.contains("output limit exceeded")
            )
        })
    }

    #[test]
    fn output_limit_exposes_structured_exhaustion_reason() {
        let mut runtime = WorkerRuntime::new();
        runtime.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        runtime.set_output_byte_limit(3);

        let events = runtime.handle_command(HostCommand::Run {
            input: "echo hello".into(),
        });

        assert_eq!(get_exit(&events), 128);
        assert!(has_output_limit_diagnostic(&events));
        assert_eq!(
            runtime.exec.stop_reason,
            Some(StopReason::Exhausted(ExhaustionReason {
                category: BudgetCategory::VisibleOutputBytes,
                used: 6,
                limit: 3,
            }))
        );
    }

    #[test]
    fn recursion_limit_exposes_structured_exhaustion_reason() {
        let mut runtime = WorkerRuntime::new();
        runtime.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        runtime.set_recursion_limit(2);

        let events = runtime.handle_command(HostCommand::Run {
            input: "f(){ f; }\nf".into(),
        });

        assert_eq!(get_exit(&events), 128);
        assert!(get_stderr(&events).contains("maximum recursion depth exceeded"));
        assert_eq!(
            runtime.exec.stop_reason,
            Some(StopReason::Exhausted(ExhaustionReason {
                category: BudgetCategory::RecursionDepth,
                used: 3,
                limit: 2,
            }))
        );
    }

    #[test]
    fn pipe_limit_exposes_structured_exhaustion_reason() {
        let mut runtime = WorkerRuntime::new();
        runtime.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        runtime.set_pipe_byte_limit(1);

        let events = runtime.handle_command(HostCommand::Run {
            input: "printf 'ab' | cat".into(),
        });

        assert_eq!(get_exit(&events), 128);
        assert!(events.iter().any(|event| {
            matches!(
                event,
                WorkerEvent::Diagnostic(_, message) if message.contains("pipe buffer limit exceeded")
            )
        }));
        assert!(matches!(
            runtime.exec.stop_reason,
            Some(StopReason::Exhausted(ExhaustionReason {
                category: BudgetCategory::PipeBytes,
                ..
            }))
        ));
    }

    #[test]
    fn vm_subset_boundary_accepts_simple_builtin_and_or() {
        let runtime = WorkerRuntime::new();
        let program = runtime
            .lower_vm_subset_and_or(&first_and_or("true && echo ok"))
            .expect("simple builtin and/or should lower");
        assert!(!program.instructions.is_empty());
    }

    #[test]
    fn vm_subset_boundary_rejects_multi_stage_pipeline() {
        let runtime = WorkerRuntime::new();
        let reason = runtime
            .lower_vm_subset_and_or(&first_and_or("echo hello | cat"))
            .unwrap_err();
        assert_eq!(
            reason,
            VmSubsetFallbackReason::Lowering(LoweringError::Unsupported(
                "pipeline shape is outside the VM subset"
            ))
        );
    }

    #[test]
    fn vm_subset_boundary_rejects_alias_expansion() {
        let mut runtime = WorkerRuntime::new();
        runtime
            .vm
            .state
            .set_var("SHOPT_expand_aliases".into(), "1".into());
        runtime.aliases.insert("echo".into(), "printf".into());
        let reason = runtime
            .lower_vm_subset_and_or(&first_and_or("echo hello"))
            .unwrap_err();
        assert_eq!(reason, VmSubsetFallbackReason::AliasExpansion);
    }

    #[test]
    fn streaming_yes_head_respects_visible_output_limit() {
        let mut runtime = WorkerRuntime::new();
        runtime.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        runtime.vm.limits.output_byte_limit = 10;

        let events = runtime.handle_command(HostCommand::Run {
            input: "yes | head -n 5".into(),
        });

        assert_eq!(get_stdout(&events), "y\ny\ny\ny\ny\n");
        assert!(!has_output_limit_diagnostic(&events));
    }

    #[test]
    fn streaming_yes_cat_head_respects_visible_output_limit() {
        let mut runtime = WorkerRuntime::new();
        runtime.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        runtime.vm.limits.output_byte_limit = 10;

        let events = runtime.handle_command(HostCommand::Run {
            input: "yes | cat | head -n 5".into(),
        });

        assert_eq!(get_stdout(&events), "y\ny\ny\ny\ny\n");
        assert!(!has_output_limit_diagnostic(&events));
    }

    #[test]
    fn streaming_yes_head_wc_respects_visible_output_limit() {
        let mut runtime = WorkerRuntime::new();
        runtime.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        runtime.vm.limits.output_byte_limit = 8;

        let events = runtime.handle_command(HostCommand::Run {
            input: "yes | head -n 5 | wc -l".into(),
        });

        assert_eq!(get_stdout(&events), "      5\n");
        assert!(!has_output_limit_diagnostic(&events));
    }

    #[test]
    fn streaming_cat_file_head_respects_visible_output_limit() {
        let mut runtime = WorkerRuntime::new();
        runtime.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        runtime.handle_command(HostCommand::WriteFile {
            path: "/big.txt".into(),
            data: b"abcdefghijklmnopqrstuvwxyz".to_vec(),
        });
        runtime.vm.limits.output_byte_limit = 10;

        let events = runtime.handle_command(HostCommand::Run {
            input: "cat /big.txt | head -c 10".into(),
        });

        assert_eq!(get_stdout(&events), "abcdefghij");
        assert!(!has_output_limit_diagnostic(&events));
    }

    #[test]
    fn streaming_yes_tr_head_respects_visible_output_limit() {
        let mut runtime = WorkerRuntime::new();
        runtime.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        runtime.vm.limits.output_byte_limit = 10;

        let events = runtime.handle_command(HostCommand::Run {
            input: "yes | tr y z | head -n 5".into(),
        });

        assert_eq!(get_stdout(&events), "z\nz\nz\nz\nz\n");
        assert!(!has_output_limit_diagnostic(&events));
    }

    #[test]
    fn streaming_yes_grep_head_respects_visible_output_limit() {
        let mut runtime = WorkerRuntime::new();
        runtime.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        runtime.vm.limits.output_byte_limit = 10;

        let events = runtime.handle_command(HostCommand::Run {
            input: "yes | grep y | head -n 5".into(),
        });

        assert_eq!(get_stdout(&events), "y\ny\ny\ny\ny\n");
        assert!(!has_output_limit_diagnostic(&events));
    }

    #[test]
    fn streaming_yes_tee_head_respects_visible_output_limit() {
        let mut runtime = WorkerRuntime::new();
        runtime.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        runtime.vm.limits.output_byte_limit = 10;

        let events = runtime.handle_command(HostCommand::Run {
            input: "yes | tee /tee.txt | head -n 5".into(),
        });

        assert_eq!(get_stdout(&events), "y\ny\ny\ny\ny\n");
        assert!(!has_output_limit_diagnostic(&events));

        let file_events = runtime.handle_command(HostCommand::ReadFile {
            path: "/tee.txt".into(),
        });
        assert_eq!(get_stdout(&file_events), "y\ny\ny\ny\ny\n");
    }

    #[test]
    fn streaming_buffered_sort_tee_cat_preserves_sorted_output() {
        let mut runtime = WorkerRuntime::new();
        runtime.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });

        let events = runtime.handle_command(HostCommand::Run {
            input: "printf 'b\\na\\n' | sort | tee /sorted.txt | cat".into(),
        });

        assert_eq!(get_stdout(&events), "a\nb\n");
        let file_events = runtime.handle_command(HostCommand::ReadFile {
            path: "/sorted.txt".into(),
        });
        assert_eq!(get_stdout(&file_events), "a\nb\n");
    }

    #[test]
    fn streaming_yes_rev_head_respects_visible_output_limit() {
        let mut runtime = WorkerRuntime::new();
        runtime.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        runtime.vm.limits.output_byte_limit = 10;

        let events = runtime.handle_command(HostCommand::Run {
            input: "yes | rev | head -n 5".into(),
        });

        assert_eq!(get_stdout(&events), "y\ny\ny\ny\ny\n");
        assert!(!has_output_limit_diagnostic(&events));
    }

    #[test]
    fn streaming_echo_cut_head_respects_visible_output_limit() {
        let mut runtime = WorkerRuntime::new();
        runtime.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        runtime.vm.limits.output_byte_limit = 6;

        let events = runtime.handle_command(HostCommand::Run {
            input: "echo abc:def | cut -d: -f2 | head -c 4".into(),
        });

        assert_eq!(get_stdout(&events), "def\n");
        assert!(!has_output_limit_diagnostic(&events));
    }

    #[test]
    fn streaming_echo_tail_head_respects_visible_output_limit() {
        let mut runtime = WorkerRuntime::new();
        runtime.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        runtime.vm.limits.output_byte_limit = 3;

        let events = runtime.handle_command(HostCommand::Run {
            input: "echo -e 'a\\nb\\nc' | tail -n 2 | head -n 1".into(),
        });

        assert_eq!(get_stdout(&events), "b\n");
        assert!(!has_output_limit_diagnostic(&events));
    }

    #[test]
    fn streaming_yes_bat_head_respects_visible_output_limit() {
        let mut runtime = WorkerRuntime::new();
        runtime.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        let expected = "    1   │ y\n    2   │ y\n";
        runtime.vm.limits.output_byte_limit = expected.len() as u64;

        let events = runtime.handle_command(HostCommand::Run {
            input: "yes | bat --style=numbers | head -n 2".into(),
        });

        assert_eq!(get_stdout(&events), expected);
        assert!(!has_output_limit_diagnostic(&events));
    }

    #[test]
    fn streaming_yes_sed_head_respects_visible_output_limit() {
        let mut runtime = WorkerRuntime::new();
        runtime.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        runtime.vm.limits.output_byte_limit = 10;

        let events = runtime.handle_command(HostCommand::Run {
            input: "yes | sed 's/y/z/' | head -n 5".into(),
        });

        assert_eq!(get_stdout(&events), "z\nz\nz\nz\nz\n");
        assert!(!has_output_limit_diagnostic(&events));
    }

    #[test]
    fn streaming_echo_paste_serial_head_respects_visible_output_limit() {
        let mut runtime = WorkerRuntime::new();
        runtime.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        runtime.vm.limits.output_byte_limit = 6;

        let events = runtime.handle_command(HostCommand::Run {
            input: "echo -e 'a\\nb\\nc' | paste -s -d , | head -c 6".into(),
        });

        assert_eq!(get_stdout(&events), "a,b,c\n");
        assert!(!has_output_limit_diagnostic(&events));
    }

    #[test]
    fn streaming_echo_column_head_respects_visible_output_limit() {
        let mut runtime = WorkerRuntime::new();
        runtime.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        runtime.vm.limits.output_byte_limit = 4;

        let events = runtime.handle_command(HostCommand::Run {
            input: "echo abc | column | head -c 4".into(),
        });

        assert_eq!(get_stdout(&events), "abc\n");
        assert!(!has_output_limit_diagnostic(&events));
    }

    #[test]
    fn streaming_echo_uniq_head_respects_visible_output_limit() {
        let mut runtime = WorkerRuntime::new();
        runtime.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        runtime.vm.limits.output_byte_limit = 6;

        let events = runtime.handle_command(HostCommand::Run {
            input: "echo -e 'a\\na\\nb' | uniq | head -n 2".into(),
        });

        assert_eq!(get_stdout(&events), "a\nb\n");
        assert!(!has_output_limit_diagnostic(&events));
    }

    #[test]
    fn streaming_buffered_printf_sort_head_respects_visible_output_limit() {
        let mut runtime = WorkerRuntime::new();
        runtime.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        runtime.vm.limits.output_byte_limit = 2;

        let events = runtime.handle_command(HostCommand::Run {
            input: "printf 'b\\na\\n' | sort | head -n 1".into(),
        });

        assert_eq!(get_stdout(&events), "a\n");
        assert!(!has_output_limit_diagnostic(&events));
    }

    #[test]
    fn streaming_buffered_function_stage_preserves_output() {
        let mut runtime = WorkerRuntime::new();
        runtime.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });

        let events = runtime.handle_command(HostCommand::Run {
            input: "f(){ cat; }\nprintf hi | f | head -c 2".into(),
        });

        assert_eq!(get_stdout(&events), "hi");
        assert!(!has_output_limit_diagnostic(&events));
    }

    #[test]
    fn streaming_buffered_function_pipe_stderr_respects_visible_output_limit() {
        let mut runtime = WorkerRuntime::new();
        runtime.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        runtime.vm.limits.output_byte_limit = 8;

        let events = runtime.handle_command(HostCommand::Run {
            input: "f(){ echo out; echo err >&2; }\nf |& head -n 2".into(),
        });

        assert_eq!(get_stdout(&events), "out\nerr\n");
        assert!(!has_output_limit_diagnostic(&events));
    }

    #[test]
    fn scheduled_group_stage_pipe_stderr_preserves_output() {
        let mut runtime = WorkerRuntime::new();
        runtime.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });

        let events = runtime.handle_command(HostCommand::Run {
            input: "printf x | { cat; echo err >&2; } |& cat".into(),
        });

        let stdout = get_stdout(&events);
        assert!(stdout.contains('x'));
        assert!(stdout.contains("err"));
    }

    #[test]
    fn streaming_tee_pipe_stderr_preserves_output_and_stage_status() {
        let mut runtime = WorkerRuntime::new();
        runtime.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });

        let events = runtime.handle_command(HostCommand::Run {
            input: "printf x | tee / |& cat\necho ${PIPESTATUS[*]}".into(),
        });

        let stdout = get_stdout(&events);
        assert!(stdout.contains('x'));
        assert!(stdout.contains("tee: /: is a directory: /"));
        assert!(stdout.contains("0 1 0"));
        assert_eq!(get_stderr(&events), "");
    }

    #[test]
    fn streaming_tee_pipe_stderr_respects_pipefail() {
        let mut runtime = WorkerRuntime::new();
        runtime.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });

        let events = runtime.handle_command(HostCommand::Run {
            input: "set -o pipefail\nprintf x | tee / |& cat".into(),
        });

        assert_eq!(runtime.vm.state.last_status, 1);
        let stdout = get_stdout(&events);
        assert!(stdout.contains('x'));
        assert!(stdout.contains("tee: /: is a directory: /"));
    }

    #[test]
    fn generic_pipeline_capture_does_not_count_hidden_stage_output() {
        let mut runtime = WorkerRuntime::new();
        runtime.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });
        runtime.vm.limits.output_byte_limit = 2;

        let events = runtime.handle_command(HostCommand::Run {
            input: "echo -e 'a\\nb' | grep b".into(),
        });

        assert_eq!(get_stdout(&events), "b\n");
        assert!(!has_output_limit_diagnostic(&events));
    }

    #[test]
    fn generic_pipeline_file_capture_preserves_redirection_behavior() {
        let mut runtime = WorkerRuntime::new();
        runtime.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });

        let events = runtime.handle_command(HostCommand::Run {
            input: "echo -e 'a\\nb' | grep b >/filtered.txt | wc -l".into(),
        });

        assert_eq!(get_stdout(&events), "      0\n");

        let file_events = runtime.handle_command(HostCommand::ReadFile {
            path: "/filtered.txt".into(),
        });
        assert_eq!(get_stdout(&file_events), "b\n");
    }

    #[test]
    fn scheduler_single_redirect_only_command_creates_target_file() {
        let mut runtime = WorkerRuntime::new();
        runtime.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });

        let events = runtime.handle_command(HostCommand::Run {
            input: "> /created.txt".into(),
        });

        assert_eq!(runtime.vm.state.last_status, 0);
        assert_eq!(get_stdout(&events), "");
        assert_eq!(get_stderr(&events), "");

        let file_events = runtime.handle_command(HostCommand::ReadFile {
            path: "/created.txt".into(),
        });
        assert_eq!(get_stdout(&file_events), "");
    }

    #[test]
    fn command_substitution_keeps_inner_stderr_visible() {
        let mut runtime = WorkerRuntime::new();
        runtime.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });

        let events = runtime.handle_command(HostCommand::Run {
            input: "echo $(printf 'hello'; echo err >&2)".into(),
        });

        assert_eq!(get_stdout(&events), "hello\n");
        assert_eq!(get_stderr(&events), "err\n");
    }

    #[test]
    fn command_substitution_isolates_shell_state() {
        let mut runtime = WorkerRuntime::new();
        runtime.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });

        let events = runtime.handle_command(HostCommand::Run {
            input: "foo=before; echo $(foo=after; printf hi); echo $foo".into(),
        });

        assert_eq!(get_stdout(&events), "hi\nbefore\n");
    }

    #[test]
    fn process_substitution_out_feeds_inner_command() {
        let mut runtime = WorkerRuntime::new();
        runtime.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });

        let events = runtime.handle_command(HostCommand::Run {
            input: "printf hi > >(cat)".into(),
        });

        assert_eq!(get_stdout(&events), "hi");
        assert_eq!(get_stderr(&events), "");
        assert_eq!(runtime.vm.state.last_status, 0);
    }

    #[test]
    fn process_substitution_out_runs_schedulable_inner_pipeline() {
        let mut runtime = WorkerRuntime::new();
        runtime.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });

        let events = runtime.handle_command(HostCommand::Run {
            input: "printf 'a\\nb\\n' > >(head -n 1 | cat)".into(),
        });

        assert_eq!(get_stdout(&events), "a\n");
        assert_eq!(get_stderr(&events), "");
        assert_eq!(runtime.vm.state.last_status, 0);
    }

    #[test]
    fn process_substitution_out_runs_live_tail_pipeline() {
        let mut runtime = WorkerRuntime::new();
        runtime.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });

        runtime.proc_subst_out_scopes.push(Vec::new());
        let path = runtime.register_process_subst_out("tail -n 1 | cat");

        {
            let sink = runtime
                .process_subst_out_sink_mut(&path)
                .expect("registered process substitution sink");
            match &sink.mode {
                PendingProcessSubstOutMode::Live { .. } => {}
                PendingProcessSubstOutMode::Buffered { .. } => {
                    panic!("expected live process substitution runner")
                }
            }
            sink.write(b"a\nb\n");
        }

        let scope = runtime.proc_subst_out_scopes.pop().unwrap_or_default();
        runtime.flush_process_subst_out_scope(scope);
        assert_eq!(runtime.vm.stdout, b"b\n");
    }

    #[test]
    fn process_substitution_out_runs_live_buffered_pipeline() {
        let mut runtime = WorkerRuntime::new();
        runtime.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });

        runtime.proc_subst_out_scopes.push(Vec::new());
        let path = runtime.register_process_subst_out("sort | cat");

        {
            let sink = runtime
                .process_subst_out_sink_mut(&path)
                .expect("registered process substitution sink");
            match &sink.mode {
                PendingProcessSubstOutMode::Live { runner } => {
                    assert!(runner.isolated_runtime.is_some());
                }
                PendingProcessSubstOutMode::Buffered { .. } => {
                    panic!("expected live buffered process substitution runner")
                }
            }
            sink.write(b"b\na\n");
        }

        let scope = runtime.proc_subst_out_scopes.pop().unwrap_or_default();
        runtime.flush_process_subst_out_scope(scope);
        assert_eq!(runtime.vm.stdout, b"a\nb\n");
    }

    #[test]
    fn process_substitution_in_registers_live_reader_and_cleans_up() {
        let mut runtime = WorkerRuntime::new();
        runtime.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });

        runtime.proc_subst_in_scopes.push(Vec::new());
        let path = runtime
            .execute_process_subst_in("yes | head -n 2")
            .to_string();
        assert!(runtime.fs.stat(&path).is_ok());

        let file = runtime.handle_command(HostCommand::ReadFile { path: path.clone() });
        assert_eq!(get_stdout(&file), "y\ny\n");
        assert!(runtime.fs.stat(&path).is_err());

        let scope = runtime.proc_subst_in_scopes.pop().unwrap_or_default();
        runtime.flush_process_subst_in_scope(scope);
        assert!(runtime.fs.stat(&path).is_err());
    }

    #[test]
    fn process_substitution_in_registers_live_sed_reader_and_cleans_up() {
        let mut runtime = WorkerRuntime::new();
        runtime.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });

        runtime.proc_subst_in_scopes.push(Vec::new());
        let path = runtime
            .execute_process_subst_in("yes | sed 's/y/z/' | head -n 2")
            .to_string();
        assert!(runtime.fs.stat(&path).is_ok());

        let file = runtime.handle_command(HostCommand::ReadFile { path: path.clone() });
        assert_eq!(get_stdout(&file), "z\nz\n");
        assert!(runtime.fs.stat(&path).is_err());

        let scope = runtime.proc_subst_in_scopes.pop().unwrap_or_default();
        runtime.flush_process_subst_in_scope(scope);
        assert!(runtime.fs.stat(&path).is_err());
    }

    #[test]
    fn process_substitution_in_runs_live_buffered_reader_and_cleans_up() {
        let mut runtime = WorkerRuntime::new();
        runtime.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });

        runtime.proc_subst_in_scopes.push(Vec::new());
        let path = runtime
            .execute_process_subst_in("printf 'b\\na\\n' | sort")
            .to_string();

        assert!(runtime.fs.stat(&path).is_ok());
        let file = runtime.handle_command(HostCommand::ReadFile { path: path.clone() });
        assert_eq!(get_stdout(&file), "a\nb\n");
        assert!(runtime.fs.stat(&path).is_err());

        let scope = runtime.proc_subst_in_scopes.pop().unwrap_or_default();
        runtime.flush_process_subst_in_scope(scope);
        assert!(runtime.fs.stat(&path).is_err());
    }

    #[test]
    fn live_process_substitution_runner_consumes_before_flush() {
        let mut runtime = WorkerRuntime::new();
        runtime.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });

        runtime.proc_subst_out_scopes.push(Vec::new());
        let path = runtime.register_process_subst_out("head -n 1 | cat");

        {
            let sink = runtime
                .process_subst_out_sink_mut(&path)
                .expect("registered process substitution sink");
            sink.write(b"a\nb\n");
            match &sink.mode {
                PendingProcessSubstOutMode::Live { runner } => {
                    assert_eq!(runner.captured_stdout, b"a\n");
                }
                PendingProcessSubstOutMode::Buffered { .. } => {
                    panic!("expected live process substitution runner")
                }
            }
        }

        let scope = runtime.proc_subst_out_scopes.pop().unwrap_or_default();
        runtime.flush_process_subst_out_scope(scope);
        assert_eq!(runtime.vm.stdout, b"a\n");
    }

    #[test]
    fn live_process_substitution_runner_tee_writes_before_flush() {
        let mut runtime = WorkerRuntime::new();
        runtime.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });

        runtime.proc_subst_out_scopes.push(Vec::new());
        let path = runtime.register_process_subst_out("tee /tee.txt | cat");

        {
            let sink = runtime
                .process_subst_out_sink_mut(&path)
                .expect("registered process substitution sink");
            sink.write(b"a\nb\n");
            match &sink.mode {
                PendingProcessSubstOutMode::Live { runner } => {
                    assert!(runner.captured_stdout.starts_with(b"a\nb"));
                }
                PendingProcessSubstOutMode::Buffered { .. } => {
                    panic!("expected live process substitution runner")
                }
            }
        }

        let file = runtime.handle_command(HostCommand::ReadFile {
            path: "/tee.txt".into(),
        });
        assert!(get_stdout(&file).starts_with("a\nb"));

        let scope = runtime.proc_subst_out_scopes.pop().unwrap_or_default();
        runtime.flush_process_subst_out_scope(scope);
        assert_eq!(runtime.vm.stdout, b"a\nb\n");

        let file = runtime.handle_command(HostCommand::ReadFile {
            path: "/tee.txt".into(),
        });
        assert_eq!(get_stdout(&file), "a\nb\n");
    }

    #[test]
    fn exec_live_redirections_preserve_left_to_right_dup_order() {
        let mut runtime = WorkerRuntime::new();
        runtime.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });

        let events = runtime.handle_command(HostCommand::Run {
            input: "printf hi > /first.txt 1>&2\nprintf hi 1>&2 > /second.txt".into(),
        });

        assert_eq!(get_stdout(&events), "");
        assert_eq!(get_stderr(&events), "hi");

        let first = runtime.handle_command(HostCommand::ReadFile {
            path: "/first.txt".into(),
        });
        assert_eq!(get_stdout(&first), "");

        let second = runtime.handle_command(HostCommand::ReadFile {
            path: "/second.txt".into(),
        });
        assert_eq!(get_stdout(&second), "hi");
    }

    #[test]
    fn exec_process_subst_redirections_preserve_left_to_right_dup_order() {
        let mut runtime = WorkerRuntime::new();
        runtime.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });

        let events = runtime.handle_command(HostCommand::Run {
            input: "printf hi > >(cat) 1>&2\nprintf hi 1>&2 > >(cat)".into(),
        });

        assert_eq!(get_stdout(&events), "hi");
        assert_eq!(get_stderr(&events), "hi");
    }

    #[test]
    fn builtin_and_utility_redirections_write_files_during_execution() {
        let mut runtime = WorkerRuntime::new();
        runtime.handle_command(HostCommand::Init {
            step_budget: 0,
            allowed_hosts: vec![],
        });

        let events = runtime.handle_command(HostCommand::Run {
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

        let builtin = runtime.handle_command(HostCommand::ReadFile {
            path: "/builtin.txt".into(),
        });
        assert!(get_stdout(&builtin).contains("printf"));

        let utility = runtime.handle_command(HostCommand::ReadFile {
            path: "/utility.txt".into(),
        });
        assert_eq!(get_stdout(&utility), "hi");
    }
}
