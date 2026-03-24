//! Shell runtime state for wasmsh.
//!
//! Manages variables, positional parameters, function registry,
//! working directory, and exit status.

use std::cell::Cell;

use indexmap::IndexMap;
use smol_str::SmolStr;

/// The value held by a shell variable: scalar, indexed array, or associative array.
#[derive(Debug, Clone, PartialEq)]
pub enum VarValue {
    Scalar(SmolStr),
    IndexedArray(IndexMap<usize, SmolStr>),
    AssocArray(IndexMap<SmolStr, SmolStr>),
}

impl VarValue {
    /// Return a scalar representation: for scalars the value itself, for arrays
    /// all values joined by a single space (matching bash behavior when an array
    /// is accessed without a subscript).
    #[must_use]
    pub fn as_scalar(&self) -> SmolStr {
        match self {
            Self::Scalar(s) => s.clone(),
            Self::IndexedArray(map) => {
                let vals: Vec<&str> = map.values().map(SmolStr::as_str).collect();
                SmolStr::from(vals.join(" "))
            }
            Self::AssocArray(map) => {
                let vals: Vec<&str> = map.values().map(SmolStr::as_str).collect();
                SmolStr::from(vals.join(" "))
            }
        }
    }
}

/// A shell variable with its attributes.
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::struct_excessive_bools)]
pub struct ShellVar {
    pub value: VarValue,
    pub exported: bool,
    pub readonly: bool,
    /// `declare -i`: auto-evaluate arithmetic on assignment.
    pub integer: bool,
    /// `declare -n`: nameref — value is the name of another variable.
    pub nameref: bool,
}

impl ShellVar {
    /// Convenience: create a scalar `ShellVar` with the given value and default flags.
    pub fn scalar(value: SmolStr) -> Self {
        Self {
            value: VarValue::Scalar(value),
            exported: false,
            readonly: false,
            integer: false,
            nameref: false,
        }
    }
}

/// The shell environment: a stack of variable scopes.
#[derive(Debug, Clone)]
pub struct ShellEnv {
    pub scopes: Vec<IndexMap<SmolStr, ShellVar>>,
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

    /// Get a mutable reference to a variable by name.
    pub fn get_mut(&mut self, name: &str) -> Option<&mut ShellVar> {
        for scope in self.scopes.iter_mut().rev() {
            if let Some(var) = scope.get_mut(name) {
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
                    result.insert(name.clone(), var.value.as_scalar());
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
    /// Current line number ($LINENO), updated by the runtime.
    pub lineno: u32,
    /// PRNG seed for `$RANDOM` (`XorShift32`). Uses `Cell` so `get_var` can remain `&self`.
    pub random_seed: Cell<u32>,
    /// Seconds elapsed since shell start ($SECONDS).
    #[cfg(not(target_arch = "wasm32"))]
    pub start_time: std::time::Instant,
    /// Function call stack for $FUNCNAME.
    pub func_stack: Vec<SmolStr>,
    /// Source file stack for `$BASH_SOURCE`.
    pub source_stack: Vec<SmolStr>,
}

impl ShellState {
    #[must_use]
    pub fn new() -> Self {
        Self {
            env: ShellEnv::new(),
            positional: Vec::new(),
            last_status: 0,
            cwd: "/".into(),
            lineno: 0,
            random_seed: Cell::new(12345),
            #[cfg(not(target_arch = "wasm32"))]
            start_time: std::time::Instant::now(),
            func_stack: Vec::new(),
            source_stack: Vec::new(),
        }
    }

    /// Advance the `XorShift32` PRNG and return a value in 0..32767.
    fn next_random(&self) -> u32 {
        let mut x = self.random_seed.get();
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.random_seed.set(x);
        x % 32768
    }

    /// Get the value of a special parameter or named variable.
    /// For arrays accessed as scalars, returns all values joined by space.
    /// Dynamic variables (`$RANDOM`, `$LINENO`, `$SECONDS`, `$FUNCNAME`, `$BASH_SOURCE`) are
    /// resolved on access.
    #[must_use]
    pub fn get_var(&self, name: &str) -> Option<SmolStr> {
        match name {
            "?" => Some(self.last_status.to_string().into()),
            "#" => Some(self.positional.len().to_string().into()),
            "0" => Some("wasmsh".into()),
            "@" | "*" => Some(self.positional.join(" ").into()),
            "RANDOM" => Some(SmolStr::from(self.next_random().to_string())),
            "LINENO" => Some(SmolStr::from(self.lineno.to_string())),
            "SECONDS" => {
                #[cfg(not(target_arch = "wasm32"))]
                {
                    let elapsed = self.start_time.elapsed().as_secs();
                    Some(SmolStr::from(elapsed.to_string()))
                }
                #[cfg(target_arch = "wasm32")]
                {
                    Some(SmolStr::from("0"))
                }
            }
            "FUNCNAME" => {
                if let Some(name) = self.func_stack.last() {
                    Some(name.clone())
                } else {
                    Some(SmolStr::default())
                }
            }
            "BASH_SOURCE" => {
                if let Some(src) = self.source_stack.last() {
                    Some(src.clone())
                } else {
                    Some(SmolStr::default())
                }
            }
            _ => {
                // Positional parameter ($1, $2, ...)
                if let Ok(n) = name.parse::<usize>() {
                    if n >= 1 {
                        return self.positional.get(n - 1).cloned();
                    }
                }
                if let Some(var) = self.env.get(name) {
                    if var.nameref {
                        // Follow the nameref: value is the name of another variable
                        let target = var.value.as_scalar();
                        if !target.is_empty() && target.as_str() != name {
                            return self.env.get(&target).map(|v| v.value.as_scalar());
                        }
                    }
                    Some(var.value.as_scalar())
                } else {
                    None
                }
            }
        }
    }

    /// Set a named variable (not a special parameter).
    /// Preserves `exported` and `readonly` flags if the variable already exists.
    /// If the variable is readonly, the write is silently skipped (like bash).
    /// Setting a scalar value on an existing array variable replaces element 0
    /// for indexed arrays, or replaces it entirely with a scalar for assoc arrays.
    pub fn set_var(&mut self, name: SmolStr, value: SmolStr) {
        // Follow nameref: if this variable is a nameref, write to the target instead
        if let Some(var) = self.env.get(&name) {
            if var.nameref {
                let target = var.value.as_scalar();
                if !target.is_empty() && target.as_str() != name.as_str() {
                    let target_name = SmolStr::from(target.as_str());
                    self.set_var(target_name, value);
                    return;
                }
            }
        }
        let (exported, readonly) = self
            .env
            .get(&name)
            .map_or((false, false), |v| (v.exported, v.readonly));
        if readonly {
            return; // readonly variables cannot be overwritten
        }
        // allexport (set -a): auto-export all variables except internal SHOPT_* vars
        let exported = if !exported
            && !name.starts_with("SHOPT_")
            && !name.starts_with('_')
            && self
                .env
                .get("SHOPT_a")
                .is_some_and(|v| matches!(&v.value, VarValue::Scalar(s) if s == "1"))
        {
            true
        } else {
            exported
        };
        // Preserve existing integer/nameref attributes
        let (integer, nameref) = self
            .env
            .get(&name)
            .map_or((false, false), |v| (v.integer, v.nameref));
        self.env.set(
            name,
            ShellVar {
                value: VarValue::Scalar(value),
                exported,
                readonly,
                integer,
                nameref,
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
        let (exported, integer, nameref) = self.env.get(&name).map_or((false, false, false), |v| {
            (v.exported, v.integer, v.nameref)
        });
        self.env.set(
            name,
            ShellVar {
                value: VarValue::Scalar(value),
                exported,
                readonly: true,
                integer,
                nameref,
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

    /// Return all variable names (across all scopes) that start with the given prefix.
    /// Innermost scope wins for shadowed names. Results are sorted alphabetically.
    #[must_use]
    pub fn var_names_with_prefix(&self, prefix: &str) -> Vec<SmolStr> {
        let mut seen = IndexMap::<SmolStr, ()>::new();
        // Walk from innermost to outermost so inner names are encountered first.
        for scope in self.env.scopes.iter().rev() {
            for name in scope.keys() {
                if name.starts_with(prefix) {
                    seen.entry(name.clone()).or_default();
                }
            }
        }
        let mut names: Vec<SmolStr> = seen.into_keys().collect();
        names.sort();
        names
    }

    // ---- Array methods ----

    /// Get a single element from an array (or scalar when index is "0").
    /// For indexed arrays, the index is parsed as `usize`.
    /// For associative arrays, the index is used as-is.
    /// For scalars, index "0" returns the scalar value.
    #[must_use]
    pub fn get_array_element(&self, name: &str, index: &str) -> Option<SmolStr> {
        let var = self.env.get(name)?;
        match &var.value {
            VarValue::Scalar(s) => {
                if index == "0" {
                    Some(s.clone())
                } else {
                    None
                }
            }
            VarValue::IndexedArray(map) => {
                let idx: usize = index.parse().ok()?;
                map.get(&idx).cloned()
            }
            VarValue::AssocArray(map) => map.get(index).cloned(),
        }
    }

    /// Set a single element in an array. Creates an indexed array if the variable
    /// does not exist. Converts a scalar to an indexed array if needed.
    pub fn set_array_element(&mut self, name: SmolStr, index: &str, value: SmolStr) {
        let (exported, readonly) = self
            .env
            .get(&name)
            .map_or((false, false), |v| (v.exported, v.readonly));
        if readonly {
            return;
        }

        if let Some(var) = self.env.get_mut(&name) {
            match &mut var.value {
                VarValue::IndexedArray(map) => {
                    if let Ok(idx) = index.parse::<usize>() {
                        map.insert(idx, value);
                    }
                }
                VarValue::AssocArray(map) => {
                    map.insert(SmolStr::from(index), value);
                }
                VarValue::Scalar(_) => {
                    // Convert scalar to indexed array
                    let mut map = IndexMap::new();
                    if let Ok(idx) = index.parse::<usize>() {
                        map.insert(idx, value);
                    }
                    var.value = VarValue::IndexedArray(map);
                }
            }
        } else {
            // Variable doesn't exist; create indexed array
            let mut map = IndexMap::new();
            if let Ok(idx) = index.parse::<usize>() {
                map.insert(idx, value);
            }
            self.env.set(
                name,
                ShellVar {
                    value: VarValue::IndexedArray(map),
                    exported,
                    readonly,
                    integer: false,
                    nameref: false,
                },
            );
        }
    }

    /// Get all keys/indices of an array variable.
    #[must_use]
    pub fn get_array_keys(&self, name: &str) -> Vec<String> {
        let Some(var) = self.env.get(name) else {
            return Vec::new();
        };
        match &var.value {
            VarValue::Scalar(s) => {
                if s.is_empty() {
                    Vec::new()
                } else {
                    vec!["0".to_string()]
                }
            }
            VarValue::IndexedArray(map) => map.keys().map(ToString::to_string).collect(),
            VarValue::AssocArray(map) => map.keys().map(ToString::to_string).collect(),
        }
    }

    /// Get all values of an array variable.
    #[must_use]
    pub fn get_array_values(&self, name: &str) -> Vec<SmolStr> {
        let Some(var) = self.env.get(name) else {
            return Vec::new();
        };
        match &var.value {
            VarValue::Scalar(s) => {
                if s.is_empty() {
                    Vec::new()
                } else {
                    vec![s.clone()]
                }
            }
            VarValue::IndexedArray(map) => map.values().cloned().collect(),
            VarValue::AssocArray(map) => map.values().cloned().collect(),
        }
    }

    /// Get the number of elements in an array.
    #[must_use]
    pub fn get_array_length(&self, name: &str) -> usize {
        let Some(var) = self.env.get(name) else {
            return 0;
        };
        match &var.value {
            VarValue::Scalar(s) => usize::from(!s.is_empty()),
            VarValue::IndexedArray(map) => map.len(),
            VarValue::AssocArray(map) => map.len(),
        }
    }

    /// Append values to an indexed array (`arr+=(val1 val2)`).
    /// If the variable is a scalar, it is first converted to an indexed array
    /// with the scalar as element 0.
    pub fn append_array(&mut self, name: &str, values: Vec<SmolStr>) {
        if let Some(var) = self.env.get(name) {
            if var.readonly {
                return;
            }
        }

        let name_key = SmolStr::from(name);
        if let Some(var) = self.env.get_mut(name) {
            match &mut var.value {
                VarValue::IndexedArray(map) => {
                    let next = map.keys().max().map_or(0, |k| k + 1);
                    for (i, v) in values.into_iter().enumerate() {
                        map.insert(next + i, v);
                    }
                }
                VarValue::AssocArray(_) => {
                    // Bash doesn't support += for assoc arrays in the same way;
                    // silently ignore.
                }
                VarValue::Scalar(s) => {
                    let mut map = IndexMap::new();
                    if !s.is_empty() {
                        map.insert(0, s.clone());
                    }
                    let next = map.keys().max().map_or(0, |k| k + 1);
                    for (i, v) in values.into_iter().enumerate() {
                        map.insert(next + i, v);
                    }
                    var.value = VarValue::IndexedArray(map);
                }
            }
        } else {
            // Variable doesn't exist; create indexed array
            let mut map = IndexMap::new();
            for (i, v) in values.into_iter().enumerate() {
                map.insert(i, v);
            }
            self.env.set(
                name_key,
                ShellVar {
                    value: VarValue::IndexedArray(map),
                    exported: false,
                    readonly: false,
                    integer: false,
                    nameref: false,
                },
            );
        }
    }

    /// Remove a single element from an array.
    pub fn unset_array_element(&mut self, name: &str, index: &str) {
        if let Some(var) = self.env.get(name) {
            if var.readonly {
                return;
            }
        }
        if let Some(var) = self.env.get_mut(name) {
            match &mut var.value {
                VarValue::IndexedArray(map) => {
                    if let Ok(idx) = index.parse::<usize>() {
                        map.shift_remove(&idx);
                    }
                }
                VarValue::AssocArray(map) => {
                    map.shift_remove(index);
                }
                VarValue::Scalar(_) => {
                    if index == "0" {
                        var.value = VarValue::Scalar(SmolStr::default());
                    }
                }
            }
        }
    }

    /// Initialize an empty indexed array variable.
    pub fn init_indexed_array(&mut self, name: SmolStr) {
        let (exported, readonly) = self
            .env
            .get(&name)
            .map_or((false, false), |v| (v.exported, v.readonly));
        if readonly {
            return;
        }
        self.env.set(
            name,
            ShellVar {
                value: VarValue::IndexedArray(IndexMap::new()),
                exported,
                readonly,
                integer: false,
                nameref: false,
            },
        );
    }

    /// Initialize an empty associative array variable.
    pub fn init_assoc_array(&mut self, name: SmolStr) {
        let (exported, readonly) = self
            .env
            .get(&name)
            .map_or((false, false), |v| (v.exported, v.readonly));
        if readonly {
            return;
        }
        self.env.set(
            name,
            ShellVar {
                value: VarValue::AssocArray(IndexMap::new()),
                exported,
                readonly,
                integer: false,
                nameref: false,
            },
        );
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
                value: VarValue::Scalar("/bin".into()),
                exported: true,
                readonly: false,
                integer: false,
                nameref: false,
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
                value: VarValue::Scalar("old".into()),
                exported: true,
                readonly: false,
                integer: false,
                nameref: false,
            },
        );
        state.set_var("X".into(), "new".into());
        let var = state.env.get("X").unwrap();
        assert_eq!(var.value.as_scalar(), "new");
        assert!(var.exported); // preserved
    }

    // ---- Array tests ----

    #[test]
    fn indexed_array_basics() {
        let mut state = ShellState::new();
        state.init_indexed_array("arr".into());
        state.set_array_element("arr".into(), "0", "zero".into());
        state.set_array_element("arr".into(), "1", "one".into());
        state.set_array_element("arr".into(), "2", "two".into());

        assert_eq!(state.get_array_element("arr", "0").unwrap(), "zero");
        assert_eq!(state.get_array_element("arr", "1").unwrap(), "one");
        assert_eq!(state.get_array_element("arr", "2").unwrap(), "two");
        assert!(state.get_array_element("arr", "3").is_none());

        assert_eq!(state.get_array_length("arr"), 3);
        assert_eq!(state.get_array_keys("arr"), vec!["0", "1", "2"]);
        assert_eq!(
            state.get_array_values("arr"),
            vec![
                SmolStr::from("zero"),
                SmolStr::from("one"),
                SmolStr::from("two")
            ]
        );
    }

    #[test]
    fn assoc_array_basics() {
        let mut state = ShellState::new();
        state.init_assoc_array("map".into());
        state.set_array_element("map".into(), "key1", "val1".into());
        state.set_array_element("map".into(), "key2", "val2".into());

        assert_eq!(state.get_array_element("map", "key1").unwrap(), "val1");
        assert_eq!(state.get_array_element("map", "key2").unwrap(), "val2");
        assert!(state.get_array_element("map", "key3").is_none());

        assert_eq!(state.get_array_length("map"), 2);
    }

    #[test]
    fn array_scalar_access() {
        let mut state = ShellState::new();
        state.init_indexed_array("arr".into());
        state.set_array_element("arr".into(), "0", "a".into());
        state.set_array_element("arr".into(), "1", "b".into());
        // Accessing array as scalar returns space-joined values
        assert_eq!(state.get_var("arr").unwrap(), "a b");
    }

    #[test]
    fn append_array_values() {
        let mut state = ShellState::new();
        state.init_indexed_array("arr".into());
        state.set_array_element("arr".into(), "0", "a".into());
        state.append_array("arr", vec!["b".into(), "c".into()]);
        assert_eq!(state.get_array_length("arr"), 3);
        assert_eq!(state.get_array_element("arr", "1").unwrap(), "b");
        assert_eq!(state.get_array_element("arr", "2").unwrap(), "c");
    }

    #[test]
    fn unset_array_element_removes() {
        let mut state = ShellState::new();
        state.init_indexed_array("arr".into());
        state.set_array_element("arr".into(), "0", "a".into());
        state.set_array_element("arr".into(), "1", "b".into());
        state.unset_array_element("arr", "0");
        assert!(state.get_array_element("arr", "0").is_none());
        assert_eq!(state.get_array_element("arr", "1").unwrap(), "b");
        assert_eq!(state.get_array_length("arr"), 1);
    }

    #[test]
    fn scalar_as_array_element_0() {
        let mut state = ShellState::new();
        state.set_var("X".into(), "hello".into());
        // Scalars can be accessed as element 0
        assert_eq!(state.get_array_element("X", "0").unwrap(), "hello");
        assert!(state.get_array_element("X", "1").is_none());
    }

    #[test]
    fn set_element_creates_indexed_array() {
        let mut state = ShellState::new();
        state.set_array_element("arr".into(), "5", "five".into());
        assert_eq!(state.get_array_element("arr", "5").unwrap(), "five");
        assert_eq!(state.get_array_length("arr"), 1);
    }

    // ---- Dynamic variable tests ----

    #[test]
    fn random_returns_bounded_value() {
        let state = ShellState::new();
        let val: u32 = state.get_var("RANDOM").unwrap().parse().unwrap();
        assert!(val < 32768);
    }

    #[test]
    fn random_changes_each_call() {
        let state = ShellState::new();
        let v1 = state.get_var("RANDOM").unwrap();
        let v2 = state.get_var("RANDOM").unwrap();
        // Successive calls should produce different values
        assert_ne!(v1, v2);
    }

    #[test]
    fn lineno_returns_current_value() {
        let mut state = ShellState::new();
        state.lineno = 42;
        assert_eq!(state.get_var("LINENO").unwrap(), "42");
    }

    #[test]
    fn seconds_returns_value() {
        let state = ShellState::new();
        let val = state.get_var("SECONDS").unwrap();
        // Should parse as a number and be >= 0
        let secs: u64 = val.parse().unwrap();
        assert!(secs < 60); // test runs quickly
    }

    #[test]
    fn funcname_empty_by_default() {
        let state = ShellState::new();
        assert_eq!(state.get_var("FUNCNAME").unwrap(), "");
    }

    #[test]
    fn funcname_returns_top_of_stack() {
        let mut state = ShellState::new();
        state.func_stack.push("myfunc".into());
        assert_eq!(state.get_var("FUNCNAME").unwrap(), "myfunc");
    }

    #[test]
    fn bash_source_returns_top_of_stack() {
        let mut state = ShellState::new();
        state.source_stack.push("/script.sh".into());
        assert_eq!(state.get_var("BASH_SOURCE").unwrap(), "/script.sh");
    }
}
