export function buildBaselineBootPlan({ assetDir } = {}) {
  return Object.freeze({
    assetDir: assetDir ?? null,
    network_required: false,
    optional_python_packages: Object.freeze([]),
    dynamic_install_steps: Object.freeze([]),
    restore_strategy: "baseline-boot",
    required_files: Object.freeze([
      "pyodide.mjs",
      "pyodide.asm.js",
      "pyodide.asm.wasm",
      "python_stdlib.zip",
      "pyodide-lock.json",
    ]),
  });
}
