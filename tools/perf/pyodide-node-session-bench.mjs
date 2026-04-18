import { basename } from "node:path";

import {
  assertThresholds,
  defaultReportPath,
  runSessionBenchmark,
  writeBenchmarkReport,
} from "../../e2e/pyodide-node/lib/perf-harness.mjs";

function formatMs(value) {
  return `${value.toFixed(2)} ms`;
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
  console.log("Pyodide Node Session Benchmark");
  console.log(`report: ${outputPath}`);
  console.log(`samples (n): ${report.samples}`);
  console.log(`concurrency (c): ${report.concurrency}`);
  console.log(`total sessions: ${report.totalSessions}`);
  printMetricTable("Per-session phases", report.metrics);
  printMetricTable("Per-batch metrics", report.batchMetrics, (value, name) =>
    name === "throughputSessionsPerSec" ? formatRate(value) : formatMs(value),
  );
}

function parseArgs(argv) {
  const options = {
    samples: 5,
    warmup: 1,
    concurrency: 1,
  };

  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    const next = argv[i + 1];
    if ((arg === "--samples" || arg === "-n") && next) {
      options.samples = Number(next);
      i += 1;
    } else if ((arg === "--concurrency" || arg === "-c") && next) {
      options.concurrency = Number(next);
      i += 1;
    } else if (arg === "--warmup" && next) {
      options.warmup = Number(next);
      i += 1;
    } else if (arg === "--output" && next) {
      options.output = next;
      i += 1;
    } else if (arg === "--node" && next) {
      options.nodeExecutable = next;
      i += 1;
    } else if (arg === "--max-init-p95-ms" && next) {
      options.thresholds ??= {};
      options.thresholds.initMs = Number(next);
      i += 1;
    } else if (arg === "--max-first-python-p95-ms" && next) {
      options.thresholds ??= {};
      options.thresholds.firstPythonMs = Number(next);
      i += 1;
    } else if (arg === "--max-total-p95-ms" && next) {
      options.thresholds ??= {};
      options.thresholds.totalMs = Number(next);
      i += 1;
    } else if (arg === "--help") {
      options.help = true;
    } else {
      throw new Error(`unknown argument: ${arg}`);
    }
  }

  return options;
}

function printHelp() {
  console.log(`Usage: node tools/perf/${basename(import.meta.url)} [options]

Options:
  --samples N                   Number of measured batches (default: 5)
  --concurrency N               Number of parallel sessions per batch (default: 1)
  --warmup N                    Number of warmup batches to discard (default: 1)
  --output PATH                 Write JSON report to PATH
  --node PATH                   Override Node executable for the host child
  --max-init-p95-ms N           Fail if initMs p95 exceeds N
  --max-first-python-p95-ms N   Fail if firstPythonMs p95 exceeds N
  --max-total-p95-ms N          Fail if totalMs p95 exceeds N

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

  const report = await runSessionBenchmark(options);
  const outputPath = writeBenchmarkReport(report, options.output ?? defaultReportPath());

  printSummary(report, outputPath);

  if (options.thresholds) {
    assertThresholds(report, options.thresholds);
  }
}

main().catch((error) => {
  process.stderr.write(`${error instanceof Error ? error.stack ?? error.message : String(error)}\n`);
  process.exit(1);
});
