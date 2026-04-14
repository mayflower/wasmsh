use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    const CPYTHON_STACK_SIZE_BYTES: u32 = 8 * 1024 * 1024;
    const CPYTHON_INITIAL_MEMORY_BYTES: u32 = 20 * 1024 * 1024;

    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_arch != "wasm32" || target_os != "wasi" {
        return;
    }

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let repo_root = manifest_dir
        .parent()
        .and_then(|path| path.parent())
        .expect("crate lives under repo root");

    let python_wasi_output = repo_root.join("tools/python-wasi/output");
    let python_lib = python_wasi_output.join("libpython3.13.a");
    let python_include_dir = python_wasi_output.join("include/python3.13");
    let python_internal_include_dir = python_include_dir.join("internal");
    let python_cpython_include_dir = python_include_dir.join("cpython");
    println!("cargo:rerun-if-changed={}", python_lib.display());
    if !python_lib.is_file() {
        panic!(
            "missing WASI CPython archive at {}. Run `bash tools/python-wasi/build.sh` first.",
            python_lib.display()
        );
    }
    let mpdec_lib = python_wasi_output.join("libmpdec.a");
    assert_archive_exists(&mpdec_lib);
    println!("cargo:rerun-if-changed={}", mpdec_lib.display());
    let hacl_sha2_lib = python_wasi_output.join("libHacl_Hash_SHA2.a");
    assert_archive_exists(&hacl_sha2_lib);
    println!("cargo:rerun-if-changed={}", hacl_sha2_lib.display());
    let expat_lib = python_wasi_output.join("libexpat.a");
    assert_archive_exists(&expat_lib);
    println!("cargo:rerun-if-changed={}", expat_lib.display());
    let wasi_sdk_dir = repo_root.join("tools/python-wasi/wasi-sdk");
    let wasi_clang = wasi_sdk_dir.join("bin/clang");
    if !wasi_clang.is_file() {
        panic!(
            "missing wasi-sdk clang at {}. Run `bash tools/python-wasi/build.sh` first.",
            wasi_clang.display()
        );
    }
    let wasi_sysroot = wasi_sdk_dir.join("share/wasi-sysroot");
    let cpython_src = repo_root.join("tools/python-wasi/build/cpython-src");
    let cpython_source_internal_include_dir = cpython_src.join("Include/internal");
    let sqlite_module_dir = cpython_src.join("Modules/_sqlite");
    let sqlite_amalgamation_dir = repo_root.join(
        "tools/pyodide/pyodide-src/emsdk/emsdk/upstream/emscripten/cache/ports/sqlite3/sqlite-amalgamation-3390000",
    );
    let sqlite_amalgamation = sqlite_amalgamation_dir.join("sqlite3.c");
    let sqlite_sources = [
        sqlite_module_dir.join("blob.c"),
        sqlite_module_dir.join("connection.c"),
        sqlite_module_dir.join("cursor.c"),
        sqlite_module_dir.join("microprotocols.c"),
        sqlite_module_dir.join("module.c"),
        sqlite_module_dir.join("prepare_protocol.c"),
        sqlite_module_dir.join("row.c"),
        sqlite_module_dir.join("statement.c"),
        sqlite_module_dir.join("util.c"),
    ];
    for source in &sqlite_sources {
        println!("cargo:rerun-if-changed={}", source.display());
        if !source.is_file() {
            panic!("missing sqlite source at {}", source.display());
        }
    }
    let shim_src = manifest_dir.join("src/python/wasi_component_shim.c");
    println!("cargo:rerun-if-changed={}", shim_src.display());
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let shim_obj = out_dir.join("wasi_component_shim.o");
    let status = Command::new(&wasi_clang)
        .arg("--target=wasm32-wasip2")
        .arg(format!("--sysroot={}", wasi_sysroot.display()))
        .arg(format!("-I{}", python_include_dir.display()))
        .arg("-c")
        .arg(&shim_src)
        .arg("-o")
        .arg(&shim_obj)
        .status()
        .expect("failed to invoke wasi-sdk clang for component shim");
    if !status.success() {
        panic!("wasi-sdk clang failed while compiling {}", shim_src.display());
    }

    let sqlite_includes = [
        python_include_dir.as_path(),
        python_internal_include_dir.as_path(),
        python_cpython_include_dir.as_path(),
        cpython_source_internal_include_dir.as_path(),
        sqlite_amalgamation_dir.as_path(),
    ];
    let mut sqlite_objects = sqlite_sources
        .iter()
        .map(|source| compile_wasi_c_object(
            &wasi_clang,
            &wasi_sysroot,
            &out_dir,
            source,
            &sqlite_includes,
            &[],
        ))
        .collect::<Vec<_>>();
    let patched_sqlite_amalgamation =
        create_wasi_sqlite_source(&sqlite_amalgamation, &out_dir);
    sqlite_objects.push(compile_wasi_c_object(
        &wasi_clang,
        &wasi_sysroot,
        &out_dir,
        &patched_sqlite_amalgamation,
        &sqlite_includes,
        &[
            "-D_GNU_SOURCE",
            "-DSQLITE_THREADSAFE=0",
            "-DSQLITE_OMIT_LOAD_EXTENSION=1",
            "-DSQLITE_OMIT_WAL=1",
            "-DSQLITE_MAX_MMAP_SIZE=0",
            "-DSQLITE_TEMP_STORE=3",
        ],
    ));

    println!("cargo:rustc-link-arg={}", shim_obj.display());
    for object in &sqlite_objects {
        println!("cargo:rustc-link-arg={}", object.display());
    }
    println!("cargo:rustc-link-arg=--stack-first");
    println!("cargo:rustc-link-arg=--initial-memory={CPYTHON_INITIAL_MEMORY_BYTES}");
    println!("cargo:rustc-link-arg=--strip-debug");
    println!("cargo:rustc-link-arg=-z");
    println!("cargo:rustc-link-arg=stack-size={CPYTHON_STACK_SIZE_BYTES}");
    println!("cargo:rustc-link-search=native={}", python_wasi_output.display());
    println!("cargo:rustc-link-lib=static=python3.13");
    println!("cargo:rustc-link-lib=static=mpdec");
    println!("cargo:rustc-link-lib=static=Hacl_Hash_SHA2");
    println!("cargo:rustc-link-lib=static=expat");
}

fn assert_archive_exists(path: &Path) {
    if !path.is_file() {
        panic!(
            "missing WASI support archive at {}. Rebuild `tools/python-wasi/build.sh` to restore the wasi-sdk sysroot.",
            path.display()
        );
    }
}

fn create_wasi_sqlite_source(sqlite_amalgamation: &Path, out_dir: &Path) -> PathBuf {
    let source = fs::read_to_string(sqlite_amalgamation)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", sqlite_amalgamation.display()));
    let patched = source
        .replace(
            "#define HAVE_FCHOWN 1",
            "/* wasmsh: disable fchown/geteuid assumptions on wasi-libc */",
        )
        .replace(
            "#include <unistd.h>",
            concat!(
                "#include <unistd.h>\n",
                "#ifndef F_RDLCK\n#define F_RDLCK 0\n#endif\n",
                "#ifndef F_WRLCK\n#define F_WRLCK 1\n#endif\n",
                "#ifndef F_UNLCK\n#define F_UNLCK 2\n#endif\n",
                "#ifndef F_GETLK\n#define F_GETLK 5\n#endif\n",
                "#ifndef F_SETLK\n#define F_SETLK 6\n#endif\n",
                "#ifndef F_SETLKW\n#define F_SETLKW 7\n#endif\n",
            ),
        );
    let output = out_dir.join("sqlite3_wasi_component.c");
    fs::write(&output, patched)
        .unwrap_or_else(|error| panic!("failed to write {}: {error}", output.display()));
    output
}

fn compile_wasi_c_object(
    wasi_clang: &Path,
    wasi_sysroot: &Path,
    out_dir: &Path,
    source: &Path,
    include_dirs: &[&Path],
    extra_args: &[&str],
) -> PathBuf {
    let object_name = source
        .file_name()
        .and_then(|name| name.to_str())
        .expect("source file name")
        .replace('.', "_");
    let object = out_dir.join(format!("{object_name}.o"));
    let mut command = Command::new(wasi_clang);
    command
        .arg("--target=wasm32-wasip2")
        .arg(format!("--sysroot={}", wasi_sysroot.display()))
        .arg("-Oz")
        .arg("-g0")
        .arg("-ffunction-sections")
        .arg("-fdata-sections");
    for include_dir in include_dirs {
        command.arg(format!("-I{}", include_dir.display()));
    }
    for extra_arg in extra_args {
        command.arg(extra_arg);
    }
    let status = command
        .arg("-c")
        .arg(source)
        .arg("-o")
        .arg(&object)
        .status()
        .unwrap_or_else(|error| panic!("failed to invoke wasi-sdk clang for {}: {error}", source.display()));
    if !status.success() {
        panic!("wasi-sdk clang failed while compiling {}", source.display());
    }
    object
}
