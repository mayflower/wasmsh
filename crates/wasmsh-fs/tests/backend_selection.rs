use std::fs;
use std::path::PathBuf;

fn fs_lib_source() -> String {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    fs::read_to_string(manifest_dir.join("src/lib.rs")).expect("read wasmsh-fs src/lib.rs")
}

#[test]
fn backend_selection_mentions_wasip2_cfg_for_libc_backend() {
    let source = fs_lib_source();
    assert!(
        source.contains("target_os = \"wasi\"") && source.contains("target_env = \"p2\""),
        "BackendFs selection must explicitly cover wasm32-wasip2 cfgs instead of only emscripten",
    );
}

#[test]
fn backend_selection_has_explicit_non_memoryfs_guard_for_wasip2() {
    let source = fs_lib_source();
    assert!(
        source.contains("BackendFs = EmscriptenFs")
            && !source.contains("not(all(feature = \"emscripten\", target_os = \"emscripten\"))"),
        "wasm32-wasip2 must not fall through the old MemoryFs fallback gate",
    );
}
