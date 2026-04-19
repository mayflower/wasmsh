#!/usr/bin/env node
// Orchestrates a full docker-compose-based e2e run for the
// `WasmshRemoteSandbox` client:
//   1. Verify docker is available.
//   2. Ensure the dispatcher + runner images exist locally — build on the
//      fly if they don't (the same images the kind e2e uses, so re-runs
//      are cheap).
//   3. Bring up the compose stack from deploy/docker/compose.dispatcher-
//      test.yml, waiting for healthchecks.
//   4. Run all tests/*.test.mjs via `node --test` with the dispatcher URL
//      exposed as WASMSH_E2E_DISPATCHER_URL.
//   5. Optionally run the Python `test_remote_integration.py` through uv
//      against the same URL (skipped cleanly if `uv` is missing).
//   6. Tear the compose stack down, unless --keep was passed.
//
// Usage:
//   node scripts/run.mjs              # full cycle
//   node scripts/run.mjs --keep       # leave the stack up after the run
//   node scripts/run.mjs --skip-build # reuse existing images (faster re-runs)
//   node scripts/run.mjs --no-python  # TS only
import { readdir } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";

import {
  commandExists,
  runCommand,
} from "../../kind/lib/process.mjs";
import {
  buildImages,
  DISPATCHER_IMAGE,
  REPO_ROOT,
  RUNNER_IMAGE,
} from "../../kind/lib/cluster.mjs";
import { createDocker } from "../../kind/lib/docker.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const COMPOSE_ROOT = resolve(HERE, "..");
const COMPOSE_FILE = resolve(
  REPO_ROOT,
  "deploy/docker/compose.dispatcher-test.yml",
);
const DISPATCHER_URL = "http://localhost:8080";

async function ensureTools() {
  if (!(await commandExists("docker"))) {
    throw new Error("docker is required for the dispatcher-compose e2e");
  }
}

async function discoverTestFiles(filter) {
  const dir = resolve(COMPOSE_ROOT, "tests");
  const entries = await readdir(dir);
  const matches = entries
    .filter((name) => name.endsWith(".test.mjs"))
    .filter((name) => (filter ? name.includes(filter) : true))
    .sort()
    .map((name) => resolve(dir, name));
  if (matches.length === 0) {
    throw new Error(`no test files matched filter ${JSON.stringify(filter ?? "*")}`);
  }
  return matches;
}

// Run the existing locally-built `wasmsh-*:e2e` images through
// compose.dispatcher-test.yml.  That file defaults to the ghcr-published
// `:latest` tags, so we retag so the pull never happens.
async function retagImages() {
  const docker = createDocker();
  await docker.run(["tag", DISPATCHER_IMAGE, "ghcr.io/mayflower/wasmsh-dispatcher:latest"]);
  await docker.run(["tag", RUNNER_IMAGE, "ghcr.io/mayflower/wasmsh-runner:latest"]);
}

async function composeUp() {
  await runCommand(
    "docker",
    [
      "compose",
      "-f",
      COMPOSE_FILE,
      "up",
      "-d",
      "--wait",
      "--wait-timeout",
      "240",
    ],
    { inherit: true, timeoutMs: 10 * 60 * 1000 },
  );
}

async function composeDown() {
  await runCommand(
    "docker",
    ["compose", "-f", COMPOSE_FILE, "down", "--remove-orphans"],
    { inherit: true, timeoutMs: 2 * 60 * 1000 },
  );
}

async function waitForReadyz() {
  // compose `--wait` already polls the per-service healthchecks, but the
  // dispatcher image ships without wget/curl so its healthcheck is a
  // no-op.  Probe /readyz from the host to close that gap.
  const deadline = Date.now() + 120_000;
  while (Date.now() < deadline) {
    try {
      const resp = await fetch(`${DISPATCHER_URL}/readyz`);
      if (resp.ok) return;
    } catch {
      // not yet; retry
    }
    await new Promise((r) => setTimeout(r, 2000));
  }
  throw new Error(`dispatcher did not become ready within 120s at ${DISPATCHER_URL}/readyz`);
}

async function main() {
  const { values } = parseArgs({
    options: {
      keep: { type: "boolean", default: false },
      "skip-build": { type: "boolean", default: false },
      "no-python": { type: "boolean", default: false },
      tests: { type: "string" },
    },
  });

  await ensureTools();

  if (!values["skip-build"]) {
    await buildImages({ skipExisting: true });
  }
  await retagImages();
  await composeUp();

  const failures = [];
  try {
    await waitForReadyz();
  } catch (error) {
    failures.push(`readyz wait: ${error.message}`);
  }

  if (failures.length === 0) {
    try {
      const testFiles = await discoverTestFiles(values.tests);
      await runCommand("node", ["--test", ...testFiles], {
        cwd: COMPOSE_ROOT,
        env: { WASMSH_E2E_DISPATCHER_URL: DISPATCHER_URL },
        inherit: true,
        timeoutMs: 20 * 60 * 1000,
      });
    } catch (error) {
      failures.push(`node --test: ${error.message}`);
    }

    if (!values.tests && !values["no-python"] && (await commandExists("uv"))) {
      try {
        const pythonPkgDir = resolve(
          REPO_ROOT,
          "packages/python/langchain-wasmsh",
        );
        await runCommand(
          "uv",
          [
            "run",
            "--group",
            "test",
            "pytest",
            "tests/integration_tests/test_remote_integration.py",
            "-v",
            "--timeout",
            "180",
          ],
          {
            cwd: pythonPkgDir,
            env: { WASMSH_DISPATCHER_URL: DISPATCHER_URL },
            inherit: true,
            timeoutMs: 30 * 60 * 1000,
          },
        );
      } catch (error) {
        failures.push(`pytest: ${error.message}`);
      }
    }
  }

  const testsFailed = failures.length > 0;
  for (const failure of failures) console.error("dispatcher-compose e2e tests failed:", failure);

  if (!values.keep) {
    await composeDown();
  }

  if (testsFailed) process.exit(1);
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
