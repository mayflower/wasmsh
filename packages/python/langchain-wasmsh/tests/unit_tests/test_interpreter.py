"""Mocked unit tests for WasmshInterpreterMiddleware and helpers."""

from __future__ import annotations

import json
from typing import Any

import pytest
from deepagents.backends.protocol import (
    ExecuteResponse,
    FileDownloadResponse,
    FileUploadResponse,
)

from langchain_wasmsh import (
    WasmshInterpreterMiddleware,
)
from langchain_wasmsh._launcher import (
    CODE_PATH,
    GLOBALS_PATH,
    LAUNCHER_PATH,
    LAUNCHER_SCRIPT,
    RESULT_MARKER,
)
from langchain_wasmsh._ptc import filter_tools_for_ptc
from langchain_wasmsh._repl import Outcome, _Registry, _ThreadREPL, format_outcome

# ── Sandbox stub ─────────────────────────────────────────────────────────────


class _StubSandbox:
    """Minimal sandbox that records calls and returns scripted execute outputs."""

    def __init__(self, execute_outputs: list[str] | None = None) -> None:
        self.execute_outputs = list(execute_outputs or [])
        self.upload_log: list[tuple[str, bytes]] = []
        self.download_log: list[str] = []
        self.execute_log: list[str] = []
        self.closed = False
        # In-memory VFS so download_files can return what was uploaded.
        self.vfs: dict[str, bytes] = {}

    def execute(self, command: str, *, timeout: int | None = None) -> ExecuteResponse:
        del timeout
        self.execute_log.append(command)
        output = self.execute_outputs.pop(0) if self.execute_outputs else ""
        return ExecuteResponse(output=output, exit_code=0, truncated=False)

    def upload_files(self, files: list[tuple[str, bytes]]) -> list[FileUploadResponse]:
        for path, content in files:
            self.upload_log.append((path, content))
            self.vfs[path] = content
        return [FileUploadResponse(path=p) for p, _ in files]

    def download_files(self, paths: list[str]) -> list[FileDownloadResponse]:
        out: list[FileDownloadResponse] = []
        for path in paths:
            self.download_log.append(path)
            content = self.vfs.get(path)
            if content is None:
                out.append(FileDownloadResponse(path=path, error="file_not_found"))
            else:
                out.append(FileDownloadResponse(path=path, content=content))
        return out

    def close(self) -> None:
        self.closed = True

    # Stubbed file-protocol methods so the WasmshFilesystemBackend traversal
    # tests can resolve the attribute lookup; they should never be reached
    # because `_scope` raises on out-of-namespace paths before dispatch.
    def ls(self, path: str) -> Any:
        msg = f"ls should not run for traversal path {path!r}"
        raise AssertionError(msg)

    def glob(self, pattern: str, path: str = "/") -> Any:
        msg = f"glob should not run for traversal pattern={pattern!r} path={path!r}"
        raise AssertionError(msg)

    def grep(
        self,
        pattern: str,
        path: str | None = None,
        glob: str | None = None,
    ) -> Any:
        del path, glob
        msg = f"grep should not run for traversal pattern={pattern!r}"
        raise AssertionError(msg)

    def write(self, file_path: str, content: str) -> Any:
        del content
        msg = f"write should not run for traversal path {file_path!r}"
        raise AssertionError(msg)

    def edit(
        self,
        file_path: str,
        old_string: str,
        new_string: str,
        replace_all: bool = False,  # noqa: FBT001, FBT002 -- mirrors BackendProtocol
    ) -> Any:
        del old_string, new_string, replace_all
        msg = f"edit should not run for traversal path {file_path!r}"
        raise AssertionError(msg)

    def read(
        self,
        file_path: str,
        offset: int = 0,
        limit: int = 2000,
    ) -> Any:
        del offset, limit
        msg = f"read should not run for traversal path {file_path!r}"
        raise AssertionError(msg)


def _marker(envelope: dict[str, Any]) -> str:
    return RESULT_MARKER + json.dumps(envelope) + "\n"


# ── REPL & Outcome ───────────────────────────────────────────────────────────


class TestOutcome:
    def test_format_ok_with_stdout_and_value(self) -> None:
        outcome = Outcome(ok=True, stdout="hi\n", value="42")
        rendered = format_outcome(outcome, max_result_chars=100)
        assert "<stdout>" in rendered
        assert "hi" in rendered
        assert "<value>" in rendered
        assert "42" in rendered

    def test_format_ok_no_output(self) -> None:
        outcome = Outcome(ok=True)
        rendered = format_outcome(outcome, max_result_chars=100)
        assert "<no output>" in rendered

    def test_format_error(self) -> None:
        outcome = Outcome(
            ok=False,
            error="NameError",
            message="name 'foo' is not defined",
            traceback="Traceback ...\nNameError: name 'foo' is not defined",
        )
        rendered = format_outcome(outcome, max_result_chars=200)
        assert "<error NameError>" in rendered
        assert "name 'foo' is not defined" in rendered
        assert "Traceback" in rendered

    def test_format_truncates_long_block(self) -> None:
        outcome = Outcome(ok=True, stdout="x" * 1000)
        rendered = format_outcome(outcome, max_result_chars=50)
        assert "…" in rendered

    def test_envelope_round_trip(self) -> None:
        env = {
            "ok": True,
            "stdout": "out",
            "stderr": "err",
            "value": "1",
        }
        outcome = Outcome.from_envelope(env)
        assert outcome.ok
        assert outcome.stdout == "out"
        assert outcome.stderr == "err"
        assert outcome.value == "1"


class TestThreadREPL:
    def test_eval_uploads_launcher_then_code_and_returns_outcome(self) -> None:
        envelope = _marker({"ok": True, "stdout": "hi\n"})
        sandbox = _StubSandbox(execute_outputs=[envelope])
        repl = _ThreadREPL(factory=lambda: sandbox)
        outcome = repl.eval_sync("print('hi')")
        assert outcome.ok
        assert outcome.stdout == "hi\n"

        # First upload is the launcher; second is the user code.
        uploaded_paths = [path for path, _ in sandbox.upload_log]
        assert LAUNCHER_PATH in uploaded_paths
        assert CODE_PATH in uploaded_paths

        # The exec command invokes python on the launcher; the code path is
        # NOT on the command line (wasmsh's python3 builtin doesn't pass
        # argv), the launcher reads from a fixed VFS path instead.
        assert "python3" in sandbox.execute_log[0]
        assert LAUNCHER_PATH in sandbox.execute_log[0]
        assert CODE_PATH not in sandbox.execute_log[0]

    def test_launcher_uploaded_only_once_across_evals(self) -> None:
        sandbox = _StubSandbox(
            execute_outputs=[
                _marker({"ok": True}),
                _marker({"ok": True}),
            ],
        )
        repl = _ThreadREPL(factory=lambda: sandbox)
        repl.eval_sync("x = 1")
        repl.eval_sync("y = 2")
        launcher_uploads = [p for p, _ in sandbox.upload_log if p == LAUNCHER_PATH]
        assert len(launcher_uploads) == 1
        # The launcher payload is exactly the script body.
        launcher_bytes = next(c for p, c in sandbox.upload_log if p == LAUNCHER_PATH)
        assert launcher_bytes == LAUNCHER_SCRIPT.encode("utf-8")

    def test_missing_marker_returns_launcher_error(self) -> None:
        sandbox = _StubSandbox(execute_outputs=["something went wrong on the host\n"])
        repl = _ThreadREPL(factory=lambda: sandbox)
        outcome = repl.eval_sync("x = 1")
        assert not outcome.ok
        assert outcome.error == "LauncherError"

    def test_snapshot_round_trip(self) -> None:
        sandbox = _StubSandbox(execute_outputs=[_marker({"ok": True})])
        repl = _ThreadREPL(factory=lambda: sandbox)
        repl.eval_sync("x = 1")
        # Simulate the launcher having written a globals pickle.
        sandbox.vfs[GLOBALS_PATH] = b"PICKLED"
        payload = repl.create_snapshot()
        assert payload == b"PICKLED"

    def test_restore_snapshot_before_sandbox_starts_is_staged(self) -> None:
        sandbox = _StubSandbox(execute_outputs=[_marker({"ok": True})])
        repl = _ThreadREPL(factory=lambda: sandbox)
        repl.restore_snapshot(b"OLDPICKLE")
        repl.eval_sync("pass")
        # GLOBALS_PATH should be one of the uploads (in addition to launcher + code).
        uploaded = dict(sandbox.upload_log)
        assert uploaded.get(GLOBALS_PATH) == b"OLDPICKLE"


# ── PTC ──────────────────────────────────────────────────────────────────────


class TestPTC:
    def test_filter_tools_for_ptc_excludes_self(self) -> None:
        # Imported here to keep top-of-file imports stable across deepagents
        # environments where langchain_core may not be on the path.
        from langchain_core.tools import StructuredTool  # noqa: PLC0415

        def echo(value: str) -> str:
            """Echo the input value back."""
            return value

        a = StructuredTool.from_function(func=echo, name="search")
        b = StructuredTool.from_function(func=echo, name="py_eval")
        exposed = filter_tools_for_ptc(
            [a, b], ["search", "py_eval"], self_tool_name="py_eval"
        )
        assert [t.name for t in exposed] == ["search"]

    def test_filter_tools_for_ptc_rejects_non_list(self) -> None:
        with pytest.raises(TypeError):
            filter_tools_for_ptc([], "search", self_tool_name="py_eval")  # type: ignore[arg-type]


# ── Middleware construction ─────────────────────────────────────────────────


class TestMiddlewareConstruction:
    def test_tool_is_registered_with_default_name(self) -> None:
        mw = WasmshInterpreterMiddleware(sandbox_factory=_StubSandbox)
        assert len(mw.tools) == 1
        assert mw.tools[0].name == "py_eval"

    def test_custom_tool_name(self) -> None:
        mw = WasmshInterpreterMiddleware(
            sandbox_factory=_StubSandbox,
            tool_name="run_python",
        )
        assert mw.tools[0].name == "run_python"

    def test_ptc_construction_accepts_allowlist(self) -> None:
        # PTC is now wired into the runner; construction no longer raises.
        mw = WasmshInterpreterMiddleware(
            sandbox_factory=_StubSandbox,
            ptc=["search"],
        )
        assert mw._ptc == ["search"]

    def test_ptc_construction_rejects_non_list(self) -> None:
        with pytest.raises(TypeError):
            WasmshInterpreterMiddleware(
                sandbox_factory=_StubSandbox,
                ptc="search",  # type: ignore[arg-type]
            )

    def test_default_mode_is_thread(self) -> None:
        mw = WasmshInterpreterMiddleware(sandbox_factory=_StubSandbox)
        assert mw._mode == "thread"
        assert "persists across" in mw._base_system_prompt

    @pytest.mark.parametrize(
        ("mode", "expected_fragment"),
        [
            ("thread", "persists across"),
            ("turn", "DOES NOT persist"),
            ("call", "does NOT persist"),
        ],
    )
    def test_mode_selects_the_matching_prompt(
        self,
        mode: str,
        expected_fragment: str,
    ) -> None:
        mw = WasmshInterpreterMiddleware(sandbox_factory=_StubSandbox, mode=mode)
        assert mw._mode == mode
        assert expected_fragment in mw._base_system_prompt

    def test_unknown_mode_is_rejected(self) -> None:
        with pytest.raises(ValueError, match="mode"):
            WasmshInterpreterMiddleware(sandbox_factory=_StubSandbox, mode="forever")

    @pytest.mark.parametrize(
        ("legacy", "expected_mode"),
        [(True, "thread"), (False, "turn")],
    )
    def test_deprecated_snapshot_flag_maps_to_a_mode(
        self,
        legacy: bool,  # noqa: FBT001 -- parametrized fixture value
        expected_mode: str,
    ) -> None:
        with pytest.deprecated_call():
            mw = WasmshInterpreterMiddleware(
                sandbox_factory=_StubSandbox,
                snapshot_between_turns=legacy,
            )
        assert mw._mode == expected_mode

    def test_mode_and_deprecated_flag_together_are_rejected(self) -> None:
        # Resolving by precedence would silently discard half of what the
        # caller asked for, so the combination is an error instead.
        with pytest.raises(ValueError, match="not both"):
            WasmshInterpreterMiddleware(
                sandbox_factory=_StubSandbox,
                mode="call",
                snapshot_between_turns=True,
            )

    def test_max_ptc_calls_must_be_positive(self) -> None:
        with pytest.raises(ValueError, match="max_ptc_calls"):
            WasmshInterpreterMiddleware(sandbox_factory=_StubSandbox, max_ptc_calls=0)

    def test_serialized_name_is_stable(self) -> None:
        # Harness profiles exclude middleware by class or `.name`, and only a
        # class carrying `serialized_name` survives a config round-trip.
        assert (
            WasmshInterpreterMiddleware.serialized_name == "WasmshInterpreterMiddleware"
        )
        mw = WasmshInterpreterMiddleware(sandbox_factory=_StubSandbox)
        assert mw.name == "WasmshInterpreterMiddleware"


class TestRegistry:
    def test_get_returns_same_repl_for_same_thread(self) -> None:
        registry = _Registry(factory=_StubSandbox)
        a = registry.get("thread-1")
        b = registry.get("thread-1")
        assert a is b

    def test_evict_closes_sandbox(self) -> None:
        sandbox_holder: dict[str, _StubSandbox] = {}

        def factory() -> _StubSandbox:
            sandbox = _StubSandbox(execute_outputs=[_marker({"ok": True})])
            sandbox_holder["s"] = sandbox
            return sandbox

        registry = _Registry(factory=factory)
        repl = registry.get("thread-1")
        repl.eval_sync("pass")
        assert "s" in sandbox_holder
        assert sandbox_holder["s"].closed is False
        registry.evict("thread-1")
        assert sandbox_holder["s"].closed is True
