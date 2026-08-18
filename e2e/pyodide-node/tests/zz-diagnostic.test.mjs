// Temporary diagnostic — not a real test. Establishes *where* the wasm
// `unreachable` trap happens for jsonschema/pydantic on CI, and with which
// resolved wheels, since the normal suite only surfaces the bare word
// "unreachable" with no Python-side context.
import { describe, it } from "node:test";
import { createNodeSession } from "@mayflowergmbh/wasmsh-pyodide";

const ALLOWED = ["pypi.org", "files.pythonhosted.org", "cdn.jsdelivr.net"];

async function show(label, fn) {
  try {
    const value = await fn();
    console.log(`[diag] ${label}: OK ${JSON.stringify(value)?.slice(0, 900)}`);
    return value;
  } catch (error) {
    console.log(`[diag] ${label}: THREW ${error?.message ?? error}`);
    return null;
  }
}

describe("diagnostic", () => {
  for (const pkg of ["regex", "jsonschema", "pydantic"]) {
    it(`${pkg} — where does it break`, { timeout: 180_000 }, async () => {
      const s = await createNodeSession({ allowedHosts: ALLOWED });
      try {
        await show(`${pkg} install`, () => s.installPythonPackages(pkg));
        await show(`${pkg} micropip.list`, async () => {
          const r = await s.run(
            `python3 -c "import micropip,json; print(json.dumps({k:v.version for k,v in micropip.list().items()}))"`,
          );
          return { exit: r.exitCode, out: r.stdout?.trim(), err: r.stderr?.trim() };
        });
        await show(`${pkg} import`, async () => {
          const r = await s.run(
            `python3 -c "
import traceback
try:
    import ${pkg}
    print('IMPORT_OK', ${pkg}.__name__)
except BaseException:
    traceback.print_exc()
"`,
          );
          return { exit: r.exitCode, out: r.stdout?.trim(), err: r.stderr?.trim() };
        });
      } finally {
        await s.close().catch(() => {});
      }
    });
  }
});
