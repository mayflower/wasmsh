//! Feature gate registry for the TOML test runner.
//!
//! Tests declare `requires = ["feature1", "feature2"]`. If any required
//! feature is not in the implemented set, the test is skipped.

use std::collections::HashSet;

/// Returns the set of features currently implemented in wasmsh.
#[must_use]
pub fn implemented_features() -> HashSet<&'static str> {
    let mut f = HashSet::new();

    // Shell syntax
    f.insert("simple-command");
    f.insert("pipeline");
    f.insert("and-or-list");
    f.insert("semicolon-list");
    f.insert("pipeline-negation");
    f.insert("variable-assignment");
    f.insert("redirection");
    f.insert("here-doc");
    f.insert("here-doc-expansion");
    f.insert("single-quoting");
    f.insert("double-quoting");
    f.insert("backslash-escape");
    f.insert("parameter-expansion");
    f.insert("parameter-default");
    f.insert("parameter-assign-default");
    f.insert("parameter-alternative");
    f.insert("parameter-error");
    f.insert("parameter-length");
    f.insert("arithmetic-expansion");
    f.insert("nested-expansion");
    f.insert("comment");
    f.insert("if");
    f.insert("while");
    f.insert("until");
    f.insert("for-in");
    f.insert("case");
    f.insert("subshell");
    f.insert("brace-group");
    f.insert("function");

    // Builtins
    f.insert("echo");
    f.insert("printf");
    f.insert("pwd");
    f.insert("cd");
    f.insert("export");
    f.insert("unset");
    f.insert("readonly");
    f.insert("test");
    f.insert("read");
    f.insert("true");
    f.insert("false");
    f.insert("colon");

    // Utilities
    f.insert("cat");
    f.insert("ls");
    f.insert("mkdir");
    f.insert("rm");
    f.insert("touch");
    f.insert("head");
    f.insert("tail");
    f.insert("wc");
    f.insert("grep");
    f.insert("sed");
    f.insert("sort");
    f.insert("uniq");
    f.insert("cut");
    f.insert("tr");
    f.insert("tee");
    f.insert("seq");
    f.insert("basename");
    f.insert("dirname");
    f.insert("mv");
    f.insert("cp");
    f.insert("env");
    f.insert("printenv");
    f.insert("id");
    f.insert("whoami");
    f.insert("uname");
    f.insert("hostname");
    f.insert("sleep");
    f.insert("expr");
    f.insert("xargs");

    // Shell features
    f.insert("glob-expansion");
    f.insert("brace-expansion");
    f.insert("here-string");
    f.insert("ansi-c-quoting");
    f.insert("stderr-redirection");
    f.insert("fd-redirection");
    f.insert("tilde-expansion");
    f.insert("command-substitution");
    f.insert("parameter-substitution");
    f.insert("parameter-substring");
    f.insert("break");
    f.insert("continue");
    f.insert("exit");
    f.insert("return");
    f.insert("local");
    f.insert("shift");
    f.insert("set");
    f.insert("type");
    f.insert("command-builtin");
    f.insert("eval");
    f.insert("source");
    f.insert("getopts");
    f.insert("trap");
    f.insert("ln");
    f.insert("readlink");
    f.insert("realpath");
    f.insert("stat");
    f.insert("find");
    f.insert("chmod");
    f.insert("date");
    f.insert("echo-escape");
    f.insert("printf-repeat");
    f.insert("errexit");
    f.insert("trap-exit");
    f.insert("parameter-strip");

    f
}

/// Check which required features are missing.
pub fn missing_features(requires: &[String]) -> Vec<String> {
    let implemented = implemented_features();
    requires
        .iter()
        .filter(|r| !implemented.contains(r.as_str()))
        .cloned()
        .collect()
}
