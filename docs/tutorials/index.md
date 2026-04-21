# Tutorials

Tutorials walk you through a single concrete task from start to finish.
They are the right place to start if you have not used wasmsh before.

If you already know what you want to do and just need a recipe, see
[How-to Guides](../guides/index.md). If you want to look up the exact
shape of a command or API, see [Reference](../reference/index.md). If you
want to understand *why* something works the way it does, see
[Explanation](../explanation/index.md).

## Choose a starting point

Pick the tutorial that matches the way you will use wasmsh:

| You want to … | Start here |
|---------------|------------|
| Run wasmsh from a Node.js program | [JavaScript / Node.js quick start](javascript-quickstart.md) |
| Run wasmsh from a Python program | [Python quick start](python-quickstart.md) |
| Embed wasmsh as a Rust library | [Getting Started (Rust)](getting-started.md) |
| Deploy wasmsh as a scalable sandbox pool (multi-user, Docker or K8s) | [Docker Compose](../../deploy/docker/README.md) or [Helm chart](../../deploy/helm/wasmsh/README.md); client side in the [LangChain integration guide](../integrations/langchain-wasmsh.md#wasmshremotesandbox--docker--kubernetes-backend) |
| Add new test cases to the suite | [Writing shell tests](writing-tests.md) |

If you are not sure which adapter you need, the JavaScript / Node.js
quickstart is the fastest path from `npm install` to a working sandbox.

## What you will learn

- How to install and load the runtime in your environment of choice.
- How to seed the virtual filesystem.
- How to run a script and read events back.
- How to grant the sandbox network access (and what happens if you don't).
- How to interrupt a long-running script.

When you are done, the [How-to Guides](../guides/index.md) cover the
follow-up tasks: embedding in a real application, adding custom commands,
and troubleshooting.
