//! Browser-safe standard utility commands for wasmsh.
//!
//! Utilities operate purely on the VFS and stream abstractions —
//! no `std::fs` or OS process calls. They are resolved separately
//! from shell builtins.

use std::io::{Cursor, Read};

use indexmap::IndexMap;
use wasmsh_fs::BackendFs;
use wasmsh_state::ShellState;

mod net_multipart;
mod net_ops;
pub mod net_types;

mod archive_ops;
mod awk_ops;
mod binary_ops;
mod data_ops;
mod diff_ops;
mod disk_ops;
mod file_ops;
mod hash_ops;
mod helpers;
mod jaq_runner;
mod jq_ops;
mod math_ops;
mod regex_posix;
mod search_ops;
mod system_ops;
mod text_ops;
mod tree_ops;
mod trivial_ops;
mod yaml_ops;

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

pub struct UtilStdin<'a> {
    reader: Box<dyn Read + 'a>,
}

impl std::fmt::Debug for UtilStdin<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UtilStdin").finish_non_exhaustive()
    }
}

impl<'a> UtilStdin<'a> {
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

impl Read for UtilStdin<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.read_chunk(buf)
    }
}

/// Context passed to utility implementations.
pub struct UtilContext<'a> {
    pub fs: &'a mut BackendFs,
    pub output: &'a mut dyn UtilOutput,
    pub cwd: &'a str,
    /// Stdin source from pipe or here-doc. `None` if not connected.
    pub stdin: Option<UtilStdin<'a>>,
    /// Optional shell state access (for env/printenv/date).
    pub state: Option<&'a ShellState>,
    /// Optional network backend for curl/wget (None = no network access).
    pub network: Option<&'a dyn net_types::NetworkBackend>,
}

impl std::fmt::Debug for UtilContext<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UtilContext")
            .field("cwd", &self.cwd)
            .finish_non_exhaustive()
    }
}

impl UtilContext<'_> {
    #[must_use]
    pub fn has_stdin(&self) -> bool {
        self.stdin.is_some()
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

        // --- File utilities ---
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
        utils.insert("mktemp", file_ops::util_mktemp);

        // --- Text utilities ---
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
        utils.insert("paste", text_ops::util_paste);
        utils.insert("rev", text_ops::util_rev);
        utils.insert("column", text_ops::util_column);

        // --- Data/string utilities ---
        utils.insert("seq", data_ops::util_seq);
        utils.insert("basename", data_ops::util_basename);
        utils.insert("dirname", data_ops::util_dirname);
        utils.insert("expr", data_ops::util_expr);
        utils.insert("xargs", data_ops::util_xargs);
        utils.insert("yes", data_ops::util_yes);
        utils.insert("md5sum", data_ops::util_md5sum);
        utils.insert("sha256sum", data_ops::util_sha256sum);
        utils.insert("base64", data_ops::util_base64);

        // --- System/env utilities ---
        utils.insert("env", system_ops::util_env);
        utils.insert("printenv", system_ops::util_printenv);
        utils.insert("id", system_ops::util_id);
        utils.insert("whoami", system_ops::util_whoami);
        utils.insert("uname", system_ops::util_uname);
        utils.insert("hostname", system_ops::util_hostname);
        utils.insert("sleep", system_ops::util_sleep);
        utils.insert("date", system_ops::util_date);

        // --- Trivial utilities (P0+P2) ---
        utils.insert("which", trivial_ops::util_which);
        utils.insert("rmdir", trivial_ops::util_rmdir);
        utils.insert("tac", trivial_ops::util_tac);
        utils.insert("nl", trivial_ops::util_nl);
        utils.insert("shuf", trivial_ops::util_shuf);
        utils.insert("cmp", trivial_ops::util_cmp);
        utils.insert("comm", trivial_ops::util_comm);
        utils.insert("fold", trivial_ops::util_fold);
        utils.insert("nproc", trivial_ops::util_nproc);
        utils.insert("expand", trivial_ops::util_expand);
        utils.insert("unexpand", trivial_ops::util_unexpand);
        utils.insert("truncate", trivial_ops::util_truncate);
        utils.insert("factor", trivial_ops::util_factor);
        utils.insert("cksum", trivial_ops::util_cksum);
        utils.insert("tsort", trivial_ops::util_tsort);
        utils.insert("install", trivial_ops::util_install);
        utils.insert("timeout", trivial_ops::util_timeout);
        utils.insert("cal", trivial_ops::util_cal);

        // --- Diff/patch ---
        utils.insert("diff", diff_ops::util_diff);
        utils.insert("patch", diff_ops::util_patch);

        // --- Tree ---
        utils.insert("tree", tree_ops::util_tree);

        // --- Search (ripgrep-like) ---
        utils.insert("rg", search_ops::util_rg);

        // --- Awk ---
        utils.insert("awk", awk_ops::util_awk);

        // --- jq (JSON processor) ---
        utils.insert("jq", jq_ops::util_jq);

        // --- Hash utilities ---
        utils.insert("sha1sum", hash_ops::util_sha1sum);
        utils.insert("sha512sum", hash_ops::util_sha512sum);

        // --- Binary utilities ---
        utils.insert("xxd", binary_ops::util_xxd);
        utils.insert("dd", binary_ops::util_dd);
        utils.insert("strings", binary_ops::util_strings);
        utils.insert("split", binary_ops::util_split);

        // --- Math ---
        utils.insert("bc", math_ops::util_bc);

        // --- Archive/compression ---
        utils.insert("tar", archive_ops::util_tar);
        utils.insert("gzip", archive_ops::util_gzip);
        utils.insert("gunzip", archive_ops::util_gunzip);
        utils.insert("zcat", archive_ops::util_zcat);

        // --- Disk usage ---
        utils.insert("du", disk_ops::util_du);
        utils.insert("df", disk_ops::util_df);

        // --- Additional utilities ---
        utils.insert("fd", search_ops::util_fd);
        utils.insert("file", binary_ops::util_file);
        utils.insert("bat", text_ops::util_bat);
        utils.insert("unzip", archive_ops::util_unzip);
        utils.insert("yq", yaml_ops::util_yq);

        // --- Network utilities ---
        utils.insert("curl", net_ops::util_curl);
        utils.insert("wget", net_ops::util_wget);

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
    use wasmsh_fs::{MemoryFs, OpenOptions, Vfs};

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
                network: None,
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
        assert!(out.stdout_str().contains('2'));
        assert!(out.stdout_str().contains('4'));
    }

    #[test]
    fn registry_lookup() {
        let reg = UtilRegistry::new();
        assert!(reg.is_utility("cat"));
        assert!(reg.is_utility("wc"));
        assert!(reg.is_utility("jq"));
        assert!(reg.is_utility("awk"));
        assert!(reg.is_utility("diff"));
        assert!(reg.is_utility("tree"));
        assert!(reg.is_utility("rg"));
        assert!(!reg.is_utility("echo")); // echo is a builtin, not a utility
    }
}
