# Agent Harness — Randomized Compatibility Testing

LLM-generated random tasks that exercise wasmsh through a real DeepAgent. Every run generates fresh tasks, executes them against the sandbox, and auto-diagnoses failures to build a persistent backlog of sandbox gaps.

## How it works

```
1. GENERATE  →  LLM invents N random tasks with verification commands
2. EXECUTE   →  createDeepAgent + WasmshSandbox runs each task
3. VERIFY    →  Shell command checks the outcome (PASS/FAIL)
4. DIAGNOSE  →  On failure, LLM classifies root cause from full tool trace
5. RECORD    →  Results as JSONL, sandbox bugs tracked persistently
```

No hand-written task bank. The LLM generates completely different tasks every run, maximizing coverage over time.

## Quick start

```bash
# Requires ANTHROPIC_API_KEY and built Pyodide assets
export ANTHROPIC_API_KEY=sk-ant-...
npm install
node run.mjs --count 5
```

## Usage

```bash
node run.mjs --count 10                # Run 10 random tasks
node run.mjs --count 5 --category python   # Only Python tasks
node run.mjs --bugs                    # Show accumulated sandbox bugs
node run.mjs --summary                 # Show results of last run
node run.mjs --help                    # Show all options
```

### Categories

`file-ops`, `text-processing`, `python`, `shell-scripting`, `data-pipeline`, `archive`, `system`, `encoding`

## Architecture

Each task gets a **fresh WasmshSandbox** (Pyodide WASM session). The agent uses `createDeepAgent` from `mayflower/deepagentsjs` with wasmsh as the only backend — the same setup a real user would have. The agent gets 7 tools: `execute`, `read_file`, `write_file`, `edit_file`, `ls`, `grep`, `glob`.

### Task generation

A single LLM call generates N tasks as JSON. Each task has:
- **description** — natural language prompt the agent receives
- **seed_files** — files pre-loaded into `/workspace/` before the agent runs
- **verification** — shell command designed by the generator that prints `PASS` or `FAIL`

The agent never sees the verification command.

### Failure diagnosis

On failure, the diagnosing LLM receives the **full tool call trace** — every command the agent ran and every response the sandbox returned. It classifies as:

| Classification | Meaning | Action |
|---|---|---|
| `sandbox_bug` | wasmsh command/utility/parser didn't behave like real bash | Fix in wasmsh |
| `llm_mistake` | Agent wrote wrong code; same commands would fail on real Linux | Ignore |
| `test_issue` | Generated task or verification was flawed | Ignore |
| `timeout` | Exceeded 5-minute limit | Investigate |

## Output

### Per-run results

`results/<timestamp>.jsonl` — one JSON line per task:

```json
{
  "id": "python-json-transform",
  "category": "python",
  "passed": false,
  "diagnosis": {
    "classification": "sandbox_bug",
    "reason": "awk PROCINFO[\"sorted_in\"] not supported",
    "wasmsh_component": "awk_ops",
    "suggested_fix": "Implement gawk-compatible sorted array traversal"
  },
  "tool_trace": [{"tool": "execute", "input": "...", "output": "..."}],
  "duration_ms": 8500
}
```

### Persistent failure backlog

`failures/sandbox-bugs.jsonl` — only `sandbox_bug` entries, appended across runs. This file is committed to git and serves as the compatibility improvement backlog.

## Bugs found so far

The harness has identified and driven fixes for:

- **`bash`/`sh` command not found** — agents call `bash script.sh`
- **awk `PROCINFO["sorted_in"]`** — gawk-compatible sorted array traversal
- **Process substitution `<(cmd)`** — `diff <(cmd1) <(cmd2)` patterns
- **`diff` stdin via `-`** — `echo x | diff - file.txt`
- **`write_file` overwrite rejection** — `BaseSandbox.write()` refused to overwrite existing files (fixed in deepagentsjs + deepagents Python)

## Cost

Uses `claude-haiku-4-5-20251001`. Approximate cost per run:
- Task generation: ~$0.01
- Agent execution: ~$0.02/task
- Failure diagnosis: ~$0.01/failure
- **10 tasks: ~$0.25**

## Dependencies

- `deepagents` + `@langchain/wasmsh` from `mayflower/deepagentsjs` fork
- `@langchain/anthropic` + `@langchain/core`
- Built Pyodide assets at `packages/npm/wasmsh-pyodide/assets/`
- `ANTHROPIC_API_KEY` environment variable
