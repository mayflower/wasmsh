/**
 * LLM-driven data analysis test using the wasmsh sandbox.
 *
 * An Anthropic model drives a tool-use loop, writing and executing Python
 * and shell commands through createNodeSession to analyze a CSV file.
 *
 * Skip: SKIP_PYODIDE=1, missing assets, or no ANTHROPIC_API_KEY
 */
import { describe, it, after } from "node:test";
import { existsSync } from "node:fs";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";

const __dirname = dirname(fileURLToPath(import.meta.url));
const PKG_DIR = resolve(__dirname, "../../../packages/npm/wasmsh-pyodide");
const ASSETS_DIR = resolve(PKG_DIR, "assets");

const SKIP =
  process.env.SKIP_PYODIDE === "1" ||
  !existsSync(resolve(ASSETS_DIR, "pyodide.asm.wasm")) ||
  !process.env.ANTHROPIC_API_KEY;

let createNodeSession;
let Anthropic;
if (!SKIP) {
  ({ createNodeSession } = await import(resolve(PKG_DIR, "index.js")));
  ({ default: Anthropic } = await import("@anthropic-ai/sdk"));
}

const CSV_DATA =
  "city,temp_c\nTokyo,22\nBerlin,15\nCairo,35\nSydney,28\nOslo,5\n";

const MODEL = "claude-haiku-4-5-20251001";
const MAX_TURNS = 15;

const EXECUTE_TOOL = {
  name: "execute",
  description:
    "Execute a shell command in the sandbox. Bash and python3 are available.",
  input_schema: {
    type: "object",
    properties: {
      command: { type: "string", description: "The shell command to run" },
    },
    required: ["command"],
  },
};

const PROMPT =
  "I have a CSV file at /workspace/temps.csv with city temperature data. " +
  "Write a Python script to /workspace/analyze.py that reads the CSV using the csv module, " +
  "calculates the average temperature across all cities, finds the city with the highest " +
  "temperature, and writes a JSON file to /workspace/analysis.json with keys: " +
  '"average" (the computed average as a number), "hottest_city" (city name string), ' +
  'and "hottest_temp" (the highest temperature as a number). ' +
  "Execute the script with python3 /workspace/analyze.py. " +
  "Then cat /workspace/analysis.json to verify the output.";

describe("LLM agent data analysis via wasmsh sandbox", () => {
  const sessions = [];
  after(async () => {
    for (const s of sessions) {
      try {
        await s.close();
      } catch {
        /* ignore */
      }
    }
  });

  it("analyzes CSV with Python and verifies with shell", {
    skip: SKIP ? "requires Pyodide assets and ANTHROPIC_API_KEY" : false,
    timeout: 120_000,
  }, async () => {
    const session = await createNodeSession({ assetDir: ASSETS_DIR });
    sessions.push(session);

    // Seed the CSV file
    await session.writeFile(
      "/workspace/temps.csv",
      new TextEncoder().encode(CSV_DATA),
    );

    const client = new Anthropic();
    const messages = [{ role: "user", content: PROMPT }];

    let usedPython = false;
    let usedShell = false;

    for (let turn = 0; turn < MAX_TURNS; turn++) {
      const response = await client.messages.create({
        model: MODEL,
        max_tokens: 4096,
        tools: [EXECUTE_TOOL],
        messages,
      });

      messages.push({ role: "assistant", content: response.content });

      if (response.stop_reason === "end_turn") break;
      if (response.stop_reason !== "tool_use") break;

      const toolResults = [];
      for (const block of response.content) {
        if (block.type !== "tool_use") continue;

        const command = block.input.command;
        if (/python3?\b/.test(command)) usedPython = true;
        if (!/^python3?\b/.test(command)) usedShell = true;

        const result = await session.run(command);
        const output =
          (result.stdout || "") +
          (result.stderr ? `\nSTDERR: ${result.stderr}` : "");

        toolResults.push({
          type: "tool_result",
          tool_use_id: block.id,
          content: output || "(no output)",
        });
      }
      messages.push({ role: "user", content: toolResults });
    }

    // Verify the agent used both Python and shell
    assert.ok(usedPython, "Agent should have used Python");
    assert.ok(usedShell, "Agent should have used shell commands");

    // Verify the output file
    const catResult = await session.run("cat /workspace/analysis.json");
    assert.equal(catResult.exitCode, 0, "cat analysis.json should succeed");

    const analysis = JSON.parse(catResult.stdout.trim());
    assert.equal(analysis.average, 21, "Average should be 21");
    assert.equal(analysis.hottest_city, "Cairo", "Hottest city should be Cairo");
    assert.equal(analysis.hottest_temp, 35, "Hottest temp should be 35");
  });
});
