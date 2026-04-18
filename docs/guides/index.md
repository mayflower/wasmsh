# How-to Guides

How-to guides are recipes for completing a specific task. They assume you
already know the basics — start with the [Tutorials](../tutorials/index.md)
if you do not.

## Embedding wasmsh

| Task | Guide |
|------|-------|
| Embed `wasmsh-runtime` in a Rust program | [Embedding wasmsh](embedding.md) |
| Drive wasmsh from a Pyodide / JS host | [Pyodide integration](pyodide-integration.md) |

## Extending wasmsh

| Task | Guide |
|------|-------|
| Add a new command (builtin, utility, or runtime intercept) | [Adding a command](adding-commands.md) |

The legacy split [adding-builtins](adding-builtins.md) and
[adding-utilities](adding-utilities.md) is still available for the narrow
case where you already know which layer you need.

## Operating wasmsh

| Task | Guide |
|------|-------|
| Diagnose common runtime errors | [Troubleshooting](troubleshooting.md) |
| Measure Pyodide startup and first-command latency | [Performance testing](performance-testing.md) |
| Develop against the scalable runner locally | [Local runner workflow](../how-to/runner-local.md) |
| Operate the scalable dispatcher + runner in Kubernetes | [Runner runbook](../how-to/runner-runbook.md) |
| Install the Helm chart | [Helm chart README](../../deploy/helm/wasmsh/README.md) |
| End-to-end test the K8s deployment with kind | [`e2e/kind/README.md`](../../e2e/kind/README.md) |

## See Also

- [Reference](../reference/index.md) for the exact shape of every command,
  builtin, utility, and protocol message.
- [Explanation](../explanation/index.md) for the architecture and design
  rationale that informs these recipes.
