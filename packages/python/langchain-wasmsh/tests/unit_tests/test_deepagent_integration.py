"""End-to-end integration tests for ``WasmshInterpreterMiddleware``.

Mirrors the shape of ``langchain_quickjs.tests.unit_tests.test_end_to_end``:
build a real DeepAgent via ``create_deep_agent``, drive it with a scripted
``FakeChatModel`` that emits a ``py_eval`` tool call, and assert the
ToolMessage the middleware returns contains the expected eval result.

Two variants:

- **Stub-sandbox path** (fast, default): swaps the sandbox factory for a
  recording stub so we exercise the middleware lifecycle + tool dispatch
  without booting Pyodide.
- **Real-sandbox path** (slow, gated on the presence of Pyodide assets):
  spawns the actual Node host so the eval runs against real Pyodide.
"""

from __future__ import annotations

import json
import shutil
from collections.abc import (
    Iterator,  # noqa: TC003 -- pydantic resolves field annotations at runtime
    Sequence,  # noqa: TC003 -- pydantic resolves field annotations at runtime
)
from pathlib import Path
from typing import Any

import pytest
from deepagents import create_deep_agent
from deepagents.backends.protocol import (
    ExecuteResponse,
    FileDownloadResponse,
    FileUploadResponse,
)
from langchain_core.language_models.fake_chat_models import GenericFakeChatModel
from langchain_core.messages import AIMessage, HumanMessage, ToolMessage
from langchain_core.tools import tool
from pydantic import Field
from wasmsh_pyodide_runtime import get_dist_dir

from langchain_wasmsh import WasmshInterpreterMiddleware, WasmshSandbox
from langchain_wasmsh._launcher import CODE_PATH, LAUNCHER_PATH, RESULT_MARKER


class FakeChatModel(GenericFakeChatModel):
    """``GenericFakeChatModel`` whose ``bind_tools`` returns ``self``.

    Without this override the agent wraps the model in a binding that no
    longer reads from the scripted message iterator. Mirrors the helper
    in ``langchain_quickjs.tests._common``.
    """

    messages: Iterator[AIMessage | str] = Field(exclude=True)

    def bind_tools(self, tools: Sequence[Any], **_: Any) -> FakeChatModel:
        del tools
        return self


def _script(code: str, *, final_message: str = "Done.") -> Iterator[AIMessage]:
    """Two-turn script: first turn calls ``py_eval``; second turn finishes."""
    return iter(
        [
            AIMessage(
                content="",
                tool_calls=[
                    {
                        "name": "py_eval",
                        "args": {"code": code},
                        "id": "call_1",
                        "type": "tool_call",
                    },
                ],
            ),
            AIMessage(content=final_message),
        ],
    )


def _eval_tool_message(result: dict[str, Any]) -> ToolMessage:
    messages = [
        m
        for m in result["messages"]
        if isinstance(m, ToolMessage) and m.name == "py_eval"
    ]
    assert messages, "expected at least one py_eval ToolMessage"
    return messages[-1]


# ── Stub sandbox path ─────────────────────────────────────────────────────


class _RecordingSandbox:
    """Sandbox stub that records calls and returns a scripted envelope.

    Matches the ``_SandboxLike`` Protocol surface ``_ThreadREPL`` consumes,
    so it can stand in for a real ``WasmshSandbox`` in the factory.
    """

    def __init__(self, envelope: dict[str, Any]) -> None:
        self._envelope = envelope
        self.uploads: list[tuple[str, bytes]] = []
        self.executes: list[str] = []
        self.vfs: dict[str, bytes] = {}

    def execute(self, command: str, *, timeout: int | None = None) -> ExecuteResponse:
        del timeout
        self.executes.append(command)
        payload = RESULT_MARKER + json.dumps(self._envelope) + "\n"
        return ExecuteResponse(output=payload, exit_code=0, truncated=False)

    def upload_files(self, files: list[tuple[str, bytes]]) -> list[FileUploadResponse]:
        for path, content in files:
            self.uploads.append((path, content))
            self.vfs[path] = content
        return [FileUploadResponse(path=p) for p, _ in files]

    def download_files(self, paths: list[str]) -> list[FileDownloadResponse]:
        return [
            FileDownloadResponse(path=p, content=self.vfs.get(p), error=None)
            for p in paths
        ]

    def close(self) -> None:  # pragma: no cover -- best-effort cleanup
        pass


class TestDeepAgentWithStubSandbox:
    """The middleware lifecycle + tool dispatch wire up correctly under create_deep_agent."""

    def test_eval_tool_runs_and_carries_result_into_messages(self) -> None:
        sandbox = _RecordingSandbox(
            {"ok": True, "stdout": "84\n", "stderr": "", "value": 84},
        )
        agent = create_deep_agent(
            model=FakeChatModel(messages=_script("2 * 42", final_message="answer = 84")),
            middleware=[
                WasmshInterpreterMiddleware(sandbox_factory=lambda: sandbox),
            ],
        )

        result = agent.invoke(
            {"messages": [HumanMessage(content="Compute 2 * 42 with py_eval.")]},
        )

        # The middleware uploaded the launcher + user code, then ran python3
        # against them inside the sandbox.
        uploaded_paths = [p for p, _ in sandbox.uploads]
        assert LAUNCHER_PATH in uploaded_paths
        assert CODE_PATH in uploaded_paths
        assert any("python3" in cmd for cmd in sandbox.executes)

        tool_message = _eval_tool_message(result)
        assert "84" in tool_message.content
        assert "<error" not in tool_message.content
        assert result["messages"][-1].content == "answer = 84"

    def test_eval_tool_surfaces_python_error(self) -> None:
        sandbox = _RecordingSandbox(
            {
                "ok": False,
                "error": "NameError",
                "message": "name 'foo' is not defined",
                "stdout": "",
                "stderr": "",
                "traceback": "Traceback (most recent call last):\n  NameError",
            },
        )
        agent = create_deep_agent(
            model=FakeChatModel(messages=_script("print(foo)", final_message="reported")),
            middleware=[
                WasmshInterpreterMiddleware(sandbox_factory=lambda: sandbox),
            ],
        )

        result = agent.invoke(
            {"messages": [HumanMessage(content="Try `print(foo)` and report.")]},
        )

        tool_message = _eval_tool_message(result)
        assert "NameError" in tool_message.content
        assert "name 'foo' is not defined" in tool_message.content
        # The agent still reaches its final turn — errors don't abort the loop.
        assert result["messages"][-1].content == "reported"

    def test_ptc_dispatch_routes_through_tool_invoke(self) -> None:
        """PTC tools registered via ``ptc=`` actually fire host_call → tool.invoke."""

        @tool
        def lookup_user(user_id: int) -> str:
            """Return a string description of a user by id."""
            return f"user#{user_id}=alice"

        # Stub sandbox: when run_ptc is called, synthesise one host_call so we
        # can verify the dispatcher pipeline. Then return the dispatcher's
        # value in the envelope (matches ptc-helper's _safe_value behaviour).
        class _PtcSandbox(_RecordingSandbox):
            def __init__(self) -> None:
                super().__init__(envelope={})
                self.host_call_results: list[dict[str, Any]] = []

            def run_ptc(
                self,
                code: str,
                *,
                tools: list[str],
                on_host_call: Any,
            ) -> dict[str, Any]:
                self.executes.append(f"runPtc:{code}")
                # Pretend the model wrote `await tools.lookup_user(user_id=1)`.
                env = on_host_call(
                    {
                        "id": "hc_1",
                        "tool": tools[0],
                        "args": {"user_id": 1},
                    },
                )
                self.host_call_results.append(env)
                return {
                    "ok": True,
                    "stdout": "",
                    "stderr": "",
                    "value": env.get("value"),
                }

        sandbox = _PtcSandbox()
        agent = create_deep_agent(
            tools=[lookup_user],
            model=FakeChatModel(
                messages=_script(
                    "await tools.lookup_user(user_id=1)",
                    final_message="user#1=alice",
                ),
            ),
            middleware=[
                WasmshInterpreterMiddleware(
                    sandbox_factory=lambda: sandbox,
                    ptc=["lookup_user"],
                ),
            ],
        )

        result = agent.invoke(
            {"messages": [HumanMessage(content="Look up user 1 via PTC.")]},
        )

        # The PTC dispatcher invoked the LangChain tool and got the right value.
        assert sandbox.host_call_results == [
            {"ok": True, "value": "user#1=alice"},
        ]
        tool_message = _eval_tool_message(result)
        assert "user#1=alice" in tool_message.content


# ── Real sandbox path (skip if Pyodide assets missing or no Node) ──────────


_PYODIDE_ASSET = Path(get_dist_dir()) / "pyodide.asm.wasm"
_NODE_AVAILABLE = shutil.which("node") is not None
_REAL_SANDBOX_AVAILABLE = _PYODIDE_ASSET.is_file() and _NODE_AVAILABLE
_SKIP_REASON = (
    "Pyodide assets or `node` not found; "
    "real-sandbox integration test requires both."
)


@pytest.mark.skipif(not _REAL_SANDBOX_AVAILABLE, reason=_SKIP_REASON)
class TestDeepAgentWithRealSandbox:
    """Real Pyodide sandbox + create_deep_agent, end to end."""

    def test_eval_computes_42(self) -> None:
        agent = create_deep_agent(
            model=FakeChatModel(messages=_script("6 * 7", final_message="42")),
            middleware=[
                WasmshInterpreterMiddleware(
                    sandbox_factory=lambda: WasmshSandbox(runtime="node"),
                ),
            ],
        )

        result = agent.invoke(
            {"messages": [HumanMessage(content="Compute 6 * 7 via py_eval.")]},
        )

        tool_message = _eval_tool_message(result)
        assert "42" in tool_message.content
        assert "<error" not in tool_message.content
        assert result["messages"][-1].content == "42"
