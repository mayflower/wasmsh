# DeepAgents + wasmsh — Node.js Example

An LLM agent backed by wasmsh's sandboxed shell runtime. The agent gets `execute` (bash/python3), `read_file`, `write_file`, `edit_file`, `ls`, `grep`, and `glob` tools — all running inside a virtual machine with no host OS access.

## Setup

```bash
npm install
export ANTHROPIC_API_KEY=sk-ant-...
```

## Run

```bash
npx tsx example.ts
```

## What happens

1. A `WasmshSandbox` is created with a virtual `/workspace` filesystem
2. A CSV file is seeded into the sandbox
3. `createDeepAgent` creates an LLM agent with the sandbox as backend
4. The agent analyzes the data using Python and shell tools
5. The report is read back from the sandbox

The agent decides autonomously how to accomplish the task — it may write Python scripts, pipe shell commands, or combine both. All execution happens inside wasmsh's sandboxed environment.

## Packages

- [`@langchain/wasmsh`](https://github.com/mayflower/deepagentsjs) — wasmsh sandbox provider for DeepAgents
- [`@mayflowergmbh/wasmsh-pyodide`](https://www.npmjs.com/package/@mayflowergmbh/wasmsh-pyodide) — wasmsh Pyodide runtime (Node.js + browser)
- [`deepagents`](https://github.com/langchain-ai/deepagentsjs) — LLM agent framework
