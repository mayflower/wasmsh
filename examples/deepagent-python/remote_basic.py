"""Minimal langchain-wasmsh remote sandbox example (no LLM).

Shows how to point `WasmshRemoteSandbox` at a running wasmsh dispatcher
(e.g. the stack in `deploy/docker/compose.dispatcher-test.yml`) and run
bash + Python against a runner pod — same virtual filesystem semantics
as the in-process backend, but the sandbox lives in Kubernetes.

Run (from repo root):

    docker compose -f deploy/docker/compose.dispatcher-test.yml up -d --wait
    WASMSH_DISPATCHER_URL=http://localhost:8080 \\
      uv --project packages/python/langchain-wasmsh \\
      run python examples/deepagent-python/remote_basic.py
"""

from __future__ import annotations

import os
import sys

from langchain_wasmsh import WasmshRemoteSandbox


def main() -> None:
    url = os.environ.get("WASMSH_DISPATCHER_URL")
    if not url:
        print(  # noqa: T201
            "Set WASMSH_DISPATCHER_URL to your wasmsh dispatcher "
            "(e.g. http://localhost:8080).",
            file=sys.stderr,
        )
        sys.exit(1)

    backend = WasmshRemoteSandbox(
        url,
        initial_files={"/workspace/data.txt": b"hello from remote wasmsh\n"},
    )
    try:
        cmd = (
            "cat data.txt && python3 -c "
            "\"print(open('/workspace/data.txt').read().strip())\""
        )
        result = backend.execute(cmd)
        print(result.output)  # noqa: T201
    finally:
        backend.close()


if __name__ == "__main__":
    main()
