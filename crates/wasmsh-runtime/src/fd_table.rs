#![allow(dead_code)]

use std::cell::RefCell;
use std::rc::Rc;

use wasmsh_fs::VfsWriteSink;
use wasmsh_vm::pipe::PipeBuffer;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum InputKind {
    Inherit,
    Bytes(Vec<u8>),
    File {
        path: String,
        remove_after_read: bool,
    },
    Pipe,
    Closed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum OutputKind {
    InheritStdout,
    InheritStderr,
    File { path: String, append: bool },
    ProcessSubst { path: String },
    Pipe,
    Closed,
}

#[derive(Clone)]
pub(crate) enum InputTarget {
    Inherit,
    Bytes(Vec<u8>),
    File {
        path: String,
        remove_after_read: bool,
    },
    Pipe(Rc<RefCell<PipeBuffer>>),
    Closed,
}

impl std::fmt::Debug for InputTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("InputTarget").field(&self.kind()).finish()
    }
}

impl InputTarget {
    pub(crate) fn kind(&self) -> InputKind {
        match self {
            Self::Inherit => InputKind::Inherit,
            Self::Bytes(data) => InputKind::Bytes(data.clone()),
            Self::File {
                path,
                remove_after_read,
            } => InputKind::File {
                path: path.clone(),
                remove_after_read: *remove_after_read,
            },
            Self::Pipe(_) => InputKind::Pipe,
            Self::Closed => InputKind::Closed,
        }
    }
}

#[derive(Clone)]
pub(crate) enum OutputTarget {
    InheritStdout,
    InheritStderr,
    File {
        path: String,
        append: bool,
        sink: Rc<RefCell<Box<dyn VfsWriteSink>>>,
    },
    ProcessSubst {
        path: String,
    },
    Pipe(Rc<RefCell<PipeBuffer>>),
    Closed,
}

impl std::fmt::Debug for OutputTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("OutputTarget").field(&self.kind()).finish()
    }
}

impl OutputTarget {
    pub(crate) fn kind(&self) -> OutputKind {
        match self {
            Self::InheritStdout => OutputKind::InheritStdout,
            Self::InheritStderr => OutputKind::InheritStderr,
            Self::File { path, append, .. } => OutputKind::File {
                path: path.clone(),
                append: *append,
            },
            Self::ProcessSubst { path } => OutputKind::ProcessSubst { path: path.clone() },
            Self::Pipe(_) => OutputKind::Pipe,
            Self::Closed => OutputKind::Closed,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct FdTable {
    stdin: InputTarget,
    stdout: OutputTarget,
    stderr: OutputTarget,
}

impl Default for FdTable {
    fn default() -> Self {
        Self {
            stdin: InputTarget::Inherit,
            stdout: OutputTarget::InheritStdout,
            stderr: OutputTarget::InheritStderr,
        }
    }
}

impl FdTable {
    pub(crate) fn stdin_kind(&self) -> InputKind {
        self.stdin.kind()
    }

    pub(crate) fn stdout_kind(&self) -> OutputKind {
        self.stdout.kind()
    }

    pub(crate) fn stderr_kind(&self) -> OutputKind {
        self.stderr.kind()
    }

    pub(crate) fn stdin_target(&self) -> &InputTarget {
        &self.stdin
    }

    pub(crate) fn stdout_target(&self) -> &OutputTarget {
        &self.stdout
    }

    pub(crate) fn stderr_target(&self) -> &OutputTarget {
        &self.stderr
    }

    pub(crate) fn take_stdin(&mut self) -> InputTarget {
        std::mem::replace(&mut self.stdin, InputTarget::Inherit)
    }

    pub(crate) fn set_input(&mut self, target: InputTarget) {
        self.stdin = target;
    }

    pub(crate) fn open_output(&mut self, fd: u32, target: OutputTarget) {
        match fd {
            2 => self.stderr = target,
            _ => self.stdout = target,
        }
    }

    pub(crate) fn dup_output(&mut self, source_fd: u32, target_fd: u32) {
        let target = match target_fd {
            2 => self.stderr.clone(),
            _ => self.stdout.clone(),
        };
        match source_fd {
            2 => self.stderr = target,
            _ => self.stdout = target,
        }
    }

    pub(crate) fn dup_input(&mut self, source_fd: u32, target_fd: u32) {
        if source_fd != 0 {
            return;
        }
        let target = match target_fd {
            0 => self.stdin.clone(),
            _ => InputTarget::Closed,
        };
        self.stdin = target;
    }

    pub(crate) fn close(&mut self, fd: u32) {
        match fd {
            0 => self.stdin = InputTarget::Closed,
            2 => self.stderr = OutputTarget::Closed,
            _ => self.stdout = OutputTarget::Closed,
        }
    }

    pub(crate) fn connect_pipe(&mut self, fd: u32, pipe: Rc<RefCell<PipeBuffer>>) {
        self.open_output(fd, OutputTarget::Pipe(pipe));
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ExecIo {
    fds: FdTable,
}

impl ExecIo {
    pub(crate) fn fds(&self) -> &FdTable {
        &self.fds
    }

    pub(crate) fn fds_mut(&mut self) -> &mut FdTable {
        &mut self.fds
    }

    pub(crate) fn take_stdin(&mut self) -> InputTarget {
        self.fds.take_stdin()
    }

    pub(crate) fn output_target(&self, stdout: bool) -> OutputTarget {
        if stdout {
            self.fds.stdout_target().clone()
        } else {
            self.fds.stderr_target().clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_file_for_stdout() {
        let mut table = FdTable::default();
        table.open_output(
            1,
            OutputTarget::ProcessSubst {
                path: "/out.txt".into(),
            },
        );

        assert_eq!(
            table.stdout_kind(),
            OutputKind::ProcessSubst {
                path: "/out.txt".into(),
            }
        );
    }

    #[test]
    fn dup_stderr_to_current_stdout_target() {
        let mut table = FdTable::default();
        table.open_output(
            1,
            OutputTarget::ProcessSubst {
                path: "/out.txt".into(),
            },
        );
        table.dup_output(2, 1);

        assert_eq!(table.stderr_kind(), table.stdout_kind());
    }

    #[test]
    fn command_scoped_redirects_restore_by_cloning_base_table() {
        let base = FdTable::default();
        let mut scoped = base.clone();
        scoped.close(1);

        assert_eq!(base.stdout_kind(), OutputKind::InheritStdout);
        assert_eq!(scoped.stdout_kind(), OutputKind::Closed);
    }

    #[test]
    fn pipe_endpoints_can_be_bound_as_fd_targets() {
        let mut table = FdTable::default();
        table.connect_pipe(1, Rc::new(RefCell::new(PipeBuffer::new(8))));

        assert_eq!(table.stdout_kind(), OutputKind::Pipe);
    }
}
