/**
 * Browser Playwright tests for bundled DuckDB support.
 *
 * Verifies that DuckDB can be installed offline from bundled assets in
 * the browser worker, imported in Python, and used to create/query
 * databases on the shared Emscripten filesystem.
 *
 * Install tests work with the current build.  Import/query tests
 * require the standard export list in EXPORTED_FUNCTIONS (set
 * SKIP_DUCKDB_IMPORT=1 to skip them if the build doesn't support
 * compiled side modules yet).
 */
import { test, expect } from "@playwright/test";

const SKIP_IMPORT = process.env.SKIP_DUCKDB_IMPORT === "1";

/**
 * Send a JSON-RPC request to the npm package worker and return the result.
 */
async function send(page: any, method: string, params: any = {}): Promise<any> {
  return page.evaluate(
    async ([m, p]: [string, any]) => {
      return (window as any)._npmSend(m, p);
    },
    [method, params],
  );
}

// ── Setup: navigate + wait for worker ready ─────────────────

test.beforeEach(async ({ page }) => {
  await page.goto("/npm-test.html");
  await page.waitForFunction(
    () => (window as any)._npmWorkerReady === true,
    { timeout: 90_000 },
  );
});

// ── Offline install ─────────────────────────────────────────

test("pip install duckdb works offline in browser", async ({ page }) => {
  const result = await send(page, "run", { command: "pip install duckdb" });
  expect(result.exitCode).toBe(0);
  expect(result.stdout).toContain("Successfully installed duckdb");
});

test("installPythonPackages('duckdb') works offline in browser", async ({
  page,
}) => {
  const result = await send(page, "installPythonPackages", {
    requirements: "duckdb",
  });
  expect(result).toBeTruthy();
  expect(result.installed).toHaveLength(1);
  expect(result.installed[0].requirement).toBe("duckdb");
});

// ── Import + query (needs standard export list) ─────────────

test("import duckdb succeeds in browser", async ({ page }) => {
  test.skip(SKIP_IMPORT, "compiled side modules not yet supported");

  await send(page, "installPythonPackages", { requirements: "duckdb" });
  const r = await send(page, "run", {
    command: 'python3 -c "import duckdb; print(duckdb.__version__)"',
  });
  expect(r.exitCode).toBe(0);
  expect(r.stdout.trim().length).toBeGreaterThan(0);
});

test("DuckDB query works in browser", async ({ page }) => {
  test.skip(SKIP_IMPORT, "compiled side modules not yet supported");

  await send(page, "installPythonPackages", { requirements: "duckdb" });
  const r = await send(page, "run", {
    command:
      'python3 -c "import duckdb; print(duckdb.sql(\'select 42 as answer\').fetchall())"',
  });
  expect(r.exitCode).toBe(0);
  expect(r.stdout).toContain("42");
});

test("DB file persists on shared FS in browser", async ({ page }) => {
  test.skip(SKIP_IMPORT, "compiled side modules not yet supported");

  await send(page, "installPythonPackages", { requirements: "duckdb" });

  // Create database
  const create = await send(page, "run", {
    command: `python3 -c "
import duckdb
con = duckdb.connect('/workspace/browser-test.duckdb')
con.sql('create table t as select 10 as x union all select 32')
print(con.sql('select sum(x) from t').fetchall())
con.close()
"`,
  });
  expect(create.exitCode).toBe(0);
  expect(create.stdout).toContain("42");

  // Verify file exists
  const ls = await send(page, "run", {
    command: "ls /workspace/browser-test.duckdb",
  });
  expect(ls.exitCode).toBe(0);

  // Reopen and query
  const reopen = await send(page, "run", {
    command: `python3 -c "
import duckdb
con = duckdb.connect('/workspace/browser-test.duckdb')
rows = con.sql('select sum(x) from t').fetchall()
print(rows)
assert rows == [(42,)], f'unexpected: {rows}'
print('browser persistence OK')
con.close()
"`,
  });
  expect(reopen.exitCode).toBe(0);
  expect(reopen.stdout).toContain("browser persistence OK");
});
