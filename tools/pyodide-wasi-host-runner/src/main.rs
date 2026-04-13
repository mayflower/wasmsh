//! Wasmtime-based host runner for the standalone Pyodide+wasmsh artifact.
//!
//! Reads a scenario JSON document from stdin and emits a JSON result
//! document to stdout.
//!
//! The standalone artifact is a WASI P1 module (not a component), so this
//! runner uses `wasmtime::Module` + `wasmtime_wasi::preview1` instead of
//! the component model.

use std::io::Read;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use wasmtime::{AsContext, AsContextMut, Engine, Linker, Module, Store, TypedFunc};
use wasmtime_wasi::p1::{self, WasiP1Ctx};

/// Shared state for the host fetch allowlist, updated during Init step.
type AllowedHosts = Arc<Mutex<Vec<String>>>;
use wasmtime_wasi::WasiCtxBuilder;

// ── Scenario types ───────────────────────────────────────────

#[derive(Deserialize)]
struct Scenario {
    artifact: String,
    #[serde(rename = "workspaceDir")]
    workspace_dir: String,
    steps: Vec<Step>,
}

#[derive(Deserialize)]
#[serde(tag = "kind")]
enum Step {
    #[serde(rename = "boot")]
    Boot,
    #[serde(rename = "init")]
    Init {
        step_budget: u64,
        allowed_hosts: Vec<String>,
    },
    #[serde(rename = "host-command")]
    HostCommand { command: serde_json::Value },
}

// ── Result types ─────────────────────────────────────────────

#[derive(Serialize)]
struct RunResult {
    ok: bool,
    steps: Vec<StepResult>,
}

#[derive(Serialize)]
struct StepResult {
    kind: String,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    events: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stdout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stderr: Option<String>,
    #[serde(rename = "exitCode", skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
}

impl StepResult {
    fn ok(kind: &str) -> Self {
        Self {
            kind: kind.to_string(),
            ok: true,
            error: None,
            events: None,
            stdout: None,
            stderr: None,
            exit_code: None,
        }
    }

    fn err(kind: &str, msg: impl ToString) -> Self {
        Self {
            kind: kind.to_string(),
            ok: false,
            error: Some(msg.to_string()),
            events: None,
            stdout: None,
            stderr: None,
            exit_code: None,
        }
    }
}

// ── Guest ABI helpers ────────────────────────────────────────

struct GuestApi {
    malloc: TypedFunc<i32, i32>,
    free: TypedFunc<i32, ()>,
    boot: TypedFunc<(), i32>,
    runtime_new: TypedFunc<(), i32>,
    handle_json: TypedFunc<(i32, i32), i32>,
    runtime_free: TypedFunc<i32, ()>,
    free_string: TypedFunc<i32, ()>,
}

impl GuestApi {
    fn from_instance(
        store: &mut Store<WasiP1Ctx>,
        instance: &wasmtime::Instance,
    ) -> Result<Self, wasmtime::Error> {
        Ok(Self {
            malloc: instance.get_typed_func::<i32, i32>(&mut *store, "malloc")?,
            free: instance.get_typed_func::<i32, ()>(&mut *store, "free")?,
            boot: instance.get_typed_func::<(), i32>(&mut *store, "wasmsh_pyodide_boot")?,
            runtime_new: instance
                .get_typed_func::<(), i32>(&mut *store, "wasmsh_runtime_new")?,
            handle_json: instance
                .get_typed_func::<(i32, i32), i32>(&mut *store, "wasmsh_runtime_handle_json")?,
            runtime_free: instance
                .get_typed_func::<i32, ()>(&mut *store, "wasmsh_runtime_free")?,
            free_string: instance
                .get_typed_func::<i32, ()>(&mut *store, "wasmsh_runtime_free_string")?,
        })
    }

    fn write_cstring(
        &self,
        store: &mut Store<WasiP1Ctx>,
        instance: &wasmtime::Instance,
        s: &str,
    ) -> Result<i32, wasmtime::Error> {
        let bytes = s.as_bytes();
        let len = (bytes.len() + 1) as i32;
        let ptr = self.malloc.call(&mut *store, len)?;
        if ptr == 0 {
            return Err(wasmtime::Error::msg("malloc returned null"));
        }
        let memory = instance
            .get_memory(&mut *store, "memory")
            .ok_or_else(|| wasmtime::Error::msg("no memory export"))?;
        let mem_data = memory.data_mut(&mut *store);
        let offset = ptr as usize;
        mem_data[offset..offset + bytes.len()].copy_from_slice(bytes);
        mem_data[offset + bytes.len()] = 0;
        Ok(ptr)
    }

    fn read_cstring(
        &self,
        store: &mut Store<WasiP1Ctx>,
        instance: &wasmtime::Instance,
        ptr: i32,
    ) -> String {
        if ptr == 0 {
            return String::new();
        }
        let memory = match instance.get_memory(&mut store.as_context_mut(), "memory") {
            Some(m) => m,
            None => return String::new(),
        };
        let mem_data = memory.data(store.as_context());
        let offset = ptr as usize;
        let mut end = offset;
        while end < mem_data.len() && mem_data[end] != 0 {
            end += 1;
        }
        String::from_utf8_lossy(&mem_data[offset..end]).into_owned()
    }
}

// ── Main ─────────────────────────────────────────────────────

fn run() -> Result<RunResult, Box<dyn std::error::Error>> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let scenario: Scenario = serde_json::from_str(&input)?;

    let mut config = wasmtime::Config::default();
    config.wasm_exceptions(true);
    config.wasm_gc(true);
    // Enable GC for ref.test (count_args trampoline helper).
    config.wasm_gc(true);
    let engine = Engine::new(&config)?;
    let module = Module::from_file(&engine, &scenario.artifact)?;

    // Build WASI P1 context.
    let mut wasi_builder = WasiCtxBuilder::new();
    wasi_builder.inherit_stderr();
    // No WASI filesystem preopens. All file I/O is handled by the
    // in-memory filesystem (memfs.c) compiled into the wasm module.
    // WASI is only used for stdin/stdout/stderr (fd 0/1/2).
    // Preopens would allocate WASI fds 3+ that collide with memfs fds.
    wasi_builder.env("PYTHONHOME", "/");
    wasi_builder.env("PYTHONPATH", "/lib/python3.13");
    wasi_builder.env("PYTHONDONTWRITEBYTECODE", "1");
    wasi_builder.env("PYTHONNOUSERSITE", "1");
    wasi_builder.env("PYTHONSAFEPATH", "1");
    wasi_builder.env("PYTHONPLATLIBDIR", "lib");
    wasi_builder.args(&["/python3"]);
    let wasi_ctx = wasi_builder.build_p1();

    let mut store = Store::new(&engine, wasi_ctx);

    // Link WASI P1.
    let mut linker: Linker<WasiP1Ctx> = Linker::new(&engine);
    p1::add_to_linker_sync(&mut linker, |ctx| ctx)?;

    // Provide stub implementations for all `env` imports that the
    // standalone Emscripten artifact still carries. We iterate the
    // module's imports and define a no-op/zero-returning stub for
    // every unresolved `env` import. This covers:
    //   - Emscripten syscalls (__syscall_*)
    //   - Pyodide-specific functions (_Py*, _PyEM_*)
    //   - Compression libs (zlib/bz2 called from CPython modules)
    //   - Networking stubs (getaddrinfo, etc.)
    //   - Emscripten runtime functions
    let allowed_hosts: AllowedHosts = Arc::new(Mutex::new(Vec::new()));
    define_host_fetch(&mut linker, allowed_hosts.clone())?;
    define_env_stubs(&mut linker, &module, &engine)?;

    // Instantiate.
    let instance = linker.instantiate(&mut store, &module)?;

    // Call _initialize if present (reactor mode).
    if let Ok(init) = instance.get_typed_func::<(), ()>(&mut store, "_initialize") {
        if let Err(e) = init.call(&mut store, ()) {
            return Err(format!("_initialize failed: {e:#}").into());
        }
    }

    // Set up the count_args function for the CPython trampoline.
    setup_trampoline(&mut store, &instance, &engine)?;

    let api = GuestApi::from_instance(&mut store, &instance)?;

    let mut results = Vec::new();
    let mut runtime_handle: Option<i32> = None;
    let mut all_ok = true;

    for step in &scenario.steps {
        let result = match step {
            Step::Boot => match api.boot.call(&mut store, ()) {
                Ok(rc) => {
                    eprintln!("[runner] boot returned {rc}");
                    if rc == 0 {
                        StepResult::ok("boot")
                    } else {
                        all_ok = false;
                        StepResult::err("boot", format!("boot returned {rc}"))
                    }
                }
                Err(e) => {
                    all_ok = false;
                    StepResult::err("boot", format!("boot crashed: {e:#}"))
                }
            },
            Step::Init {
                step_budget,
                allowed_hosts: ref init_allowed_hosts,
            } => {
                // Update the host-level allowlist for __wasmsh_host_fetch.
                if let Ok(mut hosts) = allowed_hosts.lock() {
                    *hosts = init_allowed_hosts.clone();
                }
                let handle = api.runtime_new.call(&mut store, ())?;
                if handle == 0 {
                    all_ok = false;
                    StepResult::err("init", "wasmsh_runtime_new returned null")
                } else {
                    runtime_handle = Some(handle);
                    let init_cmd = serde_json::json!({
                        "Init": {
                            "step_budget": step_budget,
                            "allowed_hosts": init_allowed_hosts,
                        }
                    });
                    let cmd_str = serde_json::to_string(&init_cmd)?;
                    let cmd_ptr = api.write_cstring(&mut store, &instance, &cmd_str)?;
                    let result_ptr = api.handle_json.call(&mut store, (handle, cmd_ptr))?;
                    let response = api.read_cstring(&mut store, &instance, result_ptr);
                    api.free_string.call(&mut store, result_ptr)?;
                    api.free.call(&mut store, cmd_ptr)?;

                    let mut sr = StepResult::ok("init");
                    sr.events = serde_json::from_str(&response).ok();
                    sr
                }
            }
            Step::HostCommand { command } => {
                let handle = match runtime_handle {
                    Some(h) => h,
                    None => {
                        all_ok = false;
                        results.push(StepResult::err(
                            "host-command",
                            "no runtime handle (missing init step)",
                        ));
                        continue;
                    }
                };
                let cmd_str = serde_json::to_string(command)?;
                let cmd_ptr = api.write_cstring(&mut store, &instance, &cmd_str)?;
                let result_ptr = api.handle_json.call(&mut store, (handle, cmd_ptr))?;
                let response = api.read_cstring(&mut store, &instance, result_ptr);
                api.free_string.call(&mut store, result_ptr)?;
                api.free.call(&mut store, cmd_ptr)?;

                let events: serde_json::Value =
                    serde_json::from_str(&response).unwrap_or(serde_json::json!([]));
                let (stdout, stderr, exit_code) = extract_output(&events);

                let mut sr = StepResult::ok("host-command");
                sr.events = Some(events);
                sr.stdout = Some(stdout);
                sr.stderr = Some(stderr);
                sr.exit_code = Some(exit_code);
                sr
            }
        };
        results.push(result);
    }

    if let Some(handle) = runtime_handle {
        api.runtime_free.call(&mut store, handle)?;
    }

    Ok(RunResult {
        ok: all_ok,
        steps: results,
    })
}

/// Set up the CPython trampoline count_args function.
///
/// Creates a helper wasm module that uses ref.test to determine function
/// argument counts, instantiates it with the main module's function table,
/// and writes the result function pointer into _PyRuntime.
fn setup_trampoline(
    store: &mut Store<WasiP1Ctx>,
    instance: &wasmtime::Instance,
    engine: &Engine,
) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("[runner] setup_trampoline: starting");
    // Get the main module's function table and memory.
    let table = match instance.get_table(&mut *store, "__indirect_function_table") {
        Some(t) => t,
        None => return Ok(()), // no table → no trampoline needed
    };
    let memory = match instance.get_memory(&mut *store, "memory") {
        Some(m) => m,
        None => return Ok(()),
    };

    // Read _PyEM_EMSCRIPTEN_COUNT_ARGS_OFFSET from the global export.
    // The exported globals are memory ADDRESSES, not direct values.
    // _PyEM_EMSCRIPTEN_COUNT_ARGS_OFFSET → address of a const int
    //   that stores the byte offset of emscripten_count_args_function
    //   within _PyRuntimeState.
    // _PyRuntime → address of the _PyRuntimeState struct.
    let offset_addr = match instance.get_global(&mut *store, "_PyEM_EMSCRIPTEN_COUNT_ARGS_OFFSET") {
        Some(g) => match g.get(&mut *store) {
            wasmtime::Val::I32(v) => v as usize,
            _ => return Ok(()),
        },
        None => return Ok(()),
    };
    let pyruntime_addr = match instance.get_global(&mut *store, "_PyRuntime") {
        Some(g) => match g.get(&mut *store) {
            wasmtime::Val::I32(v) => v as usize,
            _ => return Ok(()),
        },
        None => return Ok(()),
    };

    // Read the actual offset value from memory.
    let mem_data = memory.data(&mut *store);
    if offset_addr + 4 > mem_data.len() {
        return Ok(());
    }
    let offset = i32::from_le_bytes([
        mem_data[offset_addr],
        mem_data[offset_addr + 1],
        mem_data[offset_addr + 2],
        mem_data[offset_addr + 3],
    ]) as usize;
    eprintln!("[runner] trampoline: _PyRuntime={pyruntime_addr}, offset_addr={offset_addr}, offset={offset}");

    // The count_args helper wasm module (from Pyodide's emscripten_trampoline.c).
    // It imports a funcref table and exports a function that uses ref.test
    // to determine how many parameters a function takes.
    // Helper wasm module: checks function types via ref.test.
    // Generated from count_args.wat by wasm-tools parse.
    // Returns 0-4 for i32-returning types, 10-13 for void-returning types,
    // or -1 for unknown.
    let count_args_wasm: &[u8] = include_bytes!("/tmp/count_args.wasm");

    // Try to compile the helper module. If ref.test isn't supported,
    // skip — CPython will fall back to the JS trampoline stub.
    let helper_module = match Module::new(engine, count_args_wasm) {
        Ok(m) => {
            eprintln!("[runner] trampoline: helper module compiled");
            m
        }
        Err(e) => {
            eprintln!("[runner] trampoline: helper module failed: {e:#}");
            return Ok(());
        }
    };

    // Instantiate with the main module's function table.
    let mut helper_linker = Linker::<WasiP1Ctx>::new(engine);
    if let Err(e) = helper_linker.define(&mut *store, "e", "t", table) {
        eprintln!("[runner] trampoline: define table failed: {e:#}");
        return Ok(());
    }
    let helper_instance = match helper_linker.instantiate(&mut *store, &helper_module) {
        Ok(i) => {
            eprintln!("[runner] trampoline: helper instantiated");
            i
        }
        Err(e) => {
            eprintln!("[runner] trampoline: helper instantiation failed: {e:#}");
            return Ok(());
        }
    };

    let count_args_func = match helper_instance.get_func(&mut *store, "f") {
        Some(f) => f,
        None => {
            eprintln!("[runner] trampoline: no 'f' export in helper");
            return Ok(());
        }
    };

    let table_size = table.size(&mut *store);
    match table.grow(&mut *store, 1, wasmtime::Ref::Func(None)) {
        Ok(old_size) => eprintln!("[runner] trampoline: table grown from {old_size}"),
        Err(e) => {
            eprintln!("[runner] trampoline: table grow failed: {e:#}");
            return Ok(());
        }
    }
    if let Err(e) = table.set(&mut *store, table_size as u64, wasmtime::Ref::Func(Some(count_args_func))) {
        eprintln!("[runner] trampoline: table set failed: {e:#}");
        return Ok(());
    }

    // Write the table index into _PyRuntime.emscripten_count_args_function.
    let func_ptr = table_size as i32;
    let target_offset = pyruntime_addr + offset;
    eprintln!("[runner] trampoline: _PyRuntime={pyruntime_addr}, offset={offset}, table_idx={func_ptr}, target={target_offset}");
    let data = memory.data_mut(&mut *store);
    if target_offset + 4 <= data.len() {
        data[target_offset..target_offset + 4].copy_from_slice(&func_ptr.to_le_bytes());
        eprintln!("[runner] trampoline: wrote count_args function ptr at {target_offset}");
    } else {
        eprintln!("[runner] trampoline: target offset {target_offset} out of bounds");
    }

    Ok(())
}

/// Write a JSON response string to guest memory via malloc.
fn write_response_to_guest(
    caller: &mut wasmtime::Caller<'_, WasiP1Ctx>,
    json: &str,
) -> i32 {
    let malloc = match caller.get_export("malloc") {
        Some(wasmtime::Extern::Func(f)) => f,
        _ => return 0,
    };
    let bytes = json.as_bytes();
    let len = (bytes.len() + 1) as i32;
    let mut results = [wasmtime::Val::I32(0)];
    if malloc.call(&mut *caller, &[wasmtime::Val::I32(len)], &mut results).is_err() {
        return 0;
    }
    let ptr = match results[0] { wasmtime::Val::I32(p) => p, _ => return 0 };
    if ptr == 0 { return 0; }
    let mem = match caller.get_export("memory").and_then(|e| e.into_memory()) {
        Some(m) => m,
        None => return 0,
    };
    let data = mem.data_mut(&mut *caller);
    let off = ptr as usize;
    if off + bytes.len() + 1 <= data.len() {
        data[off..off + bytes.len()].copy_from_slice(bytes);
        data[off + bytes.len()] = 0;
    }
    ptr
}

/// Implement `__wasmsh_host_fetch` — the HTTP fetch host import.
///
/// Reads URL, method, headers from guest memory, performs the HTTP request
/// on the host side using ureq, writes the JSON response back to guest
/// memory (via guest malloc), and returns the pointer.
fn define_host_fetch(
    linker: &mut Linker<WasiP1Ctx>,
    allowed_hosts: AllowedHosts,
) -> Result<(), Box<dyn std::error::Error>> {
    linker.func_wrap(
        "env",
        "__wasmsh_host_fetch",
        move |mut caller: wasmtime::Caller<'_, WasiP1Ctx>,
              url_ptr: i32,
              method_ptr: i32,
              _headers_ptr: i32,
              _body_ptr: i32,
              _body_len: i32,
              _follow_redirects: i32|
              -> i32 {
            // Read strings from guest memory.
            let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                Some(m) => m,
                None => return 0,
            };

            let read_cstr = |mem: &wasmtime::Memory, caller: &wasmtime::Caller<'_, WasiP1Ctx>, ptr: i32| -> String {
                let data = mem.data(caller);
                let start = ptr as usize;
                let mut end = start;
                while end < data.len() && data[end] != 0 {
                    end += 1;
                }
                String::from_utf8_lossy(&data[start..end]).into_owned()
            };

            let url = read_cstr(&memory, &caller, url_ptr);
            let method = read_cstr(&memory, &caller, method_ptr);

            eprintln!("[host-fetch] {method} {url}");

            // Check allowlist before making the request.
            if let Ok(hosts) = allowed_hosts.lock() {
                if let Ok(parsed) = url::Url::parse(&url) {
                    let host = parsed.host_str().unwrap_or("");
                    let host_port = if let Some(port) = parsed.port() {
                        format!("{host}:{port}")
                    } else {
                        host.to_string()
                    };
                    let allowed = hosts.iter().any(|h| {
                        h == &host_port || h == host ||
                        (h.starts_with("*.") && host.ends_with(&h[1..]))
                    });
                    if !allowed {
                        let err = format!(
                            r#"{{"error":"host '{}' not in allowed_hosts"}}"#,
                            host_port
                        );
                        return write_response_to_guest(&mut caller, &err);
                    }
                }
            }

            // Perform the HTTP request.
            let response_json = match method.as_str() {
                "GET" | "get" | "" => {
                    match ureq::get(&url).call() {
                        Ok(resp) => {
                            let status = resp.status().as_u16();
                            let body_bytes = resp.into_body().read_to_vec()
                                .unwrap_or_default();
                            let body_b64 = base64_encode(&body_bytes);
                            format!(
                                r#"{{"status":{},"body_base64":"{}"}}"#,
                                status, body_b64
                            )
                        }
                        Err(e) => {
                            format!(r#"{{"error":"{}"}}"#, e.to_string().replace('"', "'"))
                        }
                    }
                }
                _ => {
                    format!(r#"{{"error":"unsupported method: {}"}}"#, method)
                }
            };

            write_response_to_guest(&mut caller, &response_json)
        },
    )?;
    Ok(())
}

/// Minimal base64 encoder for HTTP response bodies.
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
        out.push(if chunk.len() > 1 { CHARS[((triple >> 6) & 0x3F) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { CHARS[(triple & 0x3F) as usize] as char } else { '=' });
    }
    out
}

/// Define stubs for all unresolved `env` module imports.
///
/// For syscalls, we return -ENOSYS (not implemented) instead of 0.
/// For emscripten/Pyodide functions, we return 0 (no-op).
fn define_env_stubs(
    linker: &mut Linker<WasiP1Ctx>,
    module: &Module,
    _engine: &Engine,
) -> Result<(), Box<dyn std::error::Error>> {
    use wasmtime::{Val, ValType};

    // errno values (negative for syscall convention)
    const ENOSYS: i32 = -38;

    for import in module.imports() {
        if import.module() != "env" {
            continue;
        }
        if let wasmtime::ExternType::Func(func_ty) = import.ty() {
            let results: Vec<ValType> = func_ty.results().collect();
            let name = import.name().to_string();
            let is_syscall = name.starts_with("__syscall_");

            let name_clone = name.clone();
            if linker
                .func_new("env", &name, func_ty, move |mut caller, params, out| {
                    // Special case: __syscall_getcwd needs to write "/" to the buffer.
                    if name_clone == "__syscall_getcwd" && params.len() >= 2 {
                        if let (Some(buf), Some(size)) =
                            (params[0].i32(), params[1].i32())
                        {
                            if size > 1 {
                                let mem = caller
                                    .get_export("memory")
                                    .and_then(|e| e.into_memory());
                                if let Some(mem) = mem {
                                    let data = mem.data_mut(&mut caller);
                                    let offset = buf as usize;
                                    if offset + 2 <= data.len() {
                                        data[offset] = b'/';
                                        data[offset + 1] = 0;
                                    }
                                }
                                if !results.is_empty() {
                                    out[0] = Val::I32(0); // success
                                }
                                return Ok(());
                            }
                        }
                    }

                    for (i, ty) in results.iter().enumerate() {
                        let val = if is_syscall {
                            ENOSYS // -ENOSYS for unimplemented syscalls
                        } else {
                            0
                        };
                        out[i] = match ty {
                            ValType::I32 => Val::I32(val),
                            ValType::I64 => Val::I64(val as i64),
                            ValType::F32 => Val::F32(0),
                            ValType::F64 => Val::F64(0),
                            _ => Val::I32(0),
                        };
                    }
                    Ok(())
                })
                .is_err()
            {
                // Already defined — skip.
            }
        }
    }
    Ok(())
}

fn extract_output(events: &serde_json::Value) -> (String, String, i32) {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut exit_code = 0;

    if let Some(arr) = events.as_array() {
        for event in arr {
            if let Some(bytes) = event.get("Stdout").and_then(|v| v.as_array()) {
                for b in bytes {
                    if let Some(n) = b.as_u64() {
                        stdout.push(n as u8);
                    }
                }
            }
            if let Some(bytes) = event.get("Stderr").and_then(|v| v.as_array()) {
                for b in bytes {
                    if let Some(n) = b.as_u64() {
                        stderr.push(n as u8);
                    }
                }
            }
            if let Some(code) = event.get("Exit").and_then(|v| v.as_i64()) {
                exit_code = code as i32;
            }
        }
    }

    (
        String::from_utf8_lossy(&stdout).into_owned(),
        String::from_utf8_lossy(&stderr).into_owned(),
        exit_code,
    )
}

fn main() -> ExitCode {
    match run() {
        Ok(result) => {
            let json = serde_json::to_string(&result).unwrap_or_else(|e| {
                format!(r#"{{"ok":false,"error":"serialize error: {e}","steps":[]}}"#)
            });
            println!("{json}");
            if result.ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(e) => {
            let json = serde_json::json!({
                "ok": false,
                "error": format!("{e:#}"),
                "steps": []
            });
            println!("{}", serde_json::to_string(&json).unwrap());
            ExitCode::FAILURE
        }
    }
}
