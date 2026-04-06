/**
 * DeepAgents + wasmsh — Fully In-Browser Example
 *
 * Everything runs in the browser:
 * - wasmsh shell/Python execution via Web Worker
 * - LLM calls via Anthropic API (dangerouslyAllowBrowser)
 * - createDeepAgent orchestrates the agent with filesystem middleware
 *
 * No backend service involved.
 */
import { ChatAnthropic } from "@langchain/anthropic";
import { HumanMessage } from "@langchain/core/messages";
import { createDeepAgent, BaseSandbox } from "deepagents";

// ── Minimal browser sandbox ─────────────────────────────────
// Wraps the wasmsh browser Worker and implements the sandbox
// protocol that createDeepAgent expects.

function toBase64(bytes) {
  let binary = "";
  for (const b of bytes) binary += String.fromCharCode(b);
  return btoa(binary);
}

function fromBase64(text) {
  const binary = atob(text);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
  return bytes;
}

function getDiagnosticError(events) {
  if (!events) return undefined;
  for (const event of events) {
    if (event && typeof event === "object" && "Diagnostic" in event) {
      return event.Diagnostic[1];
    }
  }
  return undefined;
}

function mapError(msg) {
  const n = (msg || "").toLowerCase();
  if (n.includes("not found")) return "file_not_found";
  if (n.includes("directory")) return "is_directory";
  return "invalid_path";
}

class BrowserSandbox extends BaseSandbox {
  constructor(workerUrl, assetBaseUrl) {
    super();
    this.id = `wasmsh-browser-${Date.now()}`;
    this._workerUrl = workerUrl;
    this._assetBaseUrl = assetBaseUrl;
    this._nextId = 1;
    this._pending = new Map();
    this._session = null;
  }

  async initialize() {
    const worker = new Worker(this._workerUrl, { name: "wasmsh-pyodide" });
    this._session = worker;

    worker.addEventListener("message", (event) => {
      const entry = this._pending.get(event.data.id);
      if (!entry) return;
      this._pending.delete(event.data.id);
      if (event.data.ok) entry.resolve(event.data.result);
      else entry.reject(new Error(event.data.error));
    });

    worker.addEventListener("error", (event) => {
      const err = new Error(`Worker error: ${event.message || "unknown"}`);
      for (const entry of this._pending.values()) entry.reject(err);
      this._pending.clear();
    });

    await this._request("init", {
      assetBaseUrl: this._assetBaseUrl,
      stepBudget: 0,
      initialFiles: [],
      // Allow the agent to install Python packages via `pip install`.
      // wasmsh 0.5.5+ intercepts pip commands and routes them through
      // micropip.  Network installs need these hosts allowlisted.
      allowedHosts: [
        "cdn.jsdelivr.net",
        "pypi.org",
        "files.pythonhosted.org",
      ],
    });
  }

  _request(method, params) {
    const id = this._nextId++;
    return new Promise((resolve, reject) => {
      this._pending.set(id, { resolve, reject });
      this._session.postMessage({ id, method, params });
    });
  }

  async execute(command) {
    const result = await this._request("run", {
      command: `cd '/workspace' && ${command}`,
    });
    return {
      output: result.output || (result.stdout || "") + (result.stderr || ""),
      exitCode: result.exitCode ?? null,
      truncated: false,
    };
  }

  async uploadFiles(files) {
    const results = [];
    for (const [path, content] of files) {
      try {
        const result = await this._request("writeFile", {
          path,
          contentBase64: toBase64(content),
        });
        const diag = getDiagnosticError(result?.events);
        results.push({ path, error: diag ? mapError(diag) : null });
      } catch (e) {
        results.push({ path, error: mapError(e.message) });
      }
    }
    return results;
  }

  async downloadFiles(paths) {
    const results = [];
    for (const path of paths) {
      try {
        const result = await this._request("readFile", { path });
        const diag = getDiagnosticError(result.events);
        if (diag) {
          results.push({ path, content: null, error: mapError(diag) });
        } else {
          results.push({ path, content: fromBase64(result.contentBase64), error: null });
        }
      } catch (e) {
        results.push({ path, content: null, error: mapError(e.message) });
      }
    }
    return results;
  }

  async close() {
    if (this._session) {
      try { await this._request("close", {}); } catch { /* ignore */ }
      this._session.terminate();
      this._session = null;
    }
  }

  async stop() { return this.close(); }
}

// ── UI ──────────────────────────────────────────────────────

const statusEl = document.getElementById("status");
const outputEl = document.getElementById("output");
const runBtn = document.getElementById("runBtn");

function log(text) {
  outputEl.textContent += text + "\n";
}

window.runAgent = async function runAgent() {
  const apiKey = document.getElementById("apiKey").value.trim();
  const prompt = document.getElementById("prompt").value.trim();

  if (!apiKey) { alert("Enter your Anthropic API key"); return; }
  if (!prompt) { alert("Enter a task"); return; }

  runBtn.disabled = true;
  outputEl.textContent = "";

  try {
    statusEl.textContent = "Booting wasmsh sandbox (loading Pyodide WASM)...";
    const sandbox = new BrowserSandbox("/assets/browser-worker.js", "/assets");
    await sandbox.initialize();
    log("Sandbox ready: " + sandbox.id);

    statusEl.textContent = "Creating agent...";
    const model = new ChatAnthropic({
      model: "claude-haiku-4-5-20251001",
      apiKey,
      clientOptions: { dangerouslyAllowBrowser: true },
    });
    const agent = createDeepAgent({ model, backend: sandbox });

    statusEl.textContent = "Agent running — this may take 10-30 seconds...";
    log("\nTask: " + prompt + "\n");

    await agent.invoke({
      messages: [new HumanMessage(prompt)],
    });

    statusEl.textContent = "Reading results...";
    const files = await sandbox.execute("find /workspace -type f");
    log("=== Files in sandbox ===");
    log(files.output);

    // Try to show the most recently created file
    const fileList = files.output.trim().split("\n").filter(Boolean);
    for (const f of fileList) {
      if (f === "/workspace/sales.csv") continue; // skip seed data
      const content = await sandbox.execute(`cat '${f}'`);
      if (content.exitCode === 0 && content.output.trim()) {
        log(`=== ${f} ===`);
        log(content.output);
      }
    }

    await sandbox.stop();
    statusEl.textContent = "Done.";
  } catch (err) {
    statusEl.textContent = "Error: " + err.message;
    log("\nERROR: " + err.message);
    if (err.stack) log(err.stack);
  } finally {
    runBtn.disabled = false;
  }
};
