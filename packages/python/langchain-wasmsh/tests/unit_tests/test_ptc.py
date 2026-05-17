"""Mocked round-trip tests for PTC host_call bridging."""

from __future__ import annotations

import json
import logging
from typing import Any
from unittest.mock import MagicMock, patch

import pytest
from deepagents.backends.protocol import (
    FileDownloadResponse,
    FileUploadResponse,
)

from langchain_wasmsh._repl import (
    Outcome,
    _coerce_tool_output,
    _make_ptc_dispatcher,
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


# ── _coerce_tool_output ────────────────────────────────────────────────────


class TestCoerceToolOutput:
    def test_primitives_pass_through(self) -> None:
        assert _coerce_tool_output(None) is None
        assert _coerce_tool_output(True) is True
        assert _coerce_tool_output(7) == 7
        assert _coerce_tool_output(1.5) == 1.5
        assert _coerce_tool_output("hi") == "hi"

    def test_list_and_dict_recurse(self) -> None:
        assert _coerce_tool_output([1, "x", None]) == [1, "x", None]
        assert _coerce_tool_output({"a": 1, "b": [2, 3]}) == {"a": 1, "b": [2, 3]}

    def test_basemodel_like_model_dump_is_used(self) -> None:
        class FakeModel:
            def model_dump(self) -> dict[str, Any]:
                return {"x": 1}

        assert _coerce_tool_output(FakeModel()) == {"x": 1}

    def test_to_dict_fallback(self) -> None:
        class FakeObj:
            def to_dict(self) -> dict[str, Any]:
                return {"x": 2}

        assert _coerce_tool_output(FakeObj()) == {"x": 2}

    def test_string_fallback(self) -> None:
        class Custom:
            def __repr__(self) -> str:
                return "<C>"

        assert _coerce_tool_output(Custom()) == "<C>"


# ── _make_ptc_dispatcher ────────────────────────────────────────────────────


class _StubTool:
    """Minimal stand-in for langchain BaseTool with an ``invoke`` method."""

    def __init__(self, fn: Any) -> None:
        self._fn = fn

    def invoke(self, args: dict[str, Any]) -> Any:
        return self._fn(args)


class TestPtcDispatcher:
    def test_success_envelope(self) -> None:
        dispatch = _make_ptc_dispatcher(
            {"search": _StubTool(lambda a: f"hit:{a['q']}")}
        )
        env = dispatch({"id": "hc_1", "tool": "search", "args": {"q": "foo"}})
        assert env == {"ok": True, "value": "hit:foo"}

    def test_unknown_tool(self) -> None:
        dispatch = _make_ptc_dispatcher({"search": _StubTool(lambda a: "ok")})
        env = dispatch({"id": "hc_2", "tool": "ghost", "args": {}})
        assert env["ok"] is False
        assert env["error"] == "UnknownToolError"

    def test_invoke_raises_isolated(self) -> None:
        def boom(_: dict[str, Any]) -> None:
            raise RuntimeError("nope")

        dispatch = _make_ptc_dispatcher({"search": _StubTool(boom)})
        env = dispatch({"id": "hc_3", "tool": "search", "args": {}})
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
        def boom(_: dict[str, Any]) -> None:
            err = RuntimeError("kaboom")
            raise err

        dispatch = _make_ptc_dispatcher({"search": _StubTool(boom)})
        with caplog.at_level(logging.WARNING, logger="langchain_wasmsh._repl"):
            env = dispatch({"id": "hc_log", "tool": "search", "args": {"q": "x"}})
        assert env["ok"] is False
        assert env["error"] == "RuntimeError"
        records = [r for r in caplog.records if r.levelno == logging.WARNING]
        assert records, "expected a WARNING log record from the PTC catch"
        record = records[-1]
        assert "search" in record.getMessage()
        assert getattr(record, "wasmsh_ptc_call_id", None) == "hc_log"
        assert getattr(record, "wasmsh_ptc_tool", None) == "search"
        # `exc_info=True` carries the original exception for downstream
        # handlers (Sentry, structlog adapters, etc.).
        assert record.exc_info is not None
        assert isinstance(record.exc_info[1], RuntimeError)

    def test_non_dict_args_rejected(self) -> None:
        dispatch = _make_ptc_dispatcher({"search": _StubTool(lambda a: "ok")})
        env = dispatch({"id": "hc_4", "tool": "search", "args": "not a dict"})
        assert env["ok"] is False
        assert env["error"] == "TypeError"

    def test_tool_without_invoke(self) -> None:
        dispatch = _make_ptc_dispatcher({"oops": object()})
        env = dispatch({"id": "hc_5", "tool": "oops", "args": {}})
        assert env["ok"] is False
        assert env["error"] == "ToolMisconfigured"


# ── WasmshSandbox.run_ptc end-to-end (stub subprocess) ─────────────────────


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
                "search": _StubTool(lambda a: f"hit:{a['q']}"),
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
                "search": _StubTool(lambda a: f"hit:{a['q']}"),
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

        dispatcher = _make_ptc_dispatcher({"search": _StubTool(reentrant_tool)})

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
            ptc_tools={"search": _StubTool(lambda a: f"hit:{a['q']}")},
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
