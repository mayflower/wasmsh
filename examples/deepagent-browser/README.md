# DeepAgents + wasmsh — Fully In-Browser Example

An LLM agent running **entirely in the browser** — no backend service. The wasmsh sandbox provides shell, Python, and filesystem via a Web Worker. Anthropic API calls go directly from the browser using `dangerouslyAllowBrowser`.

## Setup

```bash
npm install
```

## Run

```bash
npm run dev
```

Open `http://localhost:5173`, enter your Anthropic API key, and click Run.

## What happens

1. A wasmsh browser Worker boots (loads Pyodide WASM ~8MB)
2. `createDeepAgent` creates an LLM agent with the sandbox as backend
3. The agent uses `execute`, `read_file`, `write_file`, `edit_file`, `ls`, `grep`, `glob` tools
4. All shell/Python execution happens in the Web Worker
5. LLM calls go directly to Anthropic's API (CORS enabled via `dangerouslyAllowBrowser`)

No data leaves your browser except the LLM API calls.

## Architecture

```
Browser Tab
├── main.js (agent loop, UI)
├── ChatAnthropic (Anthropic API, direct browser access)
├── createDeepAgent (LangGraph agent with filesystem middleware)
└── BrowserSandbox → Web Worker
    ├── wasmsh runtime (Emscripten WASM)
    ├── Pyodide (CPython in WASM)
    └── Virtual filesystem (/workspace)
```

## Packages

- [`deepagents`](https://github.com/langchain-ai/deepagentsjs) — LLM agent framework
- [`@langchain/anthropic`](https://www.npmjs.com/package/@langchain/anthropic) — Anthropic LLM provider
- [`@mayflowergmbh/wasmsh-pyodide`](https://www.npmjs.com/package/@mayflowergmbh/wasmsh-pyodide) — wasmsh Pyodide runtime
