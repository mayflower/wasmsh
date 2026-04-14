//! Wasmtime-based host runner for the wasmsh WASI P2 component artifact.
//!
//! Usage:
//!   component-host-runner <wasm-path> <workspace-dir> <json-command> [json-command...]
//!
//! Loads the component, configures WASI P2 with `/workspace` preopened to the
//! given host directory, constructs a `runtime.handle` resource, and sends each
//! JSON command through `handle-json`. Each response is printed on its own line
//! so the calling test harness can parse them.

use std::env;
use std::process::ExitCode;

use wasmtime::component::{Component, Linker, ResourceTable};
use wasmtime::{Config, Engine, OptLevel, Store};
use wasmtime_wasi::{DirPerms, FilePerms, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

// Generate host-side bindings from the component WIT.
wasmtime::component::bindgen!({
    world: "wasmsh",
    path: "../../crates/wasmsh-component/wit",
});

const PYTHON_STDLIB_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tools/python-wasi/output/lib"
);
const PYTHON_STDLIB_ROOT_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tools/python-wasi/output/lib/python3.13"
);

/// Host state visible to WASI and the component.
struct HostState {
    wasi: WasiCtx,
    table: ResourceTable,
}

impl WasiView for HostState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

fn run() -> Result<(), wasmtime::Error> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 4 {
        eprintln!(
            "Usage: {} <wasm-path> <workspace-dir> <json-command> [json-command...]",
            args[0]
        );
        std::process::exit(1);
    }

    let wasm_path = &args[1];
    let workspace_dir = &args[2];
    let json_commands = &args[3..];

    // Configure wasmtime with component model support.
    let mut config = Config::new();
    config.wasm_component_model(true);
    config.cranelift_opt_level(OptLevel::None);
    let engine = Engine::new(&config)?;

    // Load the component.
    let component = Component::from_file(&engine, wasm_path)?;

    // Build the WASI context with /workspace preopened.
    let mut wasi_builder = WasiCtxBuilder::new();
    wasi_builder.inherit_stderr();
    wasi_builder.env("PYTHONHOME", "/");
    wasi_builder.env("PYTHONPATH", "/Lib:/lib/python3.13");
    wasi_builder.env("PYTHONDONTWRITEBYTECODE", "1");
    wasi_builder.env("PYTHONNOUSERSITE", "1");
    wasi_builder.env("PYTHONSAFEPATH", "1");
    wasi_builder.env("PYTHONPLATLIBDIR", "lib");
    wasi_builder.arg("/python3");
    let wasi = wasi_builder
        .inherit_stderr()
        .preopened_dir(
            workspace_dir,
            "/workspace",
            DirPerms::all(),
            FilePerms::all(),
        )?
        .preopened_dir(
            PYTHON_STDLIB_DIR,
            "/lib",
            DirPerms::all(),
            FilePerms::all(),
        )?
        .preopened_dir(
            PYTHON_STDLIB_ROOT_DIR,
            "/Lib",
            DirPerms::all(),
            FilePerms::all(),
        )?
        .build();

    let mut store = Store::new(
        &engine,
        HostState {
            wasi,
            table: ResourceTable::new(),
        },
    );

    // Link WASI interfaces into the component.
    let mut linker = Linker::new(&engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker)?;

    // Instantiate the component.
    let instance = Wasmsh::instantiate(&mut store, &component, &linker)?;

    // Access the exported runtime interface.
    let runtime = instance.wasmsh_component_runtime();

    // Construct the handle resource.
    let handle = runtime.handle().call_constructor(&mut store)?;

    // Send each JSON command and print the response.
    for cmd in json_commands {
        let response = runtime
            .handle()
            .call_handle_json(&mut store, handle, cmd)?;
        println!("{response}");
    }

    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}
