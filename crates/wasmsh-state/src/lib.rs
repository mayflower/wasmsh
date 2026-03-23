//! Shell runtime state for wasmsh.
//!
//! Manages variables, positional parameters, function registry,
//! working directory, and exit status.

use indexmap::IndexMap;
use smol_str::SmolStr;

/// A shell variable with its attributes.
#[derive(Debug, Clone, PartialEq)]
pub struct ShellVar {
    pub value: SmolStr,
    pub exported: bool,
    pub readonly: bool,
}

/// The shell environment: a stack of variable scopes.
#[derive(Debug, Clone)]
pub struct ShellEnv {
    scopes: Vec<IndexMap<SmolStr, ShellVar>>,
}

impl ShellEnv {
    #[must_use]
    pub fn new() -> Self {
        Self {
            scopes: vec![IndexMap::new()],
        }
    }

    /// Look up a variable by name, searching from innermost scope outward.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&ShellVar> {
        for scope in self.scopes.iter().rev() {
            if let Some(var) = scope.get(name) {
                return Some(var);
            }
        }
        None
    }

    /// Set a variable in the current (innermost) scope.
    pub fn set(&mut self, name: SmolStr, var: ShellVar) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name, var);
        }
    }

    /// Push a new scope (e.g. for a function call).
    pub fn push_scope(&mut self) {
        self.scopes.push(IndexMap::new());
    }

    /// Pop the innermost scope. Returns `None` if only the global scope remains.
    pub fn pop_scope(&mut self) -> Option<IndexMap<SmolStr, ShellVar>> {
        if self.scopes.len() > 1 {
            self.scopes.pop()
        } else {
            None
        }
    }

    /// Remove a variable from the current (innermost) scope.
    pub fn remove(&mut self, name: &str) -> Option<ShellVar> {
        if let Some(scope) = self.scopes.last_mut() {
            scope.shift_remove(name)
        } else {
            None
        }
    }

    /// Iterate over all exported variables across all scopes
    /// (innermost wins for shadowed names).
    pub fn exported_vars(&self) -> IndexMap<SmolStr, SmolStr> {
        let mut result = IndexMap::new();
        for scope in &self.scopes {
            for (name, var) in scope {
                if var.exported {
                    result.insert(name.clone(), var.value.clone());
                }
            }
        }
        result
    }
}

impl Default for ShellEnv {
    fn default() -> Self {
        Self::new()
    }
}

/// Complete shell state: variables, positional params, status, cwd, functions.
#[derive(Debug, Clone)]
pub struct ShellState {
    /// Variable scopes.
    pub env: ShellEnv,
    /// Positional parameters ($1, $2, ...).
    pub positional: Vec<SmolStr>,
    /// Last exit status ($?).
    pub last_status: i32,
    /// Current working directory ($PWD).
    pub cwd: String,
}

impl ShellState {
    #[must_use]
    pub fn new() -> Self {
        Self {
            env: ShellEnv::new(),
            positional: Vec::new(),
            last_status: 0,
            cwd: "/".into(),
        }
    }

    /// Get the value of a special parameter or named variable.
    #[must_use]
    pub fn get_var(&self, name: &str) -> Option<SmolStr> {
        match name {
            "?" => Some(self.last_status.to_string().into()),
            "#" => Some(self.positional.len().to_string().into()),
            "0" => Some("wasmsh".into()),
            "@" | "*" => Some(self.positional.join(" ").into()),
            _ => {
                // Positional parameter ($1, $2, ...)
                if let Ok(n) = name.parse::<usize>() {
                    if n >= 1 {
                        return self.positional.get(n - 1).cloned();
                    }
                }
                self.env.get(name).map(|v| v.value.clone())
            }
        }
    }

    /// Set a named variable (not a special parameter).
    /// Preserves `exported` and `readonly` flags if the variable already exists.
    pub fn set_var(&mut self, name: SmolStr, value: SmolStr) {
        let (exported, readonly) = self
            .env
            .get(&name)
            .map_or((false, false), |v| (v.exported, v.readonly));
        self.env.set(
            name,
            ShellVar {
                value,
                exported,
                readonly,
            },
        );
    }

    /// Set a variable, returning an error if it is readonly.
    pub fn set_var_checked(&mut self, name: SmolStr, value: SmolStr) -> Result<(), String> {
        if let Some(var) = self.env.get(&name) {
            if var.readonly {
                return Err(format!("{name}: readonly variable"));
            }
        }
        self.set_var(name, value);
        Ok(())
    }

    /// Mark a variable as readonly with the given value.
    pub fn set_readonly(&mut self, name: SmolStr, value: SmolStr) {
        let exported = self.env.get(&name).map_or(false, |v| v.exported);
        self.env.set(
            name,
            ShellVar {
                value,
                exported,
                readonly: true,
            },
        );
    }

    /// Remove a variable. Returns error if readonly.
    pub fn unset_var(&mut self, name: &str) -> Result<(), String> {
        if let Some(var) = self.env.get(name) {
            if var.readonly {
                return Err(format!("{name}: readonly variable"));
            }
        }
        self.env.remove(name);
        Ok(())
    }
}

impl Default for ShellState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_set_variable() {
        let mut state = ShellState::new();
        state.set_var("FOO".into(), "bar".into());
        assert_eq!(state.get_var("FOO").unwrap(), "bar");
    }

    #[test]
    fn special_params() {
        let mut state = ShellState::new();
        state.last_status = 42;
        assert_eq!(state.get_var("?").unwrap(), "42");
        assert_eq!(state.get_var("#").unwrap(), "0");
        assert_eq!(state.get_var("0").unwrap(), "wasmsh");
    }

    #[test]
    fn positional_params() {
        let mut state = ShellState::new();
        state.positional = vec!["a".into(), "b".into(), "c".into()];
        assert_eq!(state.get_var("1").unwrap(), "a");
        assert_eq!(state.get_var("2").unwrap(), "b");
        assert_eq!(state.get_var("3").unwrap(), "c");
        assert!(state.get_var("4").is_none());
        assert_eq!(state.get_var("#").unwrap(), "3");
    }

    #[test]
    fn scope_shadowing() {
        let mut state = ShellState::new();
        state.set_var("X".into(), "global".into());
        state.env.push_scope();
        state.set_var("X".into(), "local".into());
        assert_eq!(state.get_var("X").unwrap(), "local");
        state.env.pop_scope();
        assert_eq!(state.get_var("X").unwrap(), "global");
    }

    #[test]
    fn exported_vars() {
        let mut state = ShellState::new();
        state.env.set(
            "PATH".into(),
            ShellVar {
                value: "/bin".into(),
                exported: true,
                readonly: false,
            },
        );
        state.set_var("LOCAL".into(), "val".into());
        let exports = state.env.exported_vars();
        assert_eq!(exports.len(), 1);
        assert_eq!(exports["PATH"], "/bin");
    }

    #[test]
    fn unset_var_removes() {
        let mut state = ShellState::new();
        state.set_var("X".into(), "val".into());
        assert!(state.get_var("X").is_some());
        state.unset_var("X").unwrap();
        assert!(state.get_var("X").is_none());
    }

    #[test]
    fn readonly_prevents_set() {
        let mut state = ShellState::new();
        state.set_readonly("X".into(), "locked".into());
        assert!(state.set_var_checked("X".into(), "new".into()).is_err());
        assert_eq!(state.get_var("X").unwrap(), "locked");
    }

    #[test]
    fn readonly_prevents_unset() {
        let mut state = ShellState::new();
        state.set_readonly("X".into(), "locked".into());
        assert!(state.unset_var("X").is_err());
        assert!(state.get_var("X").is_some());
    }

    #[test]
    fn set_var_preserves_exported_flag() {
        let mut state = ShellState::new();
        state.env.set(
            "X".into(),
            ShellVar {
                value: "old".into(),
                exported: true,
                readonly: false,
            },
        );
        state.set_var("X".into(), "new".into());
        let var = state.env.get("X").unwrap();
        assert_eq!(var.value, "new");
        assert!(var.exported); // preserved
    }
}
