//! Signal metadata used by the runtime.
//!
//! [`RUNTIME_SIGNAL_SPECS`] lists every signal the runtime knows about,
//! along with the shell variable names used by `trap` bookkeeping and the
//! default disposition the runtime applies when no handler is installed.
//! [`find_runtime_signal_spec`] accepts either a decimal signal number or
//! a `SIG`-prefixed / unprefixed name.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SignalDefaultAction {
    Terminate,
    Ignore,
    StopLike,
    ContinueLike,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RuntimeSignalSpec {
    pub(crate) name: &'static str,
    pub(crate) number: i32,
    pub(crate) handler_var: &'static str,
    pub(crate) ignore_var: &'static str,
    pub(crate) default_action: SignalDefaultAction,
    pub(crate) trappable: bool,
}

pub(crate) const RUNTIME_SIGNAL_SPECS: &[RuntimeSignalSpec] = &[
    RuntimeSignalSpec {
        name: "HUP",
        number: 1,
        handler_var: "_TRAP_SIG_HUP",
        ignore_var: "_TRAP_IGNORE_SIG_HUP",
        default_action: SignalDefaultAction::Terminate,
        trappable: true,
    },
    RuntimeSignalSpec {
        name: "INT",
        number: 2,
        handler_var: "_TRAP_SIG_INT",
        ignore_var: "_TRAP_IGNORE_SIG_INT",
        default_action: SignalDefaultAction::Terminate,
        trappable: true,
    },
    RuntimeSignalSpec {
        name: "QUIT",
        number: 3,
        handler_var: "_TRAP_SIG_QUIT",
        ignore_var: "_TRAP_IGNORE_SIG_QUIT",
        default_action: SignalDefaultAction::Terminate,
        trappable: true,
    },
    RuntimeSignalSpec {
        name: "ILL",
        number: 4,
        handler_var: "_TRAP_SIG_ILL",
        ignore_var: "_TRAP_IGNORE_SIG_ILL",
        default_action: SignalDefaultAction::Terminate,
        trappable: true,
    },
    RuntimeSignalSpec {
        name: "ABRT",
        number: 6,
        handler_var: "_TRAP_SIG_ABRT",
        ignore_var: "_TRAP_IGNORE_SIG_ABRT",
        default_action: SignalDefaultAction::Terminate,
        trappable: true,
    },
    RuntimeSignalSpec {
        name: "FPE",
        number: 8,
        handler_var: "_TRAP_SIG_FPE",
        ignore_var: "_TRAP_IGNORE_SIG_FPE",
        default_action: SignalDefaultAction::Terminate,
        trappable: true,
    },
    RuntimeSignalSpec {
        name: "KILL",
        number: 9,
        handler_var: "_TRAP_SIG_KILL",
        ignore_var: "_TRAP_IGNORE_SIG_KILL",
        default_action: SignalDefaultAction::Terminate,
        trappable: false,
    },
    RuntimeSignalSpec {
        name: "USR1",
        number: 10,
        handler_var: "_TRAP_SIG_USR1",
        ignore_var: "_TRAP_IGNORE_SIG_USR1",
        default_action: SignalDefaultAction::Terminate,
        trappable: true,
    },
    RuntimeSignalSpec {
        name: "SEGV",
        number: 11,
        handler_var: "_TRAP_SIG_SEGV",
        ignore_var: "_TRAP_IGNORE_SIG_SEGV",
        default_action: SignalDefaultAction::Terminate,
        trappable: true,
    },
    RuntimeSignalSpec {
        name: "USR2",
        number: 12,
        handler_var: "_TRAP_SIG_USR2",
        ignore_var: "_TRAP_IGNORE_SIG_USR2",
        default_action: SignalDefaultAction::Terminate,
        trappable: true,
    },
    RuntimeSignalSpec {
        name: "PIPE",
        number: 13,
        handler_var: "_TRAP_SIG_PIPE",
        ignore_var: "_TRAP_IGNORE_SIG_PIPE",
        default_action: SignalDefaultAction::Terminate,
        trappable: true,
    },
    RuntimeSignalSpec {
        name: "ALRM",
        number: 14,
        handler_var: "_TRAP_SIG_ALRM",
        ignore_var: "_TRAP_IGNORE_SIG_ALRM",
        default_action: SignalDefaultAction::Terminate,
        trappable: true,
    },
    RuntimeSignalSpec {
        name: "TERM",
        number: 15,
        handler_var: "_TRAP_SIG_TERM",
        ignore_var: "_TRAP_IGNORE_SIG_TERM",
        default_action: SignalDefaultAction::Terminate,
        trappable: true,
    },
    RuntimeSignalSpec {
        name: "CHLD",
        number: 17,
        handler_var: "_TRAP_SIG_CHLD",
        ignore_var: "_TRAP_IGNORE_SIG_CHLD",
        default_action: SignalDefaultAction::Ignore,
        trappable: true,
    },
    RuntimeSignalSpec {
        name: "CONT",
        number: 18,
        handler_var: "_TRAP_SIG_CONT",
        ignore_var: "_TRAP_IGNORE_SIG_CONT",
        default_action: SignalDefaultAction::ContinueLike,
        trappable: true,
    },
    RuntimeSignalSpec {
        name: "STOP",
        number: 19,
        handler_var: "_TRAP_SIG_STOP",
        ignore_var: "_TRAP_IGNORE_SIG_STOP",
        default_action: SignalDefaultAction::StopLike,
        trappable: false,
    },
    RuntimeSignalSpec {
        name: "TSTP",
        number: 20,
        handler_var: "_TRAP_SIG_TSTP",
        ignore_var: "_TRAP_IGNORE_SIG_TSTP",
        default_action: SignalDefaultAction::StopLike,
        trappable: true,
    },
    RuntimeSignalSpec {
        name: "TTIN",
        number: 21,
        handler_var: "_TRAP_SIG_TTIN",
        ignore_var: "_TRAP_IGNORE_SIG_TTIN",
        default_action: SignalDefaultAction::StopLike,
        trappable: true,
    },
    RuntimeSignalSpec {
        name: "TTOU",
        number: 22,
        handler_var: "_TRAP_SIG_TTOU",
        ignore_var: "_TRAP_IGNORE_SIG_TTOU",
        default_action: SignalDefaultAction::StopLike,
        trappable: true,
    },
    RuntimeSignalSpec {
        name: "WINCH",
        number: 28,
        handler_var: "_TRAP_SIG_WINCH",
        ignore_var: "_TRAP_IGNORE_SIG_WINCH",
        default_action: SignalDefaultAction::Ignore,
        trappable: true,
    },
];

pub(crate) fn find_runtime_signal_spec(name: &str) -> Option<&'static RuntimeSignalSpec> {
    if let Ok(number) = name.parse::<i32>() {
        return RUNTIME_SIGNAL_SPECS
            .iter()
            .find(|spec| spec.number == number);
    }
    let normalized = name
        .strip_prefix("SIG")
        .unwrap_or(name)
        .to_ascii_uppercase();
    RUNTIME_SIGNAL_SPECS
        .iter()
        .find(|spec| spec.name == normalized)
}
