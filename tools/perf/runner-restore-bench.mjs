import { basename } from "node:path";
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";

import {
  assertRestoreThresholds,
  runRestoreBenchmark,
} from "../runner-node/bench/restore-benchmark.mjs";

function formatMs(value) {
  return `${value.toFixed(2)} ms`;
}

function formatMiBFromBytes(value) {
  return `${(value / 1024 / 1024).toFixed(2)} MiB`;
}

function formatRate(value) {
  return `${value.toFixed(2)} sess/s`;
}

function printMetricTable(title, metrics, formatValue = formatMs) {
  console.log(`\n${title}`);
  console.log("metric                    p50          p95          mean         min          max          n");
  for (const [name, summary] of Object.entries(metrics)) {
    console.log(
      `${name.padEnd(24)} ${formatValue(summary.p50, name).padEnd(12)} ${formatValue(summary.p95, name).padEnd(12)} ${formatValue(summary.mean, name).padEnd(12)} ${formatValue(summary.min, name).padEnd(12)} ${formatValue(summary.max, name).padEnd(12)} ${String(summary.count).padStart(3)}`,
    );
  }
}

function printSummary(report, outputPath) {
  console.log("Runner Restore Benchmark");
  console.log(`report: ${outputPath}`);
  console.log(`samples (n): ${report.samples}`);
  console.log(`concurrency (c): ${report.concurrency}`);
  console.log(`total sessions: ${report.totalSessions}`);
  printMetricTable("Per-session metrics", report.metrics);
  printMetricTable("Per-batch load metrics", report.batchMetrics, (value, name) =>
    name === "throughputSessionsPerSec" ? formatRate(value) : formatMs(value),
  );
  console.log("\nMemory");
  console.log(`process baseline RSS: ${formatMiBFromBytes(report.memory.processBaselineRssBytes)}`);
  console.log(`runner baseline RSS:  ${formatMiBFromBytes(report.memory.runnerBaselineRssBytes)}`);
  console.log(`peak RSS:             ${formatMiBFromBytes(report.memory.peakRssBytes)}`);
  console.log(`post-close RSS:       ${formatMiBFromBytes(report.memory.finalRssBytes)}`);
  console.log(`retained RSS:         ${formatMiBFromBytes(report.memory.retainedRssBytes)}`);
  console.log(`avg active sessions:  ${report.memory.averageActiveSessions.toFixed(2)}`);
  console.log(`avg RSS (whole run):  ${formatMiBFromBytes(report.memory.averageRssBytes)}`);
  console.log(`avg RSS when busy:    ${formatMiBFromBytes(report.memory.averageRssBytesWhenBusy)}`);
  console.log(`avg above runner:     ${formatMiBFromBytes(report.memory.averageAboveRunnerBaselineRssBytesWhenBusy)}`);
  console.log(`avg per active sess:  ${formatMiBFromBytes(report.memory.averagePerActiveSessionRssBytesWhenBusy)}`);
  console.log(`peak above runner:    ${formatMiBFromBytes(report.memory.peakAboveRunnerBaselineRssBytes)}`);
  console.log(`est. per active sess: ${formatMiBFromBytes(report.memory.estimatedPerActiveSessionRssBytes)}`);
}

function parseArgs(argv) {
  const options = {
    samples: 2000,
    warmup: 1,
    concurrency: 100,
  };

  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    const next = argv[index + 1];
    if ((arg === "--samples" || arg === "-n") && next) {
      options.samples = Number(next);
      index += 1;
    } else if ((arg === "--concurrency" || arg === "-c") && next) {
      options.concurrency = Number(next);
      index += 1;
    } else if (arg === "--warmup" && next) {
      options.warmup = Number(next);
      index += 1;
    } else if (arg === "--output" && next) {
      options.output = next;
      index += 1;
    } else if (arg === "--max-restore-p95-ms" && next) {
      options.thresholds ??= {};
      options.thresholds.restoreMs = Number(next);
      index += 1;
    } else if (arg === "--help") {
      options.help = true;
    } else {
      throw new Error(`unknown argument: ${arg}`);
    }
  }

  return options;
}

function defaultReportPath(filename = "runner-restore-bench.json") {
  return resolve(process.cwd(), "artifacts", "perf", filename);
}

function writeReport(report, outputPath) {
  mkdirSync(dirname(outputPath), { recursive: true });
  writeFileSync(outputPath, `${JSON.stringify(report, null, 2)}\n`, "utf-8");
  return outputPath;
}

function printHelp() {
  console.log(`Usage: node tools/perf/${basename(import.meta.url)} [options]

Options:
  --samples N                   Number of measured batches (default: 2000)
  --concurrency N               Number of parallel session restores per batch (default: 100)
  --warmup N                    Number of warmup batches to discard (default: 1)
  --output PATH                 Write JSON report to PATH
  --max-restore-p95-ms N        Fail if restoreMs p95 exceeds N

Short aliases:
  -n N                          Same as --samples
  -c N                          Same as --concurrency
`);
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  if (options.help) {
    printHelp();
    return;
  }

  const report = await runRestoreBenchmark({
    warmup: options.warmup,
    samples: options.samples,
    concurrency: options.concurrency,
  });
  const outputPath = writeReport(report, options.output ?? defaultReportPath());

  printSummary(report, outputPath);

  if (options.thresholds) {
    assertRestoreThresholds(report, options.thresholds);
  }
}

main().catch((error) => {
  process.stderr.write(`${error instanceof Error ? error.stack ?? error.message : String(error)}\n`);
  process.exit(1);
});
