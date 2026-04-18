import { test } from "node:test";
import assert from "node:assert/strict";

import { buildBaselineBootPlan } from "../../../packages/npm/wasmsh-pyodide/lib/baseline/boot-plan.mjs";
import { composeWasmImports } from "../../../packages/npm/wasmsh-pyodide/lib/baseline/import-composer.mjs";
import { assertOfflineBaselineBootPlan } from "../../../packages/npm/wasmsh-pyodide/lib/baseline/offline-guard.mjs";
import {
  createSandboxGlobalSurface,
  captureScopedGlobals,
  restoreScopedGlobals,
} from "../../../packages/npm/wasmsh-pyodide/lib/baseline/sandbox-globals.mjs";
import {
  assertWorkspaceEmpty,
  buildEntropyContract,
} from "../../../packages/npm/wasmsh-pyodide/lib/snapshot/entropy-contract.mjs";
import {
  assertSerializableJsRefs,
  captureJsRefManifest,
  hashJsRefs,
} from "../../../packages/npm/wasmsh-pyodide/lib/snapshot/jsref-registry.mjs";
import { digestJson } from "../../../packages/npm/wasmsh-pyodide/lib/snapshot/manifest.mjs";

test("buildBaselineBootPlan is fully offline and freezes the contract", () => {
  const plan = buildBaselineBootPlan({ assetDir: "/assets" });
  assert.equal(plan.assetDir, "/assets");
  assert.equal(plan.network_required, false);
  assert.deepEqual(plan.optional_python_packages, []);
  assert.deepEqual(plan.dynamic_install_steps, []);
  assert.equal(plan.restore_strategy, "baseline-boot");
  assert.ok(Object.isFrozen(plan));
  assert.ok(Object.isFrozen(plan.required_files));
});

test("buildBaselineBootPlan returns a null assetDir when none is provided", () => {
  const plan = buildBaselineBootPlan();
  assert.equal(plan.assetDir, null);
});

test("composeWasmImports deep-merges env and sentinel without mutating the original", () => {
  const base = {
    env: { keep: 1 },
    sentinel: { preserved: true },
    other: { untouched: true },
  };
  const merged = composeWasmImports({
    imports: base,
    env: { new: 2 },
    sentinel: { fresh: "yes" },
  });
  assert.equal(merged.env.keep, 1);
  assert.equal(merged.env.new, 2);
  assert.equal(merged.sentinel.preserved, true);
  assert.equal(merged.sentinel.fresh, "yes");
  assert.deepEqual(merged.other, { untouched: true });
  assert.equal(base.env.new, undefined, "original imports.env must not be mutated");
});

test("composeWasmImports tolerates missing input fields", () => {
  const merged = composeWasmImports();
  assert.deepEqual(merged.env, {});
  assert.deepEqual(merged.sentinel, {});
});

test("assertOfflineBaselineBootPlan rejects plans that imply network access", () => {
  assert.throws(() => assertOfflineBaselineBootPlan(null), /baseline boot plan is required/);
  assert.throws(
    () => assertOfflineBaselineBootPlan({ network_required: true }),
    /must not require network access/,
  );
  assert.throws(
    () => assertOfflineBaselineBootPlan({
      network_required: false,
      optional_python_packages: ["numpy"],
    }),
    /must not preload optional Python packages/,
  );
  assert.throws(
    () => assertOfflineBaselineBootPlan({
      network_required: false,
      optional_python_packages: [],
      dynamic_install_steps: [{ kind: "pip" }],
    }),
    /must not perform dynamic install steps/,
  );
});

test("assertOfflineBaselineBootPlan returns the plan unchanged on success", () => {
  const plan = buildBaselineBootPlan();
  assert.strictEqual(assertOfflineBaselineBootPlan(plan), plan);
});

test("createSandboxGlobalSurface strips the blocked globals", () => {
  const source = {
    require: () => "blocked",
    process: {},
    Deno: {},
    fetch: () => {},
    WebSocket: () => {},
    keep: 42,
    alsoKeep: "present",
  };
  const surface = createSandboxGlobalSurface(source);
  assert.equal(surface.keep, 42);
  assert.equal(surface.alsoKeep, "present");
  assert.ok(!("require" in surface));
  assert.ok(!("process" in surface));
  assert.ok(!("fetch" in surface));
});

test("captureScopedGlobals and restoreScopedGlobals round-trip existing and absent names", () => {
  const container = { keep: "original" };
  const snapshot = captureScopedGlobals(["keep", "missing"], container);
  container.keep = "mutated";
  container.missing = "leaked";
  restoreScopedGlobals(snapshot, container);
  assert.equal(container.keep, "original");
  assert.ok(!("missing" in container));
});

test("assertWorkspaceEmpty rejects when entries exist", () => {
  const moduleStub = {
    FS: { readdir: () => [".", "..", "leftover.txt"] },
  };
  assert.throws(() => assertWorkspaceEmpty(moduleStub), /leftover\.txt/);
});

test("assertWorkspaceEmpty accepts a directory containing only FS sentinels", () => {
  const moduleStub = {
    FS: { readdir: () => [".", ".."] },
  };
  assertWorkspaceEmpty(moduleStub);
});

test("buildEntropyContract is stable and deterministic", () => {
  assert.deepEqual(buildEntropyContract(), buildEntropyContract());
});

test("captureJsRefManifest pins the loader entrypoints + pyodide version", () => {
  const refs = captureJsRefManifest({ assetDir: "/a", pyodideVersion: "0.29.3" });
  const kinds = refs.map((r) => r.kind);
  assert.deepEqual(kinds, ["loader-entrypoint", "loader-entrypoint", "pyodide-version"]);
  assert.ok(refs.some((r) => r.id === "pyodide.asm.js" && r.path === "/a/pyodide.asm.js"));
  assert.ok(refs.some((r) => r.id === "0.29.3"));
});

test("assertSerializableJsRefs rejects non-array or empty manifests", () => {
  assert.throws(() => assertSerializableJsRefs(null), /deterministic descriptors/);
  assert.throws(() => assertSerializableJsRefs([]), /deterministic descriptors/);
});

test("assertSerializableJsRefs rejects entries with non-scalar values", () => {
  assert.throws(
    () => assertSerializableJsRefs([{ kind: "x", id: "y", oops: () => {} }]),
    /deterministic scalar values/,
  );
});

test("assertSerializableJsRefs rejects entries missing string kind/id", () => {
  assert.throws(
    () => assertSerializableJsRefs([{ kind: 3, id: "y" }]),
    /string kind\/id fields/,
  );
  assert.throws(
    () => assertSerializableJsRefs([{ kind: "x", id: null }]),
    /string kind\/id fields/,
  );
});

test("assertSerializableJsRefs accepts a well-formed manifest and returns it unchanged", () => {
  const refs = captureJsRefManifest({ assetDir: "/a", pyodideVersion: "0.29.3" });
  assert.strictEqual(assertSerializableJsRefs(refs), refs);
});

test("hashJsRefs is stable for equivalent inputs and matches digestJson", () => {
  const refs = captureJsRefManifest({ assetDir: "/a", pyodideVersion: "1.2.3" });
  assert.equal(hashJsRefs(refs), digestJson(refs));
  assert.equal(hashJsRefs(refs), hashJsRefs(captureJsRefManifest({ assetDir: "/a", pyodideVersion: "1.2.3" })));
});

test("assertSerializableJsRefs rejects non-object entries", () => {
  assert.throws(() => assertSerializableJsRefs(["plain-string"]), /plain objects/);
});
