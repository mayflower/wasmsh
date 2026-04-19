# DeepAgents + wasmsh — Python Example

An LLM agent backed by wasmsh's sandboxed shell runtime. The agent gets `execute` (bash/python3), `read_file`, `write_file`, `edit_file`, `ls`, `grep`, and `glob` tools — all running inside a virtual machine with no host OS access.

## Setup

```bash
pip install -r requirements.txt
export ANTHROPIC_API_KEY=sk-ant-...
```

## Run

```bash
python example.py
```

## What happens

1. A `WasmshSandbox` is created — it spawns a Node.js subprocess running the wasmsh Pyodide runtime
2. A CSV file is seeded into the sandbox's virtual filesystem
3. `create_deep_agent` creates an LLM agent with the sandbox as backend
4. The agent analyzes the data using Python and shell tools
5. The report is read back from the sandbox

The agent decides autonomously how to accomplish the task. All execution happens inside wasmsh's sandboxed environment — no host filesystem or process access.

## Packages

- [`langchain-wasmsh`](https://github.com/mayflower/wasmsh/tree/main/packages/python/langchain-wasmsh) — wasmsh sandbox backend for LangChain Deep Agents
- [`wasmsh-pyodide-runtime`](https://pypi.org/project/wasmsh-pyodide-runtime/) — wasmsh Pyodide runtime assets
- [`deepagents`](https://github.com/langchain-ai/deepagents) — LLM agent framework
