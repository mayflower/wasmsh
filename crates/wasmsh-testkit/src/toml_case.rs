//! TOML test case schema for declarative shell tests.
//!
//! Supports the full requirements from wasmsh-testsuite-requirements.md:
//! - Test classification (normative-posix, compat-bash, etc.)
//! - VFS state verification (expect.files)
//! - Shell state verification (expect.env)
//! - Performance budgets (`expect.max_time_ms`)
//! - Oracle comparison
//! - Known divergence documentation

use serde::Deserialize;
use std::collections::HashMap;

/// A complete TOML test case file.
#[derive(Debug, Clone, Deserialize)]
pub struct TomlTestFile {
    pub test: TestMeta,
    #[serde(default)]
    pub setup: TestSetup,
    pub input: TestInput,
    #[serde(default)]
    pub expect: TestExpect,
    #[serde(default)]
    pub oracle: Option<OracleConfig>,
    #[serde(default)]
    pub known_divergence: Option<KnownDivergence>,
}

/// Test metadata: name, tags, tier, classification, required features.
#[derive(Debug, Clone, Deserialize)]
pub struct TestMeta {
    pub name: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default = "default_tier")]
    pub tier: String,
    #[serde(default)]
    pub requires: Vec<String>,
    /// Test classification per requirements §2.3
    #[serde(default = "default_class")]
    pub class: String,
    /// CI pipeline stage: smoke, core, conformance, stress, differential
    #[serde(default = "default_stage")]
    pub stage: String,
}

fn default_tier() -> String {
    "P0".into()
}

fn default_class() -> String {
    "normative-posix".into()
}

fn default_stage() -> String {
    "core".into()
}

/// VFS and environment setup before script execution.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TestSetup {
    #[serde(default)]
    pub files: HashMap<String, String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

/// Script input to execute.
#[derive(Debug, Clone, Deserialize)]
pub struct TestInput {
    #[serde(default)]
    pub script: Option<String>,
    #[serde(default)]
    pub script_file: Option<String>,
}

/// Expected results — supports status, stdout, stderr, VFS state, env state.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TestExpect {
    pub status: Option<i32>,
    pub stdout: Option<String>,
    pub stdout_contains: Option<Vec<String>>,
    pub stdout_regex: Option<String>,
    pub stderr: Option<String>,
    pub stderr_contains: Option<Vec<String>>,
    /// Expected VFS file contents after execution.
    #[serde(default)]
    pub files: HashMap<String, String>,
    /// Expected environment variable values after execution.
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// Maximum allowed execution time in milliseconds (0 = no limit).
    #[serde(default)]
    pub max_time_ms: u64,
}

/// Oracle comparison configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct OracleConfig {
    #[serde(default)]
    pub compare: bool,
    #[serde(default)]
    pub shells: Vec<String>,
    #[serde(default)]
    pub ignore_stderr: bool,
}

/// Documented known divergence from reference shells.
#[derive(Debug, Clone, Deserialize)]
pub struct KnownDivergence {
    pub id: String,
    pub description: String,
    #[serde(default)]
    pub wasmsh_behavior: String,
    #[serde(default)]
    pub reference_behavior: String,
}
