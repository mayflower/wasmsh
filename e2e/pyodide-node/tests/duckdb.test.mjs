/**
 * DuckDB integration tests — Pyodide Node.
 *
 * Verifies that DuckDB can be installed offline from bundled assets,
 * imported in Python, and used to create/query databases on the shared
 * Emscripten filesystem.
 *
 * These tests do NOT require network access — DuckDB is bundled in the
 * local pyodide-lock.json and resolved via pyodide.loadPackage().
 *
 * Skip: SKIP_PYODIDE=1
 */
import { describe, it } from "node:test";
import { existsSync } from "node:fs";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";

import { createSessionTracker } from "./test-session-helper.mjs";

const __dirname = dirname(fileURLToPath(import.meta.url));
const PKG_DIR = resolve(__dirname, "../../../packages/npm/wasmsh-pyodide");
const ASSETS_DIR = resolve(PKG_DIR, "assets");

const SKIP =
  process.env.SKIP_PYODIDE === "1" ||
  !existsSync(resolve(ASSETS_DIR, "pyodide.asm.wasm"));

// DuckDB import requires MAIN_MODULE=1 (or MAIN_MODULE=2 with the
// standard export list).  Skip import/query tests if the build does
// not support compiled side modules yet.
const SKIP_IMPORT = SKIP || process.env.SKIP_DUCKDB_IMPORT === "1";

let createNodeSession;
if (!SKIP) {
  ({ createNodeSession } = await import(resolve(PKG_DIR, "index.js")));
}

describe("DuckDB integration (Pyodide Node)", () => {
  const openSession = SKIP ? null : createSessionTracker(createNodeSession, ASSETS_DIR);

  // ── Offline install ────────────────────────────────────────

  it("pip install duckdb works offline (no allowedHosts)", { skip: SKIP, timeout: 120_000 }, async () => {
    const s = await openSession();
    const result = await s.run("pip install duckdb");
    assert.equal(result.exitCode, 0, `pip install failed: ${result.stderr}`);
    assert.ok(result.stdout.includes("Successfully installed duckdb"));
  });

  it("installPythonPackages('duckdb') works offline", { skip: SKIP, timeout: 120_000 }, async () => {
    const s = await openSession();
    const result = await s.installPythonPackages("duckdb");
    assert.ok(result.installed.length > 0, "should install duckdb");
    assert.equal(result.installed[0].requirement, "duckdb");
  });

  // ── Import + query ─────────────────────────────────────────

  it("import duckdb succeeds after install", { skip: SKIP_IMPORT, timeout: 120_000 }, async () => {
    const s = await openSession();
    await s.installPythonPackages("duckdb");
    const r = await s.run('python3 -c "import duckdb; print(duckdb.__version__)"');
    assert.equal(r.exitCode, 0, `import failed: ${r.stderr}`);
    assert.ok(r.stdout.trim().length > 0, "should print version");
  });

  it("basic query: select 42", { skip: SKIP_IMPORT, timeout: 120_000 }, async () => {
    const s = await openSession();
    await s.installPythonPackages("duckdb");
    const r = await s.run('python3 -c "import duckdb; print(duckdb.sql(\'select 42 as answer\').fetchall())"');
    assert.equal(r.exitCode, 0, `query failed: ${r.stderr}`);
    assert.ok(r.stdout.includes("42"), `expected 42 in output: ${r.stdout}`);
  });

  // ── Shared filesystem persistence ──────────────────────────

  it("DB file on shared FS persists across commands", { skip: SKIP_IMPORT, timeout: 180_000 }, async () => {
    const s = await openSession();
    await s.installPythonPackages("duckdb");

    // Create a database and write data
    const create = await s.run(`python3 -c "
import duckdb
con = duckdb.connect('/workspace/demo.duckdb')
con.sql('create table t as select 10 as x union all select 32')
print(con.sql('select sum(x) from t').fetchall())
con.close()
"`);
    assert.equal(create.exitCode, 0, `create failed: ${create.stderr}`);
    assert.ok(create.stdout.includes("42"), `expected 42: ${create.stdout}`);

    // Verify the file exists via shell
    const ls = await s.run("ls -la /workspace/demo.duckdb");
    assert.equal(ls.exitCode, 0, "DB file should exist on shared FS");

    // Reopen and verify data in a separate command
    const reopen = await s.run(`python3 -c "
import duckdb
con = duckdb.connect('/workspace/demo.duckdb')
rows = con.sql('select sum(x) from t').fetchall()
print(rows)
assert rows == [(42,)], f'unexpected: {rows}'
print('persistence OK')
con.close()
"`);
    assert.equal(reopen.exitCode, 0, `reopen failed: ${reopen.stderr}`);
    assert.ok(reopen.stdout.includes("persistence OK"));
  });

  // ── pip install flow (shell UX) ────────────────────────────

  it("pip install + python3 -c query end-to-end", { skip: SKIP_IMPORT, timeout: 180_000 }, async () => {
    const s = await openSession();
    const install = await s.run("pip install duckdb");
    assert.equal(install.exitCode, 0);

    const query = await s.run(`python3 -c "
import duckdb
con = duckdb.connect('/workspace/test.duckdb')
con.sql('create table items as select * from range(5) t(id)')
result = con.sql('select count(*) from items').fetchall()
print(result)
con.close()
"`);
    assert.equal(query.exitCode, 0, `query failed: ${query.stderr}`);
    assert.ok(query.stdout.includes("5"), `expected 5: ${query.stdout}`);
  });
});
