//! Browser-safe standard utility commands for wasmsh.
//!
//! Utilities operate purely on the VFS and stream abstractions —
//! no `std::fs` or OS process calls. They are resolved separately
//! from shell builtins.

use indexmap::IndexMap;
use wasmsh_fs::MemoryFs;
use wasmsh_state::ShellState;

mod data_ops;
mod file_ops;
mod helpers;
mod system_ops;
mod text_ops;

/// Output sink for utility commands (same interface as builtins).
pub trait UtilOutput {
    fn stdout(&mut self, data: &[u8]);
    fn stderr(&mut self, data: &[u8]);
}

/// Collected output for testing.
#[derive(Debug, Default)]
pub struct VecOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl UtilOutput for VecOutput {
    fn stdout(&mut self, data: &[u8]) {
        self.stdout.extend_from_slice(data);
    }
    fn stderr(&mut self, data: &[u8]) {
        self.stderr.extend_from_slice(data);
    }
}

impl VecOutput {
    #[must_use]
    pub fn stdout_str(&self) -> &str {
        std::str::from_utf8(&self.stdout).unwrap_or("<invalid utf-8>")
    }
}

/// Context passed to utility implementations.
pub struct UtilContext<'a> {
    pub fs: &'a mut MemoryFs,
    pub output: &'a mut dyn UtilOutput,
    pub cwd: &'a str,
    /// Stdin data from pipe or here-doc. `None` if not connected.
    pub stdin: Option<&'a [u8]>,
    /// Optional shell state access (for env/printenv/date).
    pub state: Option<&'a ShellState>,
}

impl std::fmt::Debug for UtilContext<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UtilContext")
            .field("cwd", &self.cwd)
            .finish_non_exhaustive()
    }
}

/// Signature for a utility command function.
pub type UtilFn = fn(&mut UtilContext<'_>, &[&str]) -> i32;

/// Registry of utility commands.
pub struct UtilRegistry {
    utils: IndexMap<&'static str, UtilFn>,
}

impl std::fmt::Debug for UtilRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UtilRegistry")
            .field("count", &self.utils.len())
            .finish()
    }
}

impl UtilRegistry {
    #[must_use]
    pub fn new() -> Self {
        let mut utils = IndexMap::<&'static str, UtilFn>::new();
        // File utilities
        utils.insert("cat", file_ops::util_cat);
        utils.insert("ls", file_ops::util_ls);
        utils.insert("mkdir", file_ops::util_mkdir);
        utils.insert("rm", file_ops::util_rm);
        utils.insert("touch", file_ops::util_touch);
        utils.insert("mv", file_ops::util_mv);
        utils.insert("cp", file_ops::util_cp);
        utils.insert("ln", file_ops::util_ln);
        utils.insert("readlink", file_ops::util_readlink);
        utils.insert("realpath", file_ops::util_realpath);
        utils.insert("stat", file_ops::util_stat);
        utils.insert("find", file_ops::util_find);
        utils.insert("chmod", file_ops::util_chmod);
        // Text utilities
        utils.insert("head", text_ops::util_head);
        utils.insert("tail", text_ops::util_tail);
        utils.insert("wc", text_ops::util_wc);
        utils.insert("grep", text_ops::util_grep);
        utils.insert("sed", text_ops::util_sed);
        utils.insert("sort", text_ops::util_sort);
        utils.insert("uniq", text_ops::util_uniq);
        utils.insert("cut", text_ops::util_cut);
        utils.insert("tr", text_ops::util_tr);
        utils.insert("tee", text_ops::util_tee);
        // Data/string utilities
        utils.insert("seq", data_ops::util_seq);
        utils.insert("basename", data_ops::util_basename);
        utils.insert("dirname", data_ops::util_dirname);
        utils.insert("expr", data_ops::util_expr);
        utils.insert("xargs", data_ops::util_xargs);
        // System/env utilities
        utils.insert("env", system_ops::util_env);
        utils.insert("printenv", system_ops::util_printenv);
        utils.insert("id", system_ops::util_id);
        utils.insert("whoami", system_ops::util_whoami);
        utils.insert("uname", system_ops::util_uname);
        utils.insert("hostname", system_ops::util_hostname);
        utils.insert("sleep", system_ops::util_sleep);
        utils.insert("date", system_ops::util_date);
        Self { utils }
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<UtilFn> {
        self.utils.get(name).copied()
    }

    #[must_use]
    pub fn is_utility(&self, name: &str) -> bool {
        self.utils.contains_key(name)
    }
}

impl Default for UtilRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasmsh_fs::{OpenOptions, Vfs};

    fn make_fs_with_file(path: &str, content: &[u8]) -> MemoryFs {
        let mut fs = MemoryFs::new();
        let h = fs.open(path, OpenOptions::write()).unwrap();
        fs.write_file(h, content).unwrap();
        fs.close(h);
        fs
    }

    fn run_util(name: &str, argv: &[&str], fs: &mut MemoryFs) -> (i32, VecOutput) {
        let registry = UtilRegistry::new();
        let mut output = VecOutput::default();
        let util = registry.get(name).unwrap();
        let status = {
            let mut ctx = UtilContext {
                fs,
                output: &mut output,
                cwd: "/",
                stdin: None,
                state: None,
            };
            util(&mut ctx, argv)
        };
        (status, output)
    }

    #[test]
    fn cat_file() {
        let mut fs = make_fs_with_file("/hello.txt", b"hello world");
        let (status, out) = run_util("cat", &["cat", "/hello.txt"], &mut fs);
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str(), "hello world");
    }

    #[test]
    fn cat_missing_file() {
        let mut fs = MemoryFs::new();
        let (status, _) = run_util("cat", &["cat", "/nope.txt"], &mut fs);
        assert_eq!(status, 1);
    }

    #[test]
    fn ls_root() {
        let mut fs = make_fs_with_file("/a.txt", b"");
        fs.create_dir("/mydir").unwrap();
        let (status, out) = run_util("ls", &["ls", "/"], &mut fs);
        assert_eq!(status, 0);
        assert!(out.stdout_str().contains("a.txt"));
        assert!(out.stdout_str().contains("mydir"));
    }

    #[test]
    fn mkdir_and_ls() {
        let mut fs = MemoryFs::new();
        run_util("mkdir", &["mkdir", "/newdir"], &mut fs);
        assert!(fs.stat("/newdir").unwrap().is_dir);
    }

    #[test]
    fn touch_creates_file() {
        let mut fs = MemoryFs::new();
        run_util("touch", &["touch", "/new.txt"], &mut fs);
        assert!(!fs.stat("/new.txt").unwrap().is_dir);
    }

    #[test]
    fn rm_file() {
        let mut fs = make_fs_with_file("/del.txt", b"data");
        let (status, _) = run_util("rm", &["rm", "/del.txt"], &mut fs);
        assert_eq!(status, 0);
        assert!(fs.stat("/del.txt").is_err());
    }

    #[test]
    fn head_first_lines() {
        let mut fs = make_fs_with_file("/lines.txt", b"a\nb\nc\nd\ne");
        let (status, out) = run_util("head", &["head", "-n", "3", "/lines.txt"], &mut fs);
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str(), "a\nb\nc\n");
    }

    #[test]
    fn tail_last_lines() {
        let mut fs = make_fs_with_file("/lines.txt", b"a\nb\nc\nd\ne");
        let (status, out) = run_util("tail", &["tail", "-n", "2", "/lines.txt"], &mut fs);
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str(), "d\ne\n");
    }

    #[test]
    fn wc_counts() {
        let mut fs = make_fs_with_file("/test.txt", b"hello world\nfoo bar\n");
        let (status, out) = run_util("wc", &["wc", "/test.txt"], &mut fs);
        assert_eq!(status, 0);
        // 2 lines, 4 words, 20 bytes
        assert!(out.stdout_str().contains('2'));
        assert!(out.stdout_str().contains('4'));
    }

    #[test]
    fn registry_lookup() {
        let reg = UtilRegistry::new();
        assert!(reg.is_utility("cat"));
        assert!(reg.is_utility("wc"));
        assert!(!reg.is_utility("echo")); // echo is a builtin, not a utility
    }
}
