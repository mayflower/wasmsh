import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { test } from "node:test";
import assert from "node:assert/strict";

const repoRoot = process.cwd();
const workflowPath = resolve(repoRoot, ".github/workflows/runner-dispatcher.yml");

test("runner and dispatcher workflow covers contract, lint, and e2e gates", () => {
  const workflow = readFileSync(workflowPath, "utf8");

  assert.match(workflow, /cargo test -p wasmsh-dispatcher/);
  assert.match(workflow, /cargo clippy -p wasmsh-dispatcher --all-targets -- -D warnings/);
  assert.match(workflow, /just test-e2e-runner-node/);
  assert.match(workflow, /node --test e2e\/build-contract\/tests\/runner-workflow-contract\.test\.mjs/);
  assert.match(workflow, /node-version:\s*["']22["']/);
});
