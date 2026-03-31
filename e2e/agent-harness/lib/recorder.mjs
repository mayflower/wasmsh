/**
 * Result recording — JSONL output and console summary.
 */
import { appendFileSync, mkdirSync, readFileSync, readdirSync, existsSync } from "node:fs";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const RESULTS_DIR = resolve(__dirname, "../results");
const FAILURES_DIR = resolve(__dirname, "../failures");
const BUGS_FILE = resolve(FAILURES_DIR, "sandbox-bugs.jsonl");

export function createRecorder() {
  mkdirSync(RESULTS_DIR, { recursive: true });
  mkdirSync(FAILURES_DIR, { recursive: true });

  const timestamp = new Date().toISOString().replace(/[:.]/g, "-");
  const resultsFile = resolve(RESULTS_DIR, `${timestamp}.jsonl`);
  const entries = [];

  return {
    record(entry) {
      entries.push(entry);
      appendFileSync(resultsFile, JSON.stringify(entry) + "\n");

      // Append sandbox bugs to persistent file
      if (entry.diagnosis?.classification === "sandbox_bug") {
        appendFileSync(BUGS_FILE, JSON.stringify(entry) + "\n");
      }
    },

    get resultsFile() {
      return resultsFile;
    },

    printSummary() {
      const passed = entries.filter((e) => e.passed).length;
      const failed = entries.filter((e) => !e.passed).length;
      const bugs = entries.filter(
        (e) => e.diagnosis?.classification === "sandbox_bug",
      ).length;
      const llmErrors = entries.filter(
        (e) => e.diagnosis?.classification === "llm_mistake",
      ).length;
      const testIssues = entries.filter(
        (e) => e.diagnosis?.classification === "test_issue",
      ).length;
      const timeouts = entries.filter(
        (e) => e.diagnosis?.classification === "timeout",
      ).length;

      console.log("");
      console.log(
        `Results: ${passed}/${entries.length} passed, ${failed} failed`,
      );
      if (bugs > 0) console.log(`  sandbox bugs: ${bugs}`);
      if (llmErrors > 0) console.log(`  llm mistakes: ${llmErrors}`);
      if (testIssues > 0) console.log(`  test issues:  ${testIssues}`);
      if (timeouts > 0) console.log(`  timeouts:     ${timeouts}`);
      console.log(`Written to: ${resultsFile}`);
      if (bugs > 0) {
        console.log(`Sandbox bugs appended to: ${BUGS_FILE}`);
      }
    },
  };
}

export function printBugs() {
  if (!existsSync(BUGS_FILE)) {
    console.log("No sandbox bugs recorded yet.");
    return;
  }
  const lines = readFileSync(BUGS_FILE, "utf-8").trim().split("\n").filter(Boolean);
  if (lines.length === 0) {
    console.log("No sandbox bugs recorded yet.");
    return;
  }
  console.log(`\n${lines.length} sandbox bug(s) recorded:\n`);
  for (const line of lines) {
    const entry = JSON.parse(line);
    const d = entry.diagnosis;
    console.log(
      `  [${entry.category}] ${entry.id}: ${d.reason}` +
        (d.wasmsh_component ? ` (${d.wasmsh_component})` : ""),
    );
  }
}

export function printSummaryFromFile(file) {
  if (!existsSync(file)) {
    console.log(`File not found: ${file}`);
    return;
  }
  const lines = readFileSync(file, "utf-8").trim().split("\n").filter(Boolean);
  const entries = lines.map((l) => JSON.parse(l));
  const passed = entries.filter((e) => e.passed).length;
  console.log(`\n${entries.length} tasks, ${passed} passed:\n`);
  for (const e of entries) {
    const status = e.passed ? "PASS" : "FAIL";
    const diag = e.diagnosis ? ` -> ${e.diagnosis.classification}: ${e.diagnosis.reason}` : "";
    console.log(`  [${status}] ${e.category}/${e.id}: ${e.description.slice(0, 80)}${diag}`);
  }
}

export function getLatestResultsFile() {
  if (!existsSync(RESULTS_DIR)) return null;
  const files = readdirSync(RESULTS_DIR)
    .filter((f) => f.endsWith(".jsonl"))
    .sort()
    .reverse();
  return files.length > 0 ? resolve(RESULTS_DIR, files[0]) : null;
}
