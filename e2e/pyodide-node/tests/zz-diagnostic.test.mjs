// Temporary diagnostic — not a real test. Establishes *where* the wasm
// `unreachable` trap happens for jsonschema/pydantic on CI, and with which
// resolved wheels, since the normal suite only surfaces the bare word
// "unreachable" with no Python-side context.
import { describe, it } from "node:test";
import { existsSync } from "node:fs";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const PKG_DIR = resolve(__dirname, "../../../packages/npm/wasmsh-pyodide");
const ASSETS_DIR = resolve(PKG_DIR, "assets");
const SKIP =
  process.env.SKIP_PYODIDE === "1" ||
  !existsSync(resolve(ASSETS_DIR, "pyodide.asm.wasm"));

let createNodeSession;
if (!SKIP) {
  ({ createNodeSession } = await import(resolve(PKG_DIR, "index.js")));
}

const ALLOWED = ["cdn.jsdelivr.net", "pypi.org", "files.pythonhosted.org"];

async function show(label, fn) {
  try {
    const value = await fn();
    console.log(`[diag] ${label}: OK ${JSON.stringify(value)?.slice(0, 1200)}`);
    return value;
  } catch (error) {
    console.log(`[diag] ${label}: THREW ${error?.message ?? error}`);
    return null;
  }
}

describe("diagnostic", () => {
  for (const pkg of ["regex", "jsonschema", "pydantic"]) {
    it(`${pkg} — where does it break`, { skip: SKIP, timeout: 180_000 }, async () => {
      const s = await createNodeSession({ assetDir: ASSETS_DIR, allowedHosts: ALLOWED });
      try {
        await show(`${pkg} install`, () => s.installPythonPackages(pkg));
        await show(`${pkg} versions`, async () => {
          const r = await s.run(
            `python3 -c "import micropip,json; print(json.dumps({k:v.version for k,v in micropip.list().items()}))"`,
          );
          return { exit: r.exitCode, out: r.stdout?.trim(), err: r.stderr?.trim()?.slice(0, 400) };
        });
        await show(`${pkg} import`, async () => {
          const r = await s.run(
            `python3 -c "
import traceback
try:
    import ${pkg}
    print('IMPORT_OK')
except BaseException:
    traceback.print_exc()
"`,
          );
          return { exit: r.exitCode, out: r.stdout?.trim(), err: r.stderr?.trim()?.slice(0, 800) };
        });
      } finally {
        await s.close().catch(() => {});
      }
    });
  }
});
