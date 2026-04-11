#!/usr/bin/env node
/**
 * wasmsh Agent Harness — LLM-generated random task runner.
 *
 * Usage:
 *   node run.mjs --count 10              # Generate and run 10 random tasks
 *   node run.mjs --count 5 --category python  # Only Python tasks
 *   node run.mjs --bugs                  # Show accumulated sandbox bugs
 *   node run.mjs --summary               # Show last run results
 *
 * Requires: ANTHROPIC_API_KEY environment variable
 */

import "dotenv/config";
import { generateTasks, CATEGORIES } from "./lib/generate.mjs";
import { executeTask } from "./lib/execute.mjs";
import { diagnoseFailure } from "./lib/diagnose.mjs";
import { createRecorder, printBugs, printSummaryFromFile } from "./lib/recorder.mjs";
import { readdirSync } from "node:fs";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));

function parseArgs() {
  const args = process.argv.slice(2);
  const opts = { count: 5, category: null, bugs: false, summary: false };

  for (let i = 0; i < args.length; i++) {
    switch (args[i]) {
      case "--count":
        opts.count = parseInt(args[++i], 10);
        break;
      case "--category":
        opts.category = args[++i];
        break;
      case "--bugs":
        opts.bugs = true;
        break;
      case "--summary":
        opts.summary = true;
        break;
      case "--help":
        console.log(
          "Usage: node run.mjs [--count N] [--category CAT] [--bugs] [--summary]",
        );
        console.log(`Categories: ${CATEGORIES.join(", ")}`);
        process.exit(0);
    }
  }
  return opts;
}

function padRight(s, n) {
  return s.length >= n ? s.slice(0, n) : s + " ".repeat(n - s.length);
}

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

  console.log(`Generating ${opts.count} random tasks...`);
  const tasks = await generateTasks(opts.count, { category: opts.category });
  console.log(`Generated ${tasks.length} tasks.\n`);

  const recorder = createRecorder();

  for (let i = 0; i < tasks.length; i++) {
    const task = tasks[i];
    const prefix = `[${i + 1}/${tasks.length}]`;
    const label = `${task.category}: ${task.id}`;
    process.stdout.write(`${prefix} ${padRight(label, 45)} `);

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

    if (result.passed) {
      console.log(`PASS (${(result.duration_ms / 1000).toFixed(1)}s)`);
    } else {
      console.log(`FAIL (${(result.duration_ms / 1000).toFixed(1)}s)`);
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

  recorder.printSummary();
}

main().catch((err) => {
  console.error("Fatal:", err.message || err);
  process.exit(1);
});
