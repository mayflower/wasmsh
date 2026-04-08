# JavaScript / Node.js Quick Start

This tutorial walks you from `npm install` to a working wasmsh sandbox in
Node.js. By the end you will have:

- Loaded the runtime in a Node script.
- Seeded the virtual filesystem with a file.
- Run a shell pipeline that processes that file.
- Read the result back into JavaScript.

It takes about ten minutes.

## Prerequisites

- **Node.js 20 or later.** The npm package only supports Node 20+.
- **A package manager** (`npm`, `pnpm`, or `yarn`).

## Step 1: Install the package

```bash
mkdir wasmsh-quickstart
cd wasmsh-quickstart
npm init -y
npm install @mayflowergmbh/wasmsh-pyodide
```

This pulls in the npm package, which contains a custom Pyodide build with
wasmsh linked in plus a thin Node host adapter.

## Step 2: Boot the runtime

Create `index.mjs`:

```javascript
import { createNodeSession } from "@mayflowergmbh/wasmsh-pyodide";

const session = await createNodeSession({
  stepBudget: 100_000,
  allowedHosts: [],   // no network
});

console.log("session created");

const echo = await session.run("echo 'hello from wasmsh'");
console.log("stdout:", echo.stdout);
console.log("exit:", echo.exitCode);

await session.close();
```

Run it:

```bash
node index.mjs
```

You should see:

```
session created
stdout: hello from wasmsh

exit: 0
```

If you see anything else (especially a Pyodide download progress bar
followed by a hang), make sure you are on Node 20+ and that your machine
has internet access for the first run — Pyodide downloads its assets on
first boot.

## Step 3: Seed the virtual filesystem

The runtime starts with an empty in-process filesystem. Write a file
before running a script that needs it:

```javascript
const csv = `name,score
alice,42
bob,17
carol,99
`;

await session.writeFile("/data/scores.csv", new TextEncoder().encode(csv));
```

After this, `/data/scores.csv` exists inside the sandbox. The host's
filesystem is untouched — wasmsh has no access to it.

## Step 4: Run a pipeline against the file

```javascript
const result = await session.run(
  "tail -n +2 /data/scores.csv | cut -d, -f2 | sort -n | tail -1"
);

console.log("highest score:", result.stdout.trim());
console.log("exit:", result.exitCode);
```

Expected output:

```
highest score: 99
exit: 0
```

What just happened: `tail` skipped the header, `cut` extracted the score
column, `sort -n` sorted numerically, and the final `tail -1` picked the
largest value. All four utilities ran in-process inside wasm — no host
processes were spawned.

## Step 5: Run Python in the same session

The Pyodide build links wasmsh into the same WebAssembly module as
CPython. That means `python` and `python3` work as commands and they see
the same filesystem:

```javascript
const py = await session.run(`
python3 - <<'PY'
import csv
with open("/data/scores.csv") as f:
    rows = list(csv.DictReader(f))
print(sum(int(r["score"]) for r in rows) / len(rows))
PY
`);

console.log("average:", py.stdout.trim());
```

Expected output:

```
average: 52.666666666666664
```

Both shell utilities and Python see `/data/scores.csv` because they share
the same Emscripten filesystem.

## Step 6: Read a file back into JavaScript

Suppose the script wrote a result file. You can pull it back into the
host:

```javascript
await session.run("echo 'top: carol (99)' > /data/result.txt");
const file = await session.readFile("/data/result.txt");
console.log("result file contents:", new TextDecoder().decode(file.content));
```

## Step 7: Clean up

Always call `close()` when you are done:

```javascript
await session.close();
```

This frees the runtime, cancels any pending tasks, and releases the
underlying Pyodide module.

## What just happened

You ran shell commands and Python code inside a WebAssembly sandbox with
no access to your host machine's filesystem, processes, or network. Every
command — the coreutils, the pipeline glue, the Python interpreter — ran
in-process inside the same wasm module.

## Where to go next

- [Python quick start](python-quickstart.md) for the same flow from Python.
- [Pyodide integration](../guides/pyodide-integration.md) for the deeper
  details: customising the boot, installing Python packages with pip, and
  the host adapter API.
- [Sandbox and capabilities](../reference/sandbox-and-capabilities.md) for
  what `stepBudget`, `allowedHosts`, and the other knobs actually do.
- [Worker protocol reference](../reference/protocol.md) for the underlying
  message protocol the host adapter wraps.
- [Troubleshooting](../guides/troubleshooting.md) if anything in this
  tutorial misbehaved.
