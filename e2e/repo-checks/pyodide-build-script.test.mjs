/**
 * Repo-level invariants for `tools/pyodide/build-custom.sh`.
 *
 * These tests are zero-build and run in milliseconds.  They exist to
 * catch a specific class of regression that has already cost us two
 * broken releases (v0.5.7, v0.5.8):
 *
 *   1. Someone reintroduces `MAIN_MODULE=2` or `-Wno-error=undefined`
 *      in an attempt to dodge a Rust legacy-mangling issue, silently
 *      dropping symbols so compiled Python side modules (numpy, pandas,
 *      scipy, …) fail to import at runtime.
 *   2. The emscripten.py patch section loses one of its two call-site
 *      fixes (the v0.5.8 bug).  Both `unexpected_exports` AND
 *      `function_exports` / `global_exports` must be filtered — without
 *      the second, acorn-optimizer later fails to parse the generated
 *      `pyodide.asm.js`.
 *
 * The checks are intentionally strict: they read the build script as
 * text and assert presence / absence of specific markers.  They do NOT
 * validate that the markers appear in the right order or block — that's
 * what the full build + E2E suite is for.  These guards exist to catch
 * the most likely "commit and forget to run the full build" mistake in
 * zero seconds.
 */
import { describe, it } from "node:test";
import { readFileSync } from "node:fs";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";

const __dirname = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(__dirname, "../..");
const BUILD_SCRIPT = resolve(REPO, "tools/pyodide/build-custom.sh");
const script = readFileSync(BUILD_SCRIPT, "utf-8");

describe("tools/pyodide/build-custom.sh invariants", () => {
  // ── Forbidden approaches (regressions of v0.5.7) ─────────────

  it("does not set MAIN_MODULE=2 in Makefile.envs patches", () => {
    // v0.5.7 switched to MAIN_MODULE=2 to dodge Rust legacy mangling.
    // That silently dropped ~8000 CPython / libstdc++ symbols from the
    // wasm, making every compiled Python side module fail to import.
    // If you need to sed something that contains the literal text
    // "MAIN_MODULE=2" (e.g. a comment explaining why we don't use it),
    // that's fine — but a real `-s MAIN_MODULE=2` flag is not.
    const lines = script.split("\n");
    const offending = lines
      .map((line, i) => ({ line, i: i + 1 }))
      .filter(({ line }) => {
        if (line.trim().startsWith("#")) return false; // comment line
        return /-s\s*MAIN_MODULE\s*=\s*2/.test(line) ||
          /\bMAIN_MODULE=2\b/.test(line);
      });
    assert.equal(
      offending.length,
      0,
      "MAIN_MODULE=2 reintroduced in build-custom.sh:\n" +
        offending.map((o) => `  line ${o.i}: ${o.line}`).join("\n") +
        "\nSee v0.5.7 regression history in commit log before changing this.",
    );
  });

  it("does not use -Wno-error=undefined to silence missing symbols", () => {
    // v0.5.7 used `-Wno-error=undefined` to turn missing-export errors
    // into warnings, which meant a downloaded CDN export list that
    // referenced symbols our build didn't link silently dropped them.
    // Runtime dlopen of numpy.so then died with "bad export type".
    const lines = script.split("\n");
    const offending = lines
      .map((line, i) => ({ line, i: i + 1 }))
      .filter(({ line }) => {
        if (line.trim().startsWith("#")) return false;
        return /-Wno-error=undefined/.test(line);
      });
    assert.equal(
      offending.length,
      0,
      "-Wno-error=undefined reintroduced in build-custom.sh:\n" +
        offending.map((o) => `  line ${o.i}: ${o.line}`).join("\n") +
        "\nThis silently drops required exports; fix the real symbol issue instead.",
    );
  });

  it("does not download a prebuilt export list from cdn.jsdelivr.net", () => {
    // v0.5.7 generated EXPORTED_FUNCTIONS by extracting symbols from
    // the standard Pyodide CDN wasm.  That list contains symbols our
    // custom build does not link, which combined with the previous two
    // anti-patterns produced a broken wasm.
    const lines = script.split("\n");
    const offending = lines
      .map((line, i) => ({ line, i: i + 1 }))
      .filter(({ line }) => {
        if (line.trim().startsWith("#")) return false;
        return /extract-standard-exports|standard-exports-.*\.cache/.test(line);
      });
    assert.equal(
      offending.length,
      0,
      "CDN export list extraction reintroduced:\n" +
        offending.map((o) => `  line ${o.i}: ${o.line}`).join("\n"),
    );
  });

  // ── Required patches (both halves of the v0.5.8 fix) ────────

  it("patches the unexpected_exports call site in emscripten.py", () => {
    assert.ok(
      script.includes("unexpected_exports = [e for e in unexpected_exports if e.isidentifier()]"),
      "build-custom.sh no longer patches unexpected_exports in emscripten.py.\n" +
        "Without this, em++ errors with `invalid export name: _ZN..$..E` when\n" +
        "compiling Pyodide with a Rust staticlib linked in.",
    );
  });

  it("patches the function_exports + global_exports call site in emscripten.py", () => {
    // This is THE thing that broke the v0.5.8 release: my build script
    // had only the first patch.  CI generated pyodide.asm.js with
    // `var __ZN..$..E = wasmExports[..]` declarations that later failed
    // parsing in acorn-optimizer.mjs with "Unexpected token".
    assert.ok(
      script.includes("function_exports = {k: v for k, v in function_exports.items() if k.isidentifier()}"),
      "build-custom.sh no longer patches function_exports in emscripten.py.\n" +
        "Without this, the generated pyodide.asm.js fails to parse in\n" +
        "acorn-optimizer.mjs with SyntaxError on Rust legacy-mangled symbols.\n" +
        "This was the root cause of the v0.5.8 release failure.",
    );
    assert.ok(
      script.includes("global_exports = {k: v for k, v in global_exports.items() if k.isidentifier()}"),
      "build-custom.sh patches function_exports but not global_exports in emscripten.py.\n" +
        "Both filters must be present.",
    );
  });

  it("keeps MAIN_MODULE=1 (upstream Pyodide default)", () => {
    // Positive assertion: MAIN_MODULE=1 is mentioned as the chosen
    // approach in a running line (not only inside a patched-away
    // example string).  This is a weak check — just a smoke test
    // that someone didn't silently remove the load-bearing comment.
    assert.ok(
      /MAIN_MODULE=1/.test(script),
      "build-custom.sh no longer references MAIN_MODULE=1.",
    );
  });

  it("applies per-patch idempotency inside the emscripten.py patch", () => {
    // The script must NOT use a single bash-level `grep -q` guard that
    // covers both patches, because a half-patched file (patch 1 applied
    // from an older script, patch 2 not yet) will silently short-circuit
    // the second patch.  This was the v0.5.8 root cause.
    //
    // We detect the safe pattern by requiring the `apply_patch` helper
    // function (or equivalent tri-state per-patch logic) to be present.
    assert.ok(
      script.includes("def apply_patch(") ||
        script.includes("# Patch 1:") && script.includes("# Patch 2:") &&
          script.includes("if new in src:"),
      "emscripten.py patch section is missing per-patch idempotency.\n" +
        "Do not use a single bash-level `grep -q` guard to gate BOTH patches —\n" +
        "a half-patched file (e.g. patch 1 applied from an older script) will\n" +
        "silently skip patch 2 on re-run. This was the v0.5.8 release bug.",
    );
  });

  it("uses curl -fsSL (not -sSL) for CDN downloads", () => {
    // Without -f, curl writes HTML error pages to .json / .whl files
    // on HTTP errors, producing a "successful" but broken artifact.
    const lines = script.split("\n");
    const offending = lines
      .map((line, i) => ({ line, i: i + 1 }))
      .filter(({ line }) => {
        if (line.trim().startsWith("#")) return false;
        return /\bcurl\b/.test(line) && !/curl\s+-[a-zA-Z]*f/.test(line);
      });
    assert.equal(
      offending.length,
      0,
      "curl invocations missing -f flag in build-custom.sh:\n" +
        offending.map((o) => `  line ${o.i}: ${o.line}`).join("\n") +
        "\nWithout -f, HTTP 4xx/5xx error pages are silently written to the\n" +
        "output file as if successful.",
    );
  });
});
