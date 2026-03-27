//! In-process python/python3 command handler for the Pyodide runtime.
//!
//! Delegates to CPython's `PyRun_SimpleString` via extern "C". Stdout and
//! stderr are captured by temporarily redirecting `sys.stdout`/`sys.stderr`
//! to `io.StringIO` objects, then read back via temp files.

use std::ffi::CString;
use wasmsh_runtime::ExternalCommandResult;

extern "C" {
    fn PyRun_SimpleString(command: *const std::os::raw::c_char) -> std::os::raw::c_int;
}

/// Handle python/python3 commands. Returns `None` for non-python commands.
pub fn handle_python_command(
    cmd_name: &str,
    argv: &[String],
    stdin: Option<&[u8]>,
) -> Option<ExternalCommandResult> {
    if cmd_name != "python" && cmd_name != "python3" {
        return None;
    }

    let code = extract_code(argv, stdin)?;

    // Wrap user code: capture stdout/stderr to StringIO, write to temp files.
    let wrapped = format!(
        concat!(
            "import sys, io as _wasmsh_io\n",
            "_wasmsh_old_stdout = sys.stdout\n",
            "_wasmsh_old_stderr = sys.stderr\n",
            "sys.stdout = _wasmsh_io.StringIO()\n",
            "sys.stderr = _wasmsh_io.StringIO()\n",
            "_wasmsh_exit_code = 0\n",
            "try:\n",
            "    _wasmsh_code = compile({code_repr}, \"<string>\", \"exec\")\n",
            "    exec(_wasmsh_code)\n",   // noqa: safe — runs user-provided shell input
            "except SystemExit as _e:\n",
            "    _wasmsh_exit_code = _e.code if isinstance(_e.code, int) else 1\n",
            "except BaseException:\n",
            "    import traceback\n",
            "    traceback.print_exc()\n",
            "    _wasmsh_exit_code = 1\n",
            "_wasmsh_stdout_val = sys.stdout.getvalue()\n",
            "_wasmsh_stderr_val = sys.stderr.getvalue()\n",
            "sys.stdout = _wasmsh_old_stdout\n",
            "sys.stderr = _wasmsh_old_stderr\n",
            "with open(\"/tmp/_wasmsh_py_stdout\", \"w\") as _f:\n",
            "    _f.write(_wasmsh_stdout_val)\n",
            "with open(\"/tmp/_wasmsh_py_stderr\", \"w\") as _f:\n",
            "    _f.write(_wasmsh_stderr_val)\n",
            "with open(\"/tmp/_wasmsh_py_exit\", \"w\") as _f:\n",
            "    _f.write(str(_wasmsh_exit_code))\n",
        ),
        code_repr = python_repr(&code),
    );

    let c_wrapped = match CString::new(wrapped) {
        Ok(c) => c,
        Err(_) => {
            return Some(ExternalCommandResult {
                stdout: Vec::new(),
                stderr: b"wasmsh: python: internal error (null in code)\n".to_vec(),
                status: 1,
            });
        }
    };

    let rc = unsafe { PyRun_SimpleString(c_wrapped.as_ptr()) };

    let stdout_bytes = read_temp_file("/tmp/_wasmsh_py_stdout");
    let stderr_bytes = read_temp_file("/tmp/_wasmsh_py_stderr");
    let exit_str = read_temp_file("/tmp/_wasmsh_py_exit");
    let status = if rc != 0 {
        1
    } else {
        std::str::from_utf8(&exit_str)
            .ok()
            .and_then(|s| s.trim().parse::<i32>().ok())
            .unwrap_or(0)
    };

    Some(ExternalCommandResult {
        stdout: stdout_bytes,
        stderr: stderr_bytes,
        status,
    })
}

/// Extract code to run from argv (`-c CODE`) or stdin.
fn extract_code(argv: &[String], stdin: Option<&[u8]>) -> Option<String> {
    let mut i = 1;
    while i < argv.len() {
        if argv[i] == "-c" {
            return if i + 1 < argv.len() {
                Some(argv[i + 1].clone())
            } else {
                Some(String::new())
            };
        }
        i += 1;
    }

    if let Some(data) = stdin {
        if !data.is_empty() {
            return Some(String::from_utf8_lossy(data).into_owned());
        }
    }

    // No code — empty success (interactive mode not supported).
    Some(String::new())
}

/// Escape a string as a Python triple-quoted string literal.
fn python_repr(s: &str) -> String {
    let escaped = s
        .replace('\\', "\\\\")
        .replace("\"\"\"", "\\\"\\\"\\\"");
    format!("\"\"\"{}\"\"\"", escaped)
}

/// Read a temp file via libc and clean it up.
fn read_temp_file(path: &str) -> Vec<u8> {
    let c_path = match CString::new(path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let fp = unsafe { libc::fopen(c_path.as_ptr(), c"r".as_ptr()) };
    if fp.is_null() {
        return Vec::new();
    }
    let mut buf = vec![0u8; 65536];
    let n = unsafe { libc::fread(buf.as_mut_ptr().cast(), 1, buf.len(), fp) };
    unsafe { libc::fclose(fp) };
    unsafe { libc::unlink(c_path.as_ptr()) };
    buf.truncate(n);
    buf
}
