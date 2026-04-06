/**
 * Agentic DuckDB E2E tests — Pyodide Node.
 *
 * Simulates multi-step workflows an AI agent would run inside wasmsh:
 * writing files with shell tools, analyzing with DuckDB, piping results
 * between shell and Python, and building up state across commands.
 *
 * Skip: SKIP_PYODIDE=1 or SKIP_DUCKDB_IMPORT=1
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
  process.env.SKIP_DUCKDB_IMPORT === "1" ||
  !existsSync(resolve(ASSETS_DIR, "pyodide.asm.wasm"));

let createNodeSession;
if (!SKIP) {
  ({ createNodeSession } = await import(resolve(PKG_DIR, "index.js")));
}

describe("DuckDB agentic workflows (Pyodide Node)", () => {
  const openSession = SKIP
    ? null
    : createSessionTracker(createNodeSession, ASSETS_DIR);

  // ── Data pipeline: shell → DuckDB → shell ─────────────────

  it("shell writes CSV, DuckDB analyzes, shell reads result", { skip: SKIP, timeout: 180_000 }, async () => {
    const s = await openSession();
    await s.installPythonPackages("duckdb");

    // Step 1: agent creates a dataset with shell
    let r = await s.run(`cat > /workspace/sales.csv << 'EOF'
region,product,revenue
EMEA,widget,1200
APAC,widget,800
EMEA,gadget,3500
AMER,widget,2100
APAC,gadget,1900
AMER,gadget,4200
EOF`);
    assert.equal(r.exitCode, 0);

    // Step 2: agent uses DuckDB to answer "top region by total revenue"
    r = await s.run(`python3 << 'PY'
import duckdb
result = duckdb.sql("""
    select region, sum(revenue) as total
    from read_csv_auto('/workspace/sales.csv')
    group by region
    order by total desc
    limit 1
""").fetchall()
region, total = result[0]
print(f"{region},{total}")
PY`);
    assert.equal(r.exitCode, 0, `analysis failed: ${r.stderr}`);
    assert.equal(r.stdout.trim(), "AMER,6300");

    // Step 3: agent uses shell to validate the answer
    r = await s.run("awk -F, 'NR>1{a[$1]+=$3}END{for(k in a)print k,a[k]}' /workspace/sales.csv | sort -k2 -nr | head -1");
    assert.equal(r.exitCode, 0);
    assert.ok(r.stdout.includes("AMER") && r.stdout.includes("6300"));
  });

  // ── Multi-step stateful analysis ──────────────────────────

  it("builds up a database across multiple agent turns", { skip: SKIP, timeout: 180_000 }, async () => {
    const s = await openSession();
    await s.installPythonPackages("duckdb");

    // Turn 1: agent creates the schema
    let r = await s.run(`python3 -c "
import duckdb
con = duckdb.connect('/workspace/project.duckdb')
con.sql('create table events(ts timestamp, kind varchar, user_id int)')
con.close()
print('schema created')
"`);
    assert.equal(r.exitCode, 0);
    assert.ok(r.stdout.includes("schema created"));

    // Turn 2: agent inserts data (separate command, reopens DB)
    r = await s.run(`python3 -c "
import duckdb
con = duckdb.connect('/workspace/project.duckdb')
con.sql(\"\"\"
    insert into events values
    ('2025-01-15 10:00', 'login', 1),
    ('2025-01-15 10:05', 'purchase', 1),
    ('2025-01-15 11:00', 'login', 2),
    ('2025-01-15 11:30', 'login', 3),
    ('2025-01-15 12:00', 'purchase', 2),
    ('2025-01-15 14:00', 'login', 1),
    ('2025-01-15 14:10', 'purchase', 1)
\"\"\")
print(f'rows: {con.sql(\"select count(*) from events\").fetchone()[0]}')
con.close()
"`);
    assert.equal(r.exitCode, 0);
    assert.ok(r.stdout.includes("rows: 7"));

    // Turn 3: agent runs analytics
    r = await s.run(`python3 -c "
import duckdb
con = duckdb.connect('/workspace/project.duckdb')
result = con.sql(\"\"\"
    select user_id, count(*) filter (kind = 'purchase') as purchases
    from events
    group by user_id
    order by purchases desc
\"\"\").fetchall()
for uid, purchases in result:
    print(f'user {uid}: {purchases} purchases')
con.close()
"`);
    assert.equal(r.exitCode, 0, `analytics failed: ${r.stderr}`);
    assert.ok(r.stdout.includes("user 1: 2 purchases"));
    assert.ok(r.stdout.includes("user 2: 1 purchases"));
    assert.ok(r.stdout.includes("user 3: 0 purchases"));
  });

  // ── DuckDB + shell tools interop ──────────────────────────

  it("DuckDB exports JSON, jq processes it", { skip: SKIP, timeout: 180_000 }, async () => {
    const s = await openSession();
    await s.installPythonPackages("duckdb");

    // Agent creates data and exports as JSON
    let r = await s.run(`python3 << 'PY'
import duckdb, json
rows = duckdb.sql("""
    select i as id, chr(65 + i) as letter, i * i as square
    from range(5) t(i)
""").fetchall()
with open('/workspace/data.json', 'w') as f:
    json.dump([{"id": r[0], "letter": r[1], "square": r[2]} for r in rows], f)
print('exported')
PY`);
    assert.equal(r.exitCode, 0, `export failed: ${r.stderr}`);

    // Agent uses jq to extract specific fields
    r = await s.run("jq '.[3].letter' /workspace/data.json");
    assert.equal(r.exitCode, 0);
    assert.equal(r.stdout.trim(), '"D"');

    // Agent uses jq to compute something
    r = await s.run("jq '[.[].square] | add' /workspace/data.json");
    assert.equal(r.exitCode, 0);
    assert.equal(r.stdout.trim(), "30"); // 0+1+4+9+16
  });

  // ── WriteFile API → DuckDB ────────────────────────────────

  it("agent uploads data via writeFile then queries with DuckDB", { skip: SKIP, timeout: 180_000 }, async () => {
    const s = await openSession();
    await s.installPythonPackages("duckdb");

    // Simulate an agent uploading a file through the API
    const csv = "name,age,city\nAlice,30,Berlin\nBob,25,Munich\nCarol,35,Hamburg\nDave,28,Berlin\n";
    await s.writeFile("/workspace/people.csv", new TextEncoder().encode(csv));

    // Agent queries the uploaded data
    const r = await s.run(`python3 << 'PY'
import duckdb
result = duckdb.sql("""
    select city, count(*) as n, round(avg(age), 1) as avg_age
    from read_csv_auto('/workspace/people.csv')
    group by city
    order by n desc, city
""").fetchall()
for row in result:
    print(f'{row[0]}: {row[1]} people, avg age {row[2]}')
PY`);
    assert.equal(r.exitCode, 0, `query failed: ${r.stderr}`);
    assert.ok(r.stdout.includes("Berlin: 2 people"));
    assert.ok(r.stdout.includes("Hamburg: 1 people"));
  });

  // ── In-memory analysis with Python data ───────────────────

  it("agent passes Python objects to DuckDB for ad-hoc analysis", { skip: SKIP, timeout: 120_000 }, async () => {
    const s = await openSession();
    await s.installPythonPackages("duckdb");

    const r = await s.run(`python3 << 'PY'
import duckdb

# Simulates data an agent computed in Python
metrics = [
    {"endpoint": "/api/users", "latency_ms": 45, "status": 200},
    {"endpoint": "/api/users", "latency_ms": 1200, "status": 200},
    {"endpoint": "/api/orders", "latency_ms": 89, "status": 200},
    {"endpoint": "/api/users", "latency_ms": 52, "status": 500},
    {"endpoint": "/api/orders", "latency_ms": 95, "status": 200},
    {"endpoint": "/api/users", "latency_ms": 38, "status": 200},
]

result = duckdb.sql("""
    select
        endpoint,
        count(*) as requests,
        round(avg(latency_ms), 1) as avg_latency,
        max(latency_ms) as p100,
        count(*) filter (status >= 500) as errors
    from metrics
    group by endpoint
    order by endpoint
""").fetchall()

for ep, reqs, avg_lat, p100, errs in result:
    print(f"{ep}: {reqs} reqs, avg {avg_lat}ms, p100 {p100}ms, {errs} errors")
PY`);
    assert.equal(r.exitCode, 0, `analysis failed: ${r.stderr}`);
    assert.ok(r.stdout.includes("/api/orders: 2 reqs"));
    assert.ok(r.stdout.includes("/api/users: 4 reqs"));
    assert.ok(r.stdout.includes("1 errors"));
  });

  // ── Heredoc SQL workflow ──────────────────────────────────

  it("agent runs multi-step SQL via heredoc piped through python", { skip: SKIP, timeout: 180_000 }, async () => {
    const s = await openSession();
    await s.installPythonPackages("duckdb");

    // Agent writes a reusable SQL script
    let r = await s.run(`cat > /workspace/analyze.py << 'PYEOF'
import duckdb, sys
db_path = sys.argv[1] if len(sys.argv) > 1 else ':memory:'
con = duckdb.connect(db_path)
con.sql("create table if not exists log(msg varchar, level varchar)")
con.sql("""
    insert into log values
    ('startup complete', 'INFO'),
    ('connection refused', 'ERROR'),
    ('request handled', 'INFO'),
    ('disk full', 'ERROR'),
    ('request handled', 'INFO'),
    ('timeout', 'WARN'),
    ('request handled', 'INFO')
""")
result = con.sql("""
    select level, count(*) as cnt
    from log
    group by level
    order by cnt desc
""").fetchall()
for level, cnt in result:
    print(f"{level}: {cnt}")
con.close()
PYEOF`);
    assert.equal(r.exitCode, 0);

    // Agent runs the script
    r = await s.run("python3 /workspace/analyze.py /workspace/logs.duckdb");
    assert.equal(r.exitCode, 0, `script failed: ${r.stderr}`);
    assert.ok(r.stdout.includes("INFO: 4"));
    assert.ok(r.stdout.includes("ERROR: 2"));
    assert.ok(r.stdout.includes("WARN: 1"));

    // Agent verifies the DB file was created on the shared FS
    r = await s.run("ls -la /workspace/logs.duckdb");
    assert.equal(r.exitCode, 0, "DB file should exist");
  });
});
