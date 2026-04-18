#!/usr/bin/env node
// Orchestrates a full kind-based end-to-end run:
//   1. Require prerequisite binaries.
//   2. Build dispatcher + runner images.
//   3. Create the kind cluster, load images, helm install.
//   4. Open a port-forward to the dispatcher service.
//   5. Run all test files via `node --test`.
//   6. Tear down (unless --keep or the test suite fails with --keep-on-failure).
//
// Usage:
//   node scripts/run.mjs              # full cycle
//   node scripts/run.mjs --keep       # leave cluster up after the run
//   node scripts/run.mjs --reuse      # reuse an existing cluster if present
//   node scripts/run.mjs --tests foo  # restrict to a single test file glob
import { readdir } from "node:fs/promises";
import { resolve } from "node:path";
import { parseArgs } from "node:util";

import { commandExists } from "../lib/process.mjs";
import { openPortForward } from "../lib/port-forward.mjs";
import { runCommand } from "../lib/process.mjs";
import {
  E2E_ROOT,
  HELM_RELEASE,
  NAMESPACE,
  setupCluster,
  teardownCluster,
} from "../lib/cluster.mjs";

const REQUIRED_TOOLS = ["docker", "kind", "kubectl", "helm"];

async function ensureTools() {
  const missing = [];
  for (const tool of REQUIRED_TOOLS) {
    if (!(await commandExists(tool))) missing.push(tool);
  }
  if (missing.length > 0) {
    throw new Error(
      `missing required tools on PATH: ${missing.join(", ")}. install via brew/apt before running the kind e2e.`,
    );
  }
}

async function discoverTestFiles(filter) {
  const dir = resolve(E2E_ROOT, "tests");
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

async function main() {
  const { values } = parseArgs({
    options: {
      keep: { type: "boolean", default: false },
      "keep-on-failure": { type: "boolean", default: false },
      reuse: { type: "boolean", default: false },
      tests: { type: "string" },
      "skip-build": { type: "boolean", default: false },
    },
  });

  await ensureTools();

  const tooling = await setupCluster({
    reuseExisting: values.reuse,
    buildImagesIfMissing: values["skip-build"],
  });

  const dispatcherService = `svc/${HELM_RELEASE}-dispatcher`;
  const portForward = await openPortForward({
    kubeconfig: tooling.kubeconfig,
    context: tooling.context,
    namespace: NAMESPACE,
    target: dispatcherService,
    targetPort: 8080,
    localPort: 0,
    readyTimeoutMs: 30_000,
  });

  let testsFailed = false;
  try {
    const testFiles = await discoverTestFiles(values.tests);
    // Each node --test run receives the port-forward URL + kubeconfig via
    // env vars.  Tests should never mutate cluster-scoped resources.
    await runCommand("node", ["--test", ...testFiles], {
      cwd: E2E_ROOT,
      env: {
        WASMSH_E2E_DISPATCHER_URL: portForward.url,
        WASMSH_E2E_KUBECONFIG: tooling.kubeconfig,
        WASMSH_E2E_KUBE_CONTEXT: tooling.context,
        WASMSH_E2E_NAMESPACE: NAMESPACE,
        WASMSH_E2E_RELEASE: HELM_RELEASE,
      },
      inherit: true,
      timeoutMs: 20 * 60 * 1000,
    });
  } catch (error) {
    testsFailed = true;
    console.error("kind e2e tests failed:", error.message);
  } finally {
    await portForward.stop();
    const keep = values.keep || (values["keep-on-failure"] && testsFailed);
    await teardownCluster({ keep });
  }

  if (testsFailed) {
    process.exit(1);
  }
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
