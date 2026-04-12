#!/usr/bin/env node
/**
 * wasmsh Agent Harness — LLM-generated random task runner.
 *
 * Usage:
 *   node run.mjs --count 10                     # 10 tasks, default concurrency (8)
 *   node run.mjs --count 100 --concurrency 12   # 100 tasks, 12 parallel sandboxes
 *   node run.mjs --count 20 --category python   # 20 Python-only tasks
 *   node run.mjs --bugs                         # Show accumulated sandbox bugs
 *   node run.mjs --summary                      # Show last run results
 *
 * Requires: ANTHROPIC_API_KEY environment variable (or .env file)
 */

import "dotenv/config";
import pLimit from "p-limit";
import { generateTasks, CATEGORIES } from "./lib/generate.mjs";
import { executeTask } from "./lib/execute.mjs";
import { diagnoseFailure } from "./lib/diagnose.mjs";
import { createRecorder, printBugs, printSummaryFromFile } from "./lib/recorder.mjs";
import { readdirSync } from "node:fs";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));

/** Max tasks per generation call (avoids token-limit truncation). */
const GENERATION_BATCH_SIZE = 5;

function parseArgs() {
  const args = process.argv.slice(2);
  const opts = { count: 5, category: null, concurrency: 8, bugs: false, summary: false };

  for (let i = 0; i < args.length; i++) {
    switch (args[i]) {
      case "--count":
        opts.count = parseInt(args[++i], 10);
        break;
      case "--category":
        opts.category = args[++i];
        break;
      case "--concurrency":
        opts.concurrency = parseInt(args[++i], 10);
        break;
      case "--bugs":
        opts.bugs = true;
        break;
      case "--summary":
        opts.summary = true;
        break;
      case "--help":
        console.log(
          "Usage: node run.mjs [--count N] [--category CAT] [--concurrency N] [--bugs] [--summary]",
        );
        console.log(`Categories: ${CATEGORIES.join(", ")}`);
        console.log("Default concurrency: 8 (each sandbox uses ~150 MB)");
        process.exit(0);
    }
  }
  return opts;
}

function padRight(s, n) {
  return s.length >= n ? s.slice(0, n) : s + " ".repeat(n - s.length);
}

// ---------------------------------------------------------------------------
// Task generation (batched + parallel)
// ---------------------------------------------------------------------------

/**
 * Generate `count` tasks, splitting into parallel batches of
 * GENERATION_BATCH_SIZE to avoid token-limit truncation.
 */
async function generateAllTasks(count, options) {
  if (count <= GENERATION_BATCH_SIZE) {
    return generateTasks(count, options);
  }

  const batchCount = Math.ceil(count / GENERATION_BATCH_SIZE);
  const batchSizes = [];
  let remaining = count;
  for (let i = 0; i < batchCount; i++) {
    const size = Math.min(remaining, GENERATION_BATCH_SIZE);
    batchSizes.push(size);
    remaining -= size;
  }

  console.log(
    `  Splitting into ${batchSizes.length} generation batches ` +
    `(${batchSizes.join(", ")} tasks each)...`,
  );

  const batches = await Promise.all(
    batchSizes.map((size) => generateTasks(size, options)),
  );

  return batches.flat();
}

// ---------------------------------------------------------------------------
// Task execution (parallel with concurrency limit)
// ---------------------------------------------------------------------------

async function runAllTasks(tasks, concurrency, recorder) {
  const limit = pLimit(concurrency);
  const total = tasks.length;

  // Counters for the live progress line.
  let completed = 0;
  let passed = 0;
  let failed = 0;

  // We print a progress line after each completion, but since tasks
  // finish out of order, we buffer the full results and print a
  // sorted table at the end.  The live output shows a counter.
  const results = new Array(total);

  const promises = tasks.map((task, i) =>
    limit(async () => {
      const result = await executeTask(task);

      let diagnosis = null;
      if (!result.passed) {
        diagnosis = await diagnoseFailure(task, result);
      }

      const entry = {
        id: task.id,
        category: task.category,
        description: task.description,
        seed_files: task.seed_files,
        verification: task.verification,
        passed: result.passed,
        diagnosis,
        verification_stdout: result.verification.stdout,
        verification_exit_code: result.verification.exitCode,
        tool_trace: (result.toolTrace || []).map((t) => ({
          tool: t.tool,
          input: typeof t.input === "string" ? t.input : JSON.stringify(t.input),
          output: (t.output || "").slice(0, 1000),
        })),
        files_after: result.filesAfter,
        duration_ms: result.duration_ms,
        error: result.error,
        timestamp: new Date().toISOString(),
      };

      recorder.record(entry);
      results[i] = { task, result, diagnosis, entry };

      completed++;
      if (result.passed) passed++;
      else failed++;

      // Live progress line — overwrite in-place.
      const pct = ((completed / total) * 100).toFixed(0);
      process.stdout.write(
        `\r  [${completed}/${total}] ${pct}% — ` +
        `${passed} passed, ${failed} failed` +
        `    `,
      );
    }),
  );

  await Promise.all(promises);
  process.stdout.write("\n\n");

  return results;
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

function printDetailedResults(results) {
  for (let i = 0; i < results.length; i++) {
    const { task, result, diagnosis } = results[i];
    const prefix = `[${i + 1}/${results.length}]`;
    const label = `${task.category}: ${task.id}`;

    if (result.passed) {
      console.log(
        `${prefix} ${padRight(label, 45)} PASS (${(result.duration_ms / 1000).toFixed(1)}s)`,
      );
    } else {
      console.log(
        `${prefix} ${padRight(label, 45)} FAIL (${(result.duration_ms / 1000).toFixed(1)}s)`,
      );
      if (diagnosis) {
        console.log(`    -> ${diagnosis.classification}: ${diagnosis.reason}`);
        if (diagnosis.failed_command) {
          console.log(`    -> command: ${diagnosis.failed_command}`);
        }
        if (diagnosis.wasmsh_component) {
          console.log(`    -> component: ${diagnosis.wasmsh_component}`);
        }
      }
    }
  }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

async function main() {
  const opts = parseArgs();

  if (opts.bugs) {
    printBugs();
    return;
  }

  if (opts.summary) {
    const resultsDir = resolve(__dirname, "results");
    try {
      const files = readdirSync(resultsDir)
        .filter((f) => f.endsWith(".jsonl"))
        .sort()
        .reverse();
      if (files.length === 0) {
        console.log("No results found.");
      } else {
        printSummaryFromFile(resolve(resultsDir, files[0]));
      }
    } catch {
      console.log("No results found.");
    }
    return;
  }

  if (!process.env.ANTHROPIC_API_KEY) {
    console.error("Error: ANTHROPIC_API_KEY environment variable required.");
    process.exit(1);
  }

  const startTime = Date.now();

  console.log(`Generating ${opts.count} random tasks...`);
  const tasks = await generateAllTasks(opts.count, {
    category: opts.category,
  });
  console.log(`Generated ${tasks.length} tasks.\n`);

  console.log(
    `Running with concurrency=${opts.concurrency} ` +
    `(${tasks.length} tasks across ${Math.min(opts.concurrency, tasks.length)} parallel sandboxes)\n`,
  );

  const recorder = createRecorder();
  const results = await runAllTasks(tasks, opts.concurrency, recorder);

  printDetailedResults(results);
  recorder.printSummary();

  const elapsed = ((Date.now() - startTime) / 1000).toFixed(1);
  console.log(`\nTotal wall time: ${elapsed}s`);
}

main().catch((err) => {
  console.error("Fatal:", err.message || err);
  process.exit(1);
});
