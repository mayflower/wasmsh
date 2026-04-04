//! In-process python/python3 command handler for the Pyodide runtime.
//!
//! Delegates to CPython's `PyRun_SimpleString` via extern "C". Stdout and
//! stderr are captured by temporarily redirecting `sys.stdout`/`sys.stderr`
//! to `io.StringIO` objects, then read back via temp files.

use std::ffi::CString;
use std::sync::atomic::{AtomicU64, Ordering};
use wasmsh_runtime::ExternalCommandResult;

extern "C" {
    fn PyRun_SimpleString(command: *const std::os::raw::c_char) -> std::os::raw::c_int;
}

/// Monotonic counter for unique temp file names (avoids collisions between
/// concurrent or sequential python invocations).
static INVOCATION_ID: AtomicU64 = AtomicU64::new(0);

/// RAII guard that closes a libc `FILE*` on drop.
struct FileGuard(*mut libc::FILE);

impl Drop for FileGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { libc::fclose(self.0) };
        }
    }
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
    let id = INVOCATION_ID.fetch_add(1, Ordering::Relaxed);
    let stdout_path = format!("/tmp/_wasmsh_py_stdout_{id}");
    let stderr_path = format!("/tmp/_wasmsh_py_stderr_{id}");
    let exit_path = format!("/tmp/_wasmsh_py_exit_{id}");

    // Encode user code as base64 and decode in Python. This avoids any
    // string-escaping issues (triple-quotes, backslashes, null bytes).
    let code_b64 = base64_encode(code.as_bytes());

    let wrapped = format!(
        concat!(
            "import sys, io as _wasmsh_io, base64 as _wasmsh_b64\n",
            "_wasmsh_old_stdout = sys.stdout\n",
            "_wasmsh_old_stderr = sys.stderr\n",
            "sys.stdout = _wasmsh_io.StringIO()\n",
            "sys.stderr = _wasmsh_io.StringIO()\n",
            "_wasmsh_exit_code = 0\n",
            "try:\n",
            "    _wasmsh_code = compile(_wasmsh_b64.b64decode(\"{code_b64}\").decode(), \"<string>\", \"exec\")\n",
            "    exec(_wasmsh_code)\n",
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
            "with open(\"{stdout_path}\", \"w\") as _f:\n",
            "    _f.write(_wasmsh_stdout_val)\n",
            "with open(\"{stderr_path}\", \"w\") as _f:\n",
            "    _f.write(_wasmsh_stderr_val)\n",
            "with open(\"{exit_path}\", \"w\") as _f:\n",
            "    _f.write(str(_wasmsh_exit_code))\n",
        ),
        code_b64 = code_b64,
        stdout_path = stdout_path,
        stderr_path = stderr_path,
        exit_path = exit_path,
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

    let stdout_bytes = read_temp_file(&stdout_path);
    let stderr_bytes = read_temp_file(&stderr_path);
    let exit_str = read_temp_file(&exit_path);
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

/// Extract code to run from argv (`-c CODE`, script file, or stdin).
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
        // Skip known flags
        if argv[i].starts_with('-') {
            i += 1;
            continue;
        }
        // Non-flag argument is a script file path — read it via libc
        return Some(read_script_file(&argv[i]));
    }

    if let Some(data) = stdin {
        if !data.is_empty() {
            return Some(String::from_utf8_lossy(data).into_owned());
        }
    }

    // No code — empty success (interactive mode not supported).
    Some(String::new())
}

/// Read a script file from the Emscripten filesystem via libc.
fn read_script_file(path: &str) -> String {
    let c_path = match CString::new(path) {
        Ok(c) => c,
        Err(_) => return String::new(),
    };
    let fp = unsafe { libc::fopen(c_path.as_ptr(), c"r".as_ptr()) };
    if fp.is_null() {
        return String::new();
    }
    let guard = FileGuard(fp);
    let data = read_fp_to_vec(guard.0);
    String::from_utf8_lossy(&data).into_owned()
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
    let guard = FileGuard(fp);
    let data = read_fp_to_vec(guard.0);
    drop(guard); // close before unlink
    unsafe { libc::unlink(c_path.as_ptr()) };
    data
}

/// Read all bytes from a libc FILE* in a loop.
fn read_fp_to_vec(fp: *mut libc::FILE) -> Vec<u8> {
    let mut result = Vec::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = unsafe { libc::fread(buf.as_mut_ptr().cast(), 1, buf.len(), fp) };
        if n == 0 {
            break;
        }
        result.extend_from_slice(&buf[..n]);
    }
    result
}

/// Minimal base64 encoder (no external dependency needed in this crate).
fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
        out.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(CHARS[(triple & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}
