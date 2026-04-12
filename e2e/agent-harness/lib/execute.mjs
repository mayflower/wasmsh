/**
 * Task execution — runs createDeepAgent with WasmshSandbox backend and verifies.
 *
 * The deepagent automatically gets all filesystem middleware tools
 * (execute, read_file, write_file, edit_file, ls, grep, glob) backed
 * by the wasmsh sandbox — shell + Python in WASM.
 */
import { createDeepAgent } from "deepagents";
import { HumanMessage } from "@langchain/core/messages";

const MODEL = "claude-sonnet-4-5-20250929";
const MAX_AGENT_TIMEOUT = 300_000; // 5 minutes per task

/** Categories that need network access for micropip package installation. */
const NETWORK_CATEGORIES = new Set(["python-packages", "combined"]);

/** Hosts required for micropip installs from the Pyodide CDN and PyPI. */
const PACKAGE_ALLOWED_HOSTS = [
  "cdn.jsdelivr.net",
  "pypi.org",
  "files.pythonhosted.org",
];

const SANDBOX_SYSTEM_PROMPT = `You are working in a wasmsh sandbox — a WASM-based shell environment with Python 3.13 via Pyodide.

Key facts:
- Pre-installed Python packages (just import, no install needed): pyyaml (import yaml), beautifulsoup4 (from bs4 import BeautifulSoup), sqlite3, micropip
- For other pure-Python packages: python -c "import micropip; await micropip.install('package_name')"
- Only pure-Python packages work (no numpy, pandas, scipy)
- Python stdlib is fully available (json, csv, re, os, pathlib, collections, itertools, sqlite3, xml, html, etc.)
- 88 shell utilities are available (grep, sed, awk, jq, find, tar, curl, etc.)
- tar uses standard flag syntax: tar -czf, tar -xzf, tar -tzf (always use dash before flags)
- No apt, npm, docker, git, ssh, pip command — use micropip for package installs
- All files should be under /workspace/`;

/**
 * Extract tool call trace from the agent's message history.
 * Returns an array of { tool, input, output } objects.
 */
function extractToolTrace(messages) {
  const trace = [];
  if (!messages) return trace;

  for (const msg of messages) {
    // AI messages with tool_calls
    if (msg.tool_calls?.length) {
      for (const tc of msg.tool_calls) {
        trace.push({
          tool: tc.name,
          input: tc.args,
          toolCallId: tc.id ?? null,
          output: null, // filled from the next ToolMessage
        });
      }
    }
    // ToolMessage — result of a tool call
    if (msg.constructor?.name === "ToolMessage" || msg.name) {
      // Match by tool_call_id first (correct for parallel calls),
      // fall back to first pending entry with no output.
      const toolCallId = msg.tool_call_id;
      let pending = toolCallId
        ? trace.find((t) => t.output === null && t.toolCallId === toolCallId)
        : null;
      if (!pending) {
        pending = trace.find((t) => t.output === null);
      }
      if (pending) {
        const content = typeof msg.content === "string"
          ? msg.content
          : JSON.stringify(msg.content);
        pending.output = content.slice(0, 2000); // cap for diagnosis prompt
      }
    }
  }
  return trace;
}

/**
 * Format tool trace as readable text for the diagnosis LLM.
 */
function formatToolTrace(trace) {
  if (trace.length === 0) return "(no tool calls recorded)";
  return trace
    .map((t, i) => {
      const input = typeof t.input === "string"
        ? t.input
        : JSON.stringify(t.input);
      const inputShort = input.slice(0, 500);
      const outputShort = (t.output || "(no output)").slice(0, 500);
      return `[${i + 1}] ${t.tool}(${inputShort})\n    → ${outputShort}`;
    })
    .join("\n");
}

export { formatToolTrace };

export async function executeTask(task) {
  const { createSandbox } = await import("./session.mjs");
  const needsNetwork = NETWORK_CATEGORIES.has(task.category);
  const sandbox = await createSandbox(
    needsNetwork ? { allowedHosts: PACKAGE_ALLOWED_HOSTS } : {},
  );
  const startTime = Date.now();
  const toolTrace = [];

  try {
    // Upload seed files
    for (const f of task.seed_files || []) {
      await sandbox.uploadFiles([
        [f.path, new TextEncoder().encode(f.content)],
      ]);
    }

    // Create the deepagent with wasmsh sandbox as the only backend.
    // Disable HumanMessage eviction — our tasks are small and the eviction
    // writes to the sandbox filesystem, interfering with workspace state.
    const agent = createDeepAgent({
      model: MODEL,
      backend: sandbox,
      systemPrompt: SANDBOX_SYSTEM_PROMPT,
      filesystemOptions: {
        humanMessageTokenLimitBeforeEvict: null,
      },
    });

    // Run with timeout
    const agentResult = await Promise.race([
      agent.invoke({ messages: [new HumanMessage(task.description)] }),
      new Promise((_, reject) =>
        setTimeout(() => reject(new Error("TIMEOUT")), MAX_AGENT_TIMEOUT),
      ),
    ]);

    // Extract full tool call trace from agent messages
    const messages = agentResult?.messages ?? [];
    toolTrace.push(...extractToolTrace(messages));

    // Collect workspace state after agent ran
    const lsResult = await sandbox.execute("find /workspace -type f 2>/dev/null | head -50");

    // Run verification
    const verify = await sandbox.execute(task.verification);
    const passed = verify.output.trim().startsWith("PASS");

    return {
      passed,
      verification: { stdout: verify.output, exitCode: verify.exitCode },
      toolTrace,
      filesAfter: lsResult.output.trim(),
      duration_ms: Date.now() - startTime,
      error: null,
    };
  } catch (err) {
    return {
      passed: false,
      verification: { stdout: "", exitCode: -1 },
      toolTrace,
      filesAfter: "",
      duration_ms: Date.now() - startTime,
      error: err.message === "TIMEOUT" ? "timeout" : err.message,
    };
  } finally {
    try { await sandbox.stop(); } catch { /* ignore */ }
  }
}
