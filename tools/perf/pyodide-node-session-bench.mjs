import { basename } from "node:path";

import {
  assertThresholds,
  defaultReportPath,
  runSessionBenchmark,
  writeBenchmarkReport,
} from "../../e2e/pyodide-node/lib/perf-harness.mjs";

function parseArgs(argv) {
  const options = {
    samples: 5,
    warmup: 1,
  };

  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    const next = argv[i + 1];
    if (arg === "--samples" && next) {
      options.samples = Number(next);
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
  --samples N                   Number of measured samples (default: 5)
  --warmup N                    Number of warmup samples to discard (default: 1)
  --output PATH                 Write JSON report to PATH
  --node PATH                   Override Node executable for the host child
  --max-init-p95-ms N           Fail if initMs p95 exceeds N
  --max-first-python-p95-ms N   Fail if firstPythonMs p95 exceeds N
  --max-total-p95-ms N          Fail if totalMs p95 exceeds N
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

  console.log(JSON.stringify({
    report: outputPath,
    metrics: report.metrics,
  }, null, 2));

  if (options.thresholds) {
    assertThresholds(report, options.thresholds);
  }
}

main().catch((error) => {
  process.stderr.write(`${error instanceof Error ? error.stack ?? error.message : String(error)}\n`);
  process.exit(1);
});
