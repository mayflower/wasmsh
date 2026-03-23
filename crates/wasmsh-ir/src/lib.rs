//! Linear instruction representation for the wasmsh VM.
//!
//! IR instructions form a flat sequence executed by the VM.
//! Each instruction is a typed enum variant rather than an
//! opcode+operand pair, for clarity at this stage.

use smol_str::SmolStr;

/// A single IR instruction for the VM.
#[derive(Debug, Clone, PartialEq)]
pub enum Ir {
    /// Set a shell variable.
    SetVar { name: SmolStr, value: SmolStr },
    /// Push an argument onto the current argv.
    PushArg { value: SmolStr },
    /// Invoke a builtin command using the current argv.
    CallBuiltin { name: SmolStr },
    /// Invoke a utility using the current argv.
    CallUtility { name: SmolStr },
    /// Set exit status and halt.
    Return { status: i32 },
    /// No operation (used for padding / debugging).
    Nop,
}

/// A compiled program: a sequence of IR instructions.
#[derive(Debug, Clone, PartialEq)]
pub struct IrProgram {
    pub instructions: Vec<Ir>,
}

impl IrProgram {
    #[must_use]
    pub fn new(instructions: Vec<Ir>) -> Self {
        Self { instructions }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ir_program_construction() {
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
        assert_eq!(prog.instructions.len(), 4);
    }
}
