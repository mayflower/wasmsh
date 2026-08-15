"""Mocked round-trip tests for PTC host_call bridging."""

from __future__ import annotations

import asyncio
import json
import logging
from dataclasses import dataclass
from typing import Annotated, Any
from unittest.mock import MagicMock, patch

import pytest
from deepagents.backends.protocol import (
    FileDownloadResponse,
    FileUploadResponse,
)
from langchain.tools import InjectedState, ToolRuntime
from langchain_core.messages import ToolMessage
from langchain_core.tools import InjectedToolCallId, tool
from langgraph.store.memory import InMemoryStore
from langgraph.types import Command
from pydantic import BaseModel

from langchain_wasmsh._ptc import (
    coerce_tool_output_for_ptc,
    normalize_tool_input,
)
from langchain_wasmsh._repl import (
    Outcome,
    _PTCSession,
    _ThreadREPL,
)
from langchain_wasmsh.sandbox import WasmshSandbox

# ── Mock subprocess that drives a scripted dialogue ─────────────────────────


class _ScriptedDialogue:
    """Drives stdin/stdout for a WasmshSandbox under test.

    Constructed with a list of ``Turn`` entries; each turn maps an incoming
    stdin message (matched by id or by ``type``) to a sequence of outgoing
    stdout lines. Used to drive the PTC round-trip without booting Pyodide.
    """

    def __init__(self, scripted_outputs: list[str]) -> None:
        # FIFO queue of stdout lines (each must end in \n). The test will
        # also append new lines in response to stdin writes.
        self._outbox: list[str] = list(scripted_outputs)
        self.inbox: list[dict[str, Any]] = []  # all lines we received on stdin
        self._writeback_hook: Any = None

    def set_writeback_hook(
        self,
        hook: Any,
    ) -> None:
        """Optional callback invoked with each stdin message; may enqueue replies."""
        self._writeback_hook = hook

    def stdin_write(self, line: str) -> int:
        msg = json.loads(line.strip())
        self.inbox.append(msg)
        if self._writeback_hook is not None:
            self._writeback_hook(msg, self._outbox)
        return len(line)

    def stdin_flush(self) -> None:
        pass

    def stdout_readline(self) -> str:
        if not self._outbox:
            return ""
        return self._outbox.pop(0)


def _build_sandbox(
    dialogue: _ScriptedDialogue,
    *,
    advertise_ack: bool = True,
) -> WasmshSandbox:
    """Spin up a WasmshSandbox whose subprocess is replaced by ``dialogue``."""
    process = MagicMock()
    process.poll.return_value = None
    process.stdin = MagicMock()
    process.stdin.write.side_effect = dialogue.stdin_write
    process.stdin.flush.side_effect = dialogue.stdin_flush
    process.stderr = MagicMock()
    process.stderr.read.return_value = ""
    process.stdout = MagicMock()
    process.stdout.readline = MagicMock(side_effect=dialogue.stdout_readline)

    # Scripted init: optional ack then the init response.
    if advertise_ack:
        dialogue._outbox.insert(
            0,
            json.dumps({"type": "ack", "capabilities": {"host_call": "v1"}}) + "\n",
        )
    dialogue._outbox.append(
        json.dumps({"id": 1, "ok": True, "result": {"events": []}}) + "\n",
    )

    with (
        patch("shutil.which", return_value="/usr/bin/deno"),
        patch("subprocess.Popen", return_value=process),
    ):
        return WasmshSandbox()


# ── coerce_tool_output_for_ptc ─────────────────────────────────────────────


class TestCoerceToolOutput:
    def test_primitives_pass_through(self) -> None:
        assert coerce_tool_output_for_ptc(None) is None
        assert coerce_tool_output_for_ptc(True) is True
        assert coerce_tool_output_for_ptc(7) == 7
        assert coerce_tool_output_for_ptc(1.5) == 1.5
        assert coerce_tool_output_for_ptc("hi") == "hi"

    def test_list_and_dict_recurse(self) -> None:
        assert coerce_tool_output_for_ptc([1, "x", None]) == [1, "x", None]
        assert coerce_tool_output_for_ptc({"a": 1, "b": [2, 3]}) == {
            "a": 1,
            "b": [2, 3],
        }

    def test_pydantic_model_keeps_field_shape(self) -> None:
        class Result(BaseModel):
            score: int
            label: str

        assert coerce_tool_output_for_ptc(Result(score=3, label="x")) == {
            "score": 3,
            "label": "x",
        }

    def test_nested_structure_survives_an_unserialisable_leaf(self) -> None:
        # Only the leaf is stringified; the object around it stays navigable
        # from the interpreter.
        class Custom:
            def __repr__(self) -> str:
                return "<C>"

        assert coerce_tool_output_for_ptc({"rows": [{"obj": Custom()}]}) == {
            "rows": [{"obj": "<C>"}],
        }

    def test_tool_message_is_unwrapped_to_its_content(self) -> None:
        message = ToolMessage(content="payload", tool_call_id="abc")
        assert coerce_tool_output_for_ptc(message) == "payload"

    def test_command_is_unwrapped_to_its_trailing_message(self) -> None:
        command = Command(
            update={"messages": [ToolMessage(content="done", tool_call_id="abc")]},
        )
        assert coerce_tool_output_for_ptc(command) == "done"


# ── argument normalization ─────────────────────────────────────────────────


class TestNormalizeToolInput:
    def test_none_becomes_empty_args(self) -> None:
        assert normalize_tool_input(None) == {}

    def test_dict_passes_through_as_a_copy(self) -> None:
        source = {"q": "x"}
        assert normalize_tool_input(source) == {"q": "x"}
        assert normalize_tool_input(source) is not source

    def test_scalar_is_routed_into_schema_validation(self) -> None:
        # Wrapping under a conventional key makes the tool's own schema
        # produce an actionable error instead of silently missing an arg.
        assert normalize_tool_input("bare") == {"input": "bare"}


# ── runtime-aware dispatch ─────────────────────────────────────────────────


@dataclass
class _Context:
    """Stand-in for an application's `context_schema` dataclass."""

    user_id: str


def _runtime(
    *,
    state: Any = None,
    store: Any = None,
    context: Any = None,
) -> ToolRuntime:
    return ToolRuntime(
        state=state if state is not None else {"messages": []},
        context=context,
        config={"configurable": {"thread_id": "t1"}},
        stream_writer=lambda _: None,
        tool_call_id="outer_call",
        store=store,
        tools=[],
    )


def _dispatch(session: _PTCSession, tool: str, args: Any = None) -> dict[str, Any]:
    return session.dispatch({"id": "hc_1", "tool": tool, "args": args})


class TestPtcDispatcher:
    def test_success_envelope(self) -> None:
        @tool
        def search(q: str) -> str:
            """Search."""
            return f"hit:{q}"

        session = _PTCSession(tools={"search": search}, outer_runtime=_runtime())
        assert _dispatch(session, "search", {"q": "foo"}) == {
            "ok": True,
            "value": "hit:foo",
        }

    def test_unknown_tool(self) -> None:
        session = _PTCSession(tools={})
        env = _dispatch(session, "ghost", {})
        assert env["ok"] is False
        assert env["error"] == "UnknownToolError"

    def test_tool_receives_child_runtime_with_a_fresh_call_id(self) -> None:
        seen: dict[str, Any] = {}

        @tool
        def probe(runtime: ToolRuntime) -> str:
            """Probe the injected runtime."""
            seen["tool_call_id"] = runtime.tool_call_id
            seen["context"] = runtime.context
            seen["state"] = runtime.state
            return "ok"

        outer = _runtime(
            state={"messages": [], "custom": 42},
            context=_Context(user_id="u1"),
        )
        session = _PTCSession(tools={"probe": probe}, outer_runtime=outer)

        assert _dispatch(session, "probe")["ok"] is True
        # Derived from the outer call, but with its own id so tracing and
        # checkpointed state can correlate the nested call separately.
        assert seen["tool_call_id"] != "outer_call"
        assert seen["tool_call_id"].startswith("ptc_probe_")
        assert seen["context"] == _Context(user_id="u1")
        assert seen["state"]["custom"] == 42

    def test_tool_receives_the_store(self) -> None:
        store = InMemoryStore()
        store.put(("ns",), "k", {"v": 1})

        @tool
        def reader(runtime: ToolRuntime) -> dict[str, Any]:
            """Read from the store."""
            item = runtime.store.get(("ns",), "k")
            return item.value

        session = _PTCSession(
            tools={"reader": reader},
            outer_runtime=_runtime(store=store),
        )
        assert _dispatch(session, "reader") == {"ok": True, "value": {"v": 1}}

    def test_injected_state_field_is_supplied(self) -> None:
        @tool
        def peek(custom: Annotated[int, InjectedState("custom")]) -> int:
            """Peek at a state field."""
            return custom

        session = _PTCSession(
            tools={"peek": peek},
            outer_runtime=_runtime(state={"messages": [], "custom": 7}),
        )
        assert _dispatch(session, "peek") == {"ok": True, "value": 7}

    def test_injected_tool_call_id_is_supplied(self) -> None:
        @tool
        def stamped(tool_call_id: Annotated[str, InjectedToolCallId]) -> str:
            """Return a message stamped with the call id."""
            return ToolMessage(content=tool_call_id, tool_call_id=tool_call_id)

        session = _PTCSession(tools={"stamped": stamped}, outer_runtime=_runtime())
        env = _dispatch(session, "stamped")
        assert env["ok"] is True
        assert env["value"].startswith("ptc_stamped_")

    def test_generated_code_cannot_forge_injected_arguments(self) -> None:
        seen: dict[str, Any] = {}

        @tool
        def probe(runtime: ToolRuntime) -> str:
            """Probe the injected runtime."""
            seen["context"] = runtime.context
            return "ok"

        session = _PTCSession(
            tools={"probe": probe},
            outer_runtime=_runtime(context=_Context(user_id="real")),
        )
        # A model-authored program supplying its own `runtime` must not be
        # able to substitute identity.
        _dispatch(
            session, "probe", {"runtime": {"context": _Context(user_id="forged")}}
        )
        assert seen["context"] == _Context(user_id="real")

    def test_async_only_tool_runs_from_the_sync_path(self) -> None:
        @tool
        async def fetch(q: str) -> str:
            """Async-only tool."""
            await asyncio.sleep(0)
            return f"async:{q}"

        session = _PTCSession(tools={"fetch": fetch}, outer_runtime=_runtime())
        assert _dispatch(session, "fetch", {"q": "x"}) == {
            "ok": True,
            "value": "async:x",
        }

    async def test_tools_run_on_the_agent_loop_under_async_execution(self) -> None:
        loop = asyncio.get_running_loop()
        seen: dict[str, Any] = {}

        @tool
        async def fetch() -> str:
            """Record which loop executed the tool."""
            seen["loop"] = asyncio.get_running_loop()
            return "ok"

        session = _PTCSession(
            tools={"fetch": fetch},
            outer_runtime=_runtime(),
            outer_loop=loop,
        )
        # The dispatcher itself runs on a worker thread, mirroring
        # `eval_async`'s `asyncio.to_thread` hand-off.
        env = await asyncio.to_thread(_dispatch, session, "fetch")
        assert env == {"ok": True, "value": "ok"}
        assert seen["loop"] is loop

    def test_invoke_raises_isolated(self) -> None:
        @tool
        def boom() -> str:
            """Always fails."""
            msg = "nope"
            raise RuntimeError(msg)

        session = _PTCSession(tools={"boom": boom}, outer_runtime=_runtime())
        env = _dispatch(session, "boom")
        assert env["ok"] is False
        assert env["error"] == "RuntimeError"
        assert env["message"] == "nope"

    def test_invoke_raises_emits_warning_log(
        self,
        caplog: pytest.LogCaptureFixture,
    ) -> None:
        # The envelope still round-trips to the sandbox so the model can
        # recover; the structured log is the host's only window into the
        # original stack and call context.
        @tool
        def boom() -> str:
            """Always fails."""
            msg = "kaboom"
            raise RuntimeError(msg)

        session = _PTCSession(tools={"boom": boom}, outer_runtime=_runtime())
        with caplog.at_level(logging.WARNING, logger="langchain_wasmsh._repl"):
            env = session.dispatch({"id": "hc_log", "tool": "boom", "args": {}})
        assert env["ok"] is False
        assert env["error"] == "RuntimeError"
        records = [r for r in caplog.records if r.levelno == logging.WARNING]
        assert records, "expected a WARNING log record from the PTC catch"
        record = records[-1]
        assert "boom" in record.getMessage()
        assert getattr(record, "wasmsh_ptc_call_id", None) == "hc_log"
        assert getattr(record, "wasmsh_ptc_tool", None) == "boom"
        # `exc_info=True` carries the original exception for downstream
        # handlers (Sentry, structlog adapters, etc.).
        assert record.exc_info is not None
        assert isinstance(record.exc_info[1], RuntimeError)

    def test_invalid_arguments_reach_schema_validation(self) -> None:
        @tool
        def search(q: str) -> str:
            """Search."""
            return q

        session = _PTCSession(tools={"search": search}, outer_runtime=_runtime())
        env = _dispatch(session, "search", "not a dict")
        assert env["ok"] is False
        assert "q" in env["message"]


class TestPtcCallBudget:
    @staticmethod
    def _counting_session(limit: int | None) -> tuple[_PTCSession, list[int]]:
        calls: list[int] = []

        @tool
        def ping() -> str:
            """Count one call."""
            calls.append(1)
            return "pong"

        session = _PTCSession(
            tools={"ping": ping},
            outer_runtime=_runtime(),
            max_ptc_calls=limit,
        )
        return session, calls

    def test_calls_up_to_the_limit_succeed(self) -> None:
        session, calls = self._counting_session(3)
        for _ in range(3):
            assert _dispatch(session, "ping")["ok"] is True
        assert len(calls) == 3

    def test_the_call_past_the_limit_fails_without_invoking_the_tool(self) -> None:
        session, calls = self._counting_session(2)
        _dispatch(session, "ping")
        _dispatch(session, "ping")
        env = _dispatch(session, "ping")
        assert env["ok"] is False
        assert env["error"] == "PTCCallBudgetExceededError"
        assert "limit=2" in env["message"]
        # The point of the budget: the third call never reaches the tool.
        assert len(calls) == 2

    def test_none_disables_the_budget(self) -> None:
        session, calls = self._counting_session(None)
        for _ in range(5):
            assert _dispatch(session, "ping")["ok"] is True
        assert len(calls) == 5

    def test_budget_is_per_evaluation(self) -> None:
        # A fresh _PTCSession is built per eval, so a new program starts
        # with a full budget rather than inheriting the previous one.
        first, _ = self._counting_session(1)
        assert _dispatch(first, "ping")["ok"] is True
        assert _dispatch(first, "ping")["ok"] is False
        second, _ = self._counting_session(1)
        assert _dispatch(second, "ping")["ok"] is True


# ── WasmshSandbox.run_ptc end-to-end (stub subprocess) ─────────────────────


# These exercise the JSON-RPC transport, not tool semantics, so they build a
# dispatcher over trivial tools and assert on the wire exchange.


def _stub_tool(name: str, fn: Any) -> Any:
    @tool(name)
    def _impl(q: str = "") -> Any:
        """Stub tool."""
        return fn({"q": q})

    return _impl


def _make_ptc_dispatcher(tools: dict[str, Any]) -> Any:
    return _PTCSession(tools=tools).dispatch


class TestRunPtcRoundTrip:
    def test_single_host_call(self) -> None:
        dialogue = _ScriptedDialogue([])

        def writeback(msg: dict[str, Any], outbox: list[str]) -> None:
            # When the sandbox writes a `runPtc` request, push host_call then result.
            if msg.get("method") == "runPtc":
                req_id = msg["id"]
                outbox.append(
                    json.dumps(
                        {
                            "type": "host_call",
                            "id": "hc_a",
                            "tool": "search",
                            "args": {"q": "foo"},
                        }
                    )
                    + "\n",
                )
                outbox.append(
                    json.dumps(
                        {
                            "id": req_id,
                            "ok": True,
                            "result": {
                                "envelope": {
                                    "ok": True,
                                    "stdout": "",
                                    "stderr": "",
                                    "value": "hit:foo",
                                },
                            },
                        }
                    )
                    + "\n",
                )

        dialogue.set_writeback_hook(writeback)
        sandbox = _build_sandbox(dialogue)
        dispatcher = _make_ptc_dispatcher(
            {
                "search": _stub_tool("search", lambda a: f"hit:{a['q']}"),
            }
        )

        envelope = sandbox.run_ptc(
            "await tools.search(q='foo')",
            tools=["search"],
            on_host_call=dispatcher,
        )

        # Sandbox saw the host_call_result we wrote back.
        host_results = [
            m for m in dialogue.inbox if m.get("type") == "host_call_result"
        ]
        assert len(host_results) == 1
        assert host_results[0]["id"] == "hc_a"
        assert host_results[0]["ok"] is True
        assert host_results[0]["value"] == "hit:foo"
        # And we got the launcher envelope back.
        assert envelope["ok"] is True
        # _safe_value passes primitives through; native string, not repr.
        assert envelope["value"] == "hit:foo"

    def test_unknown_tool_emits_error_envelope(self) -> None:
        dialogue = _ScriptedDialogue([])

        def writeback(msg: dict[str, Any], outbox: list[str]) -> None:
            if msg.get("method") == "runPtc":
                outbox.append(
                    json.dumps(
                        {
                            "type": "host_call",
                            "id": "hc_z",
                            "tool": "search",
                            "args": {},
                        }
                    )
                    + "\n",
                )
                outbox.append(
                    json.dumps(
                        {
                            "id": msg["id"],
                            "ok": True,
                            "result": {
                                "envelope": {"ok": True, "stdout": "", "stderr": ""}
                            },
                        }
                    )
                    + "\n",
                )

        dialogue.set_writeback_hook(writeback)
        sandbox = _build_sandbox(dialogue)
        dispatcher = _make_ptc_dispatcher({})  # empty allowlist

        sandbox.run_ptc("await tools.search()", tools=[], on_host_call=dispatcher)

        host_results = [
            m for m in dialogue.inbox if m.get("type") == "host_call_result"
        ]
        assert host_results[0]["ok"] is False
        assert host_results[0]["error"] == "UnknownToolError"

    def test_no_capability_raises(self) -> None:
        dialogue = _ScriptedDialogue([])

        sandbox = _build_sandbox(dialogue, advertise_ack=False)
        dispatcher = _make_ptc_dispatcher({})

        with pytest.raises(RuntimeError, match="host_call capability"):
            sandbox.run_ptc("pass", tools=[], on_host_call=dispatcher)

    def test_parallel_host_calls_correlated_by_id(self) -> None:
        """Two concurrent host_calls (asyncio.gather-style) round-trip correctly.

        The dispatcher receives both events; both host_call_result envelopes
        are written back; the sandbox's terminal envelope still lands.
        """
        dialogue = _ScriptedDialogue([])

        def writeback(msg: dict[str, Any], outbox: list[str]) -> None:
            if msg.get("method") == "runPtc":
                req_id = msg["id"]
                # Two host_calls in flight, sandbox emits both before result.
                outbox.append(
                    json.dumps(
                        {
                            "type": "host_call",
                            "id": "hc_alpha",
                            "tool": "search",
                            "args": {"q": "alpha"},
                        }
                    )
                    + "\n",
                )
                outbox.append(
                    json.dumps(
                        {
                            "type": "host_call",
                            "id": "hc_beta",
                            "tool": "search",
                            "args": {"q": "beta"},
                        }
                    )
                    + "\n",
                )
                outbox.append(
                    json.dumps(
                        {
                            "id": req_id,
                            "ok": True,
                            "result": {
                                "envelope": {
                                    "ok": True,
                                    "stdout": "",
                                    "stderr": "",
                                    "value": ["hit:alpha", "hit:beta"],
                                },
                            },
                        }
                    )
                    + "\n",
                )

        dialogue.set_writeback_hook(writeback)
        sandbox = _build_sandbox(dialogue)
        dispatcher = _make_ptc_dispatcher(
            {
                "search": _stub_tool("search", lambda a: f"hit:{a['q']}"),
            }
        )

        envelope = sandbox.run_ptc(
            "await asyncio.gather(tools.search(q='alpha'), tools.search(q='beta'))",
            tools=["search"],
            on_host_call=dispatcher,
        )

        host_results = {
            m["id"]: m for m in dialogue.inbox if m.get("type") == "host_call_result"
        }
        # Both ids round-tripped, each with the right per-call value.
        assert host_results["hc_alpha"]["value"] == "hit:alpha"
        assert host_results["hc_beta"]["value"] == "hit:beta"
        assert envelope["ok"] is True
        assert envelope["value"] == ["hit:alpha", "hit:beta"]

    def test_stuck_host_emitting_stale_ids_bails_out(self) -> None:
        """The _MAX_STALE_RESPONSES guard prevents the infinite-loop OOM bug."""
        dialogue = _ScriptedDialogue([])

        def writeback(msg: dict[str, Any], outbox: list[str]) -> None:
            if msg.get("method") == "runPtc":
                # Flood with mismatched-id responses.
                stale = json.dumps({"id": 99999, "ok": True, "result": {}}) + "\n"
                outbox.extend([stale] * 200)

        dialogue.set_writeback_hook(writeback)
        sandbox = _build_sandbox(dialogue)
        dispatcher = _make_ptc_dispatcher({})

        with pytest.raises(RuntimeError, match="mismatched ids"):
            sandbox.run_ptc("pass", tools=[], on_host_call=dispatcher)

    def test_reentrant_sandbox_call_raises_clearly(self) -> None:
        """A PTC tool that calls back into the sandbox surfaces a clean error."""
        dialogue = _ScriptedDialogue([])
        # Build sandbox first so the dispatcher closure can reference it.
        captured_sandbox: list[Any] = []

        def writeback(msg: dict[str, Any], outbox: list[str]) -> None:
            if msg.get("method") == "runPtc":
                outbox.append(
                    json.dumps(
                        {
                            "type": "host_call",
                            "id": "hc_reentry",
                            "tool": "search",
                            "args": {"q": "x"},
                        }
                    )
                    + "\n",
                )
                outbox.append(
                    json.dumps(
                        {
                            "id": msg["id"],
                            "ok": True,
                            "result": {
                                "envelope": {"ok": True, "stdout": "", "stderr": ""}
                            },
                        }
                    )
                    + "\n",
                )

        dialogue.set_writeback_hook(writeback)
        sandbox = _build_sandbox(dialogue)
        captured_sandbox.append(sandbox)

        def reentrant_tool(_args: dict[str, Any]) -> str:
            # Tool synchronously calls back into the same sandbox — should
            # raise instead of deadlocking.
            captured_sandbox[0].execute("echo hi")
            return "should not reach"

        dispatcher = _make_ptc_dispatcher(
            {"search": _stub_tool("search", reentrant_tool)}
        )

        sandbox.run_ptc(
            "await tools.search(q='x')", tools=["search"], on_host_call=dispatcher
        )

        host_results = [
            m for m in dialogue.inbox if m.get("type") == "host_call_result"
        ]
        assert host_results[0]["ok"] is False
        assert host_results[0]["error"] == "RuntimeError"
        assert "reentrant" in host_results[0]["message"]


# ── _ThreadREPL routes through run_ptc when ptc_tools is provided ─────────


class _StubSandboxForRepl:
    """Records run_ptc invocations; pretends to support every operation."""

    def __init__(self) -> None:
        self.run_ptc_calls: list[dict[str, Any]] = []
        self.upload_log: list[tuple[str, bytes]] = []
        self.vfs: dict[str, bytes] = {}
        self.closed = False

    def run_ptc(
        self,
        code: str,
        *,
        tools: list[str],
        on_host_call: Any,
    ) -> dict[str, Any]:
        self.run_ptc_calls.append({"code": code, "tools": tools})
        # Synthesise one host_call so we can verify dispatcher wiring.
        env = on_host_call(
            {
                "id": "hc_test",
                "tool": tools[0] if tools else "missing",
                "args": {"q": "foo"},
            }
        )
        return {
            "ok": env["ok"],
            "stdout": "",
            "stderr": "",
            # _safe_value passes primitives through unchanged.
            "value": env.get("value") if env["ok"] else None,
            "error": env.get("error"),
            "message": env.get("message"),
        }

    def upload_files(self, files: list[tuple[str, bytes]]) -> list[FileUploadResponse]:
        for path, content in files:
            self.upload_log.append((path, content))
            self.vfs[path] = content
        return [FileUploadResponse(path=p) for p, _ in files]

    def download_files(self, paths: list[str]) -> list[FileDownloadResponse]:
        return [
            FileDownloadResponse(path=p, content=self.vfs.get(p), error=None)
            for p in paths
        ]

    def execute(self, command: str, *, timeout: int | None = None) -> Any:  # noqa: ARG002
        msg = "execute() not expected on PTC path"
        raise AssertionError(msg)

    def close(self) -> None:
        self.closed = True


class TestReplRoutesThroughRunPtc:
    def test_ptc_tools_provided_routes_to_run_ptc(self) -> None:
        sandbox = _StubSandboxForRepl()
        repl = _ThreadREPL(factory=lambda: sandbox)
        outcome = repl.eval_sync(
            "await tools.search(q='foo')",
            ptc_tools={"search": _stub_tool("search", lambda a: f"hit:{a['q']}")},
        )
        assert isinstance(outcome, Outcome)
        assert outcome.ok is True
        # run_ptc was called with the tool list resolved to allowlist names.
        assert len(sandbox.run_ptc_calls) == 1
        assert sandbox.run_ptc_calls[0]["tools"] == ["search"]
        # Dispatcher returned the tool result; _safe_value passes the
        # native string through.
        assert outcome.value == "hit:foo"

    def test_no_ptc_tools_falls_through_to_shell_launcher(self) -> None:
        # When no ptc_tools is passed, run_ptc must not be called.
        sandbox = _StubSandboxForRepl()
        repl = _ThreadREPL(factory=lambda: sandbox)
        # execute() raises AssertionError in our stub; we expect a host-error
        # Outcome because the sandbox doesn't actually run anything.
        outcome = repl.eval_sync("x = 1")
        assert not sandbox.run_ptc_calls
        assert outcome.ok is False
