"""Unit tests for WasmshRemoteSandbox — dispatcher HTTP fully mocked."""

from __future__ import annotations

import base64
import json
from typing import Any

import httpx
import pytest
import respx

from langchain_wasmsh import WasmshRemoteSandbox

BASE_URL = "http://dispatcher.test"
SESSION_ID = "test-session"


def _create_session_route(router: respx.MockRouter) -> respx.Route:
    return router.post(f"{BASE_URL}/sessions").mock(
        return_value=httpx.Response(
            201,
            json={
                "ok": True,
                "session": {
                    "sessionId": SESSION_ID,
                    "workerId": "worker-1",
                    "restoreMetrics": {},
                    "init": {"workerId": "worker-1"},
                },
            },
        ),
    )


def _make_sandbox(
    router: respx.MockRouter,
    **kwargs: Any,
) -> WasmshRemoteSandbox:
    _create_session_route(router)
    return WasmshRemoteSandbox(BASE_URL, session_id=SESSION_ID, **kwargs)


# ── Construction ───────────────────────────────────────────────────────────


class TestInit:
    @respx.mock
    def test_posts_session_with_payload(self) -> None:
        route = _create_session_route(respx.mock)
        WasmshRemoteSandbox(
            BASE_URL,
            session_id=SESSION_ID,
            allowed_hosts=["pypi.org"],
            step_budget=42,
            initial_files={"/workspace/a.txt": "hi"},
        )
        assert route.called
        payload = json.loads(route.calls[0].request.content)
        assert payload["session_id"] == SESSION_ID
        assert payload["allowed_hosts"] == ["pypi.org"]
        assert payload["step_budget"] == 42
        assert payload["initial_files"] == [
            {
                "path": "/workspace/a.txt",
                "contentBase64": base64.b64encode(b"hi").decode("ascii"),
            },
        ]

    @respx.mock
    def test_defaults_are_empty(self) -> None:
        route = _create_session_route(respx.mock)
        WasmshRemoteSandbox(BASE_URL, session_id=SESSION_ID)
        payload = json.loads(route.calls[0].request.content)
        assert payload["allowed_hosts"] == []
        assert payload["step_budget"] == 0
        assert payload["initial_files"] == []

    @respx.mock
    def test_server_assigned_session_id_wins(self) -> None:
        respx.mock.post(f"{BASE_URL}/sessions").mock(
            return_value=httpx.Response(
                201,
                json={
                    "ok": True,
                    "session": {"sessionId": "server-chosen"},
                },
            ),
        )
        sandbox = WasmshRemoteSandbox(BASE_URL)
        assert sandbox.id == "server-chosen"

    @respx.mock
    def test_strips_trailing_slash(self) -> None:
        route = respx.mock.post("http://dispatcher.test/sessions").mock(
            return_value=httpx.Response(
                201,
                json={"ok": True, "session": {"sessionId": SESSION_ID}},
            ),
        )
        WasmshRemoteSandbox("http://dispatcher.test/", session_id=SESSION_ID)
        assert route.called

    @respx.mock
    def test_failure_closes_owned_client(self) -> None:
        respx.mock.post(f"{BASE_URL}/sessions").mock(
            return_value=httpx.Response(
                503,
                json={"ok": False, "error": "no runner available"},
            ),
        )
        with pytest.raises(RuntimeError, match="no runner available"):
            WasmshRemoteSandbox(BASE_URL, session_id=SESSION_ID)


# ── execute ────────────────────────────────────────────────────────────────


class TestExecute:
    @respx.mock
    def test_posts_run_with_cd_prefix(self) -> None:
        sandbox = _make_sandbox(respx.mock)
        run_route = respx.mock.post(f"{BASE_URL}/sessions/{SESSION_ID}/run").mock(
            return_value=httpx.Response(
                200,
                json={
                    "ok": True,
                    "sessionId": SESSION_ID,
                    "result": {"output": "hello\n", "exitCode": 0},
                },
            ),
        )
        result = sandbox.execute("echo hello")
        body = json.loads(run_route.calls[0].request.content)
        assert body["command"] == "cd /workspace && echo hello"
        assert result.output == "hello\n"
        assert result.exit_code == 0

    @respx.mock
    def test_custom_working_directory(self) -> None:
        sandbox = _make_sandbox(respx.mock, working_directory="/other")
        run_route = respx.mock.post(f"{BASE_URL}/sessions/{SESSION_ID}/run").mock(
            return_value=httpx.Response(
                200,
                json={"ok": True, "result": {"output": "", "exitCode": 0}},
            ),
        )
        sandbox.execute("pwd")
        body = json.loads(run_route.calls[0].request.content)
        assert body["command"].startswith("cd /other && ")

    @respx.mock
    def test_surfaces_non_zero_exit(self) -> None:
        sandbox = _make_sandbox(respx.mock)
        respx.mock.post(f"{BASE_URL}/sessions/{SESSION_ID}/run").mock(
            return_value=httpx.Response(
                200,
                json={
                    "ok": True,
                    "result": {"output": "not-found\n", "exitCode": 127},
                },
            ),
        )
        result = sandbox.execute("nope")
        assert result.exit_code == 127


# ── upload_files ───────────────────────────────────────────────────────────


class TestUploadFiles:
    @respx.mock
    def test_encodes_content_as_base64(self) -> None:
        sandbox = _make_sandbox(respx.mock)
        write_route = respx.mock.post(
            f"{BASE_URL}/sessions/{SESSION_ID}/write-file",
        ).mock(
            return_value=httpx.Response(
                200,
                json={"ok": True, "result": {"events": []}},
            ),
        )
        result = sandbox.upload_files([("/workspace/x.txt", b"hello")])
        body = json.loads(write_route.calls[0].request.content)
        assert body["path"] == "/workspace/x.txt"
        assert base64.b64decode(body["contentBase64"]) == b"hello"
        assert result[0].error is None

    @respx.mock
    def test_rejects_relative_path_without_request(self) -> None:
        sandbox = _make_sandbox(respx.mock)
        write_route = respx.mock.post(
            f"{BASE_URL}/sessions/{SESSION_ID}/write-file",
        )
        result = sandbox.upload_files([("relative.txt", b"x")])
        assert result[0].error == "invalid_path"
        assert not write_route.called

    @respx.mock
    def test_maps_diagnostic_to_error(self) -> None:
        sandbox = _make_sandbox(respx.mock)
        respx.mock.post(f"{BASE_URL}/sessions/{SESSION_ID}/write-file").mock(
            return_value=httpx.Response(
                200,
                json={
                    "ok": True,
                    "result": {
                        "events": [{"Diagnostic": ["error", "permission denied"]}],
                    },
                },
            ),
        )
        result = sandbox.upload_files([("/readonly/x", b"x")])
        assert result[0].error == "permission_denied"


# ── download_files ─────────────────────────────────────────────────────────


class TestDownloadFiles:
    @respx.mock
    def test_decodes_base64_content(self) -> None:
        sandbox = _make_sandbox(respx.mock)
        # The directory pre-check calls `execute("test -d ...")` — stub it out
        # so it reports "not a directory".
        respx.mock.post(f"{BASE_URL}/sessions/{SESSION_ID}/run").mock(
            return_value=httpx.Response(
                200,
                json={"ok": True, "result": {"output": "\n", "exitCode": 0}},
            ),
        )
        respx.mock.post(f"{BASE_URL}/sessions/{SESSION_ID}/read-file").mock(
            return_value=httpx.Response(
                200,
                json={
                    "ok": True,
                    "result": {
                        "contentBase64": base64.b64encode(b"hello").decode("ascii"),
                        "events": [],
                    },
                },
            ),
        )
        [resp] = sandbox.download_files(["/workspace/hello.txt"])
        assert resp.error is None
        assert resp.content == b"hello"

    @respx.mock
    def test_detects_directory_via_precheck(self) -> None:
        sandbox = _make_sandbox(respx.mock)
        # Pre-check returns "DIR".
        respx.mock.post(f"{BASE_URL}/sessions/{SESSION_ID}/run").mock(
            return_value=httpx.Response(
                200,
                json={"ok": True, "result": {"output": "DIR\n", "exitCode": 0}},
            ),
        )
        read_route = respx.mock.post(f"{BASE_URL}/sessions/{SESSION_ID}/read-file")
        [resp] = sandbox.download_files(["/workspace"])
        assert resp.error == "is_directory"
        assert not read_route.called

    @respx.mock
    def test_rejects_relative_path(self) -> None:
        sandbox = _make_sandbox(respx.mock)
        [resp] = sandbox.download_files(["nope.txt"])
        assert resp.error == "invalid_path"

    @respx.mock
    def test_maps_404_to_file_not_found(self) -> None:
        sandbox = _make_sandbox(respx.mock)
        respx.mock.post(f"{BASE_URL}/sessions/{SESSION_ID}/run").mock(
            return_value=httpx.Response(
                200,
                json={"ok": True, "result": {"output": "\n", "exitCode": 0}},
            ),
        )
        respx.mock.post(f"{BASE_URL}/sessions/{SESSION_ID}/read-file").mock(
            return_value=httpx.Response(
                404,
                json={"ok": False, "error": "file not found: /workspace/missing"},
            ),
        )
        [resp] = sandbox.download_files(["/workspace/missing"])
        assert resp.error == "file_not_found"


# ── close / stop ───────────────────────────────────────────────────────────


class TestClose:
    @respx.mock
    def test_sends_close_and_delete(self) -> None:
        sandbox = _make_sandbox(respx.mock)
        close_route = respx.mock.post(
            f"{BASE_URL}/sessions/{SESSION_ID}/close",
        ).mock(return_value=httpx.Response(200, json={"ok": True, "result": {}}))
        delete_route = respx.mock.delete(
            f"{BASE_URL}/sessions/{SESSION_ID}",
        ).mock(return_value=httpx.Response(200, json={"ok": True, "result": {}}))
        sandbox.close()
        assert close_route.called
        assert delete_route.called

    @respx.mock
    def test_close_is_idempotent(self) -> None:
        sandbox = _make_sandbox(respx.mock)
        close_route = respx.mock.post(
            f"{BASE_URL}/sessions/{SESSION_ID}/close",
        ).mock(return_value=httpx.Response(200, json={"ok": True, "result": {}}))
        respx.mock.delete(f"{BASE_URL}/sessions/{SESSION_ID}").mock(
            return_value=httpx.Response(200, json={"ok": True, "result": {}}),
        )
        sandbox.close()
        sandbox.close()
        assert close_route.call_count == 1

    @respx.mock
    def test_close_swallows_dispatcher_errors(self) -> None:
        sandbox = _make_sandbox(respx.mock)
        respx.mock.post(f"{BASE_URL}/sessions/{SESSION_ID}/close").mock(
            return_value=httpx.Response(503, json={"ok": False, "error": "down"}),
        )
        respx.mock.delete(f"{BASE_URL}/sessions/{SESSION_ID}").mock(
            side_effect=httpx.ConnectError("refused"),
        )
        # Must not raise — close() is a "best effort" teardown.
        sandbox.close()

    @respx.mock
    def test_stop_delegates_to_close(self) -> None:
        sandbox = _make_sandbox(respx.mock)
        close_route = respx.mock.post(
            f"{BASE_URL}/sessions/{SESSION_ID}/close",
        ).mock(return_value=httpx.Response(200, json={"ok": True, "result": {}}))
        respx.mock.delete(f"{BASE_URL}/sessions/{SESSION_ID}").mock(
            return_value=httpx.Response(200, json={"ok": True, "result": {}}),
        )
        sandbox.stop()
        assert close_route.called

    @respx.mock
    def test_context_manager_closes_on_exit(self) -> None:
        respx.mock.post(f"{BASE_URL}/sessions").mock(
            return_value=httpx.Response(
                201,
                json={"ok": True, "session": {"sessionId": SESSION_ID}},
            ),
        )
        close_route = respx.mock.post(
            f"{BASE_URL}/sessions/{SESSION_ID}/close",
        ).mock(return_value=httpx.Response(200, json={"ok": True, "result": {}}))
        respx.mock.delete(f"{BASE_URL}/sessions/{SESSION_ID}").mock(
            return_value=httpx.Response(200, json={"ok": True, "result": {}}),
        )
        with WasmshRemoteSandbox(BASE_URL, session_id=SESSION_ID):
            pass
        assert close_route.called


# ── edit ───────────────────────────────────────────────────────────────────


class TestEdit:
    """Edit is upstream's temp-file route: two uploads plus one `execute`."""

    @respx.mock
    def test_uploads_payload_and_runs_server_side_script(self) -> None:
        sandbox = _make_sandbox(respx.mock)
        run_route = respx.mock.post(f"{BASE_URL}/sessions/{SESSION_ID}/run").mock(
            return_value=httpx.Response(
                200,
                json={"ok": True, "result": {"output": '{"count": 1}', "exitCode": 0}},
            ),
        )
        write_route = respx.mock.post(
            f"{BASE_URL}/sessions/{SESSION_ID}/write-file",
        ).mock(return_value=httpx.Response(200, json={"ok": True, "result": {}}))

        result = sandbox.edit("/workspace/a.txt", "world", "there")

        assert result.error is None
        assert result.occurrences == 1
        assert result.path == "/workspace/a.txt"
        # old_string and new_string travel as temp files, never as stdin —
        # wasmsh's in-process `python3` never receives the shell's stdin.
        uploaded = [
            base64.b64decode(json.loads(call.request.content)["contentBase64"])
            for call in write_route.calls
        ]
        assert uploaded == [b"world", b"there"]
        assert run_route.called

    @respx.mock
    def test_maps_upstream_error_codes(self) -> None:
        sandbox = _make_sandbox(respx.mock)
        respx.mock.post(f"{BASE_URL}/sessions/{SESSION_ID}/run").mock(
            return_value=httpx.Response(
                200,
                json={
                    "ok": True,
                    "result": {
                        "output": '{"error": "multiple_occurrences", "count": 3}',
                        "exitCode": 0,
                    },
                },
            ),
        )
        respx.mock.post(f"{BASE_URL}/sessions/{SESSION_ID}/write-file").mock(
            return_value=httpx.Response(200, json={"ok": True, "result": {}}),
        )

        result = sandbox.edit("/workspace/a.txt", "a", "b")

        assert result.error is not None
        assert "replace_all=True" in result.error

    @respx.mock
    def test_rejects_known_binary_extension_without_touching_the_sandbox(
        self,
    ) -> None:
        sandbox = _make_sandbox(respx.mock)
        run_route = respx.mock.post(f"{BASE_URL}/sessions/{SESSION_ID}/run").mock(
            return_value=httpx.Response(
                200,
                json={"ok": True, "result": {"output": '{"count": 1}', "exitCode": 0}},
            ),
        )

        result = sandbox.edit("/workspace/logo.png", "a", "b")

        assert result.error is not None
        assert "not a text file" in result.error
        assert not run_route.called


# ── read ───────────────────────────────────────────────────────────────────


class TestRead:
    """Read runs upstream's server-side pagination script through `execute`."""

    @staticmethod
    def _run_returns(payload: dict[str, Any]) -> respx.Route:
        return respx.mock.post(f"{BASE_URL}/sessions/{SESSION_ID}/run").mock(
            return_value=httpx.Response(
                200,
                json={
                    "ok": True,
                    "result": {"output": json.dumps(payload), "exitCode": 0},
                },
            ),
        )

    @respx.mock
    def test_text_returns_file_data_with_pagination_metadata(self) -> None:
        sandbox = _make_sandbox(respx.mock)
        self._run_returns(
            {
                "encoding": "utf-8",
                "content": "line1\nline2",
                "total_lines": 2,
                "start_line": 1,
                "end_line": 2,
                "next_offset": None,
            },
        )

        result = sandbox.read("/workspace/a.txt")

        assert result.error is None
        assert result.file_data is not None
        assert result.file_data["encoding"] == "utf-8"
        assert result.file_data["content"] == "line1\nline2"
        assert (result.total_lines, result.start_line, result.end_line) == (2, 1, 2)
        assert result.next_offset is None

    @respx.mock
    def test_empty_file_returns_reminder(self) -> None:
        sandbox = _make_sandbox(respx.mock)
        self._run_returns(
            {
                "encoding": "utf-8",
                "content": "System reminder: File exists but has empty contents",
            },
        )

        result = sandbox.read("/workspace/empty.txt")

        assert result.file_data is not None
        assert "empty contents" in result.file_data["content"]
        assert result.no_lines_requested is False

    @respx.mock
    def test_zero_limit_flags_no_lines_requested(self) -> None:
        sandbox = _make_sandbox(respx.mock)
        self._run_returns(
            {"encoding": "utf-8", "content": "", "no_lines_requested": True},
        )

        result = sandbox.read("/workspace/a.txt", limit=0)

        assert result.no_lines_requested is True
        assert result.start_line is None

    @respx.mock
    def test_missing_file_returns_error(self) -> None:
        sandbox = _make_sandbox(respx.mock)
        self._run_returns({"error": "file_not_found"})

        result = sandbox.read("/missing")

        assert result.error is not None
        assert "file_not_found" in result.error


# ── grep ───────────────────────────────────────────────────────────────────


class TestGrep:
    """Grep runs the wasmsh-safe script and parses upstream's record format."""

    @staticmethod
    def _run_returns(output: str) -> respx.Route:
        return respx.mock.post(f"{BASE_URL}/sessions/{SESSION_ID}/run").mock(
            return_value=httpx.Response(
                200,
                json={"ok": True, "result": {"output": output, "exitCode": 0}},
            ),
        )

    @respx.mock
    def test_parses_nul_separated_records(self) -> None:
        sandbox = _make_sandbox(respx.mock)
        self._run_returns("/w/a.txt\x0012:hit here\n")

        result = sandbox.grep("hit")

        assert result.error is None
        assert result.matches == [
            {"path": "/w/a.txt", "line": 12, "text": "hit here"},
        ]
        assert result.truncated is False

    @respx.mock
    def test_max_count_marks_truncated(self) -> None:
        sandbox = _make_sandbox(respx.mock)
        # The script deliberately emits one record past the cap so the
        # parser can tell "exactly at the cap" from "capped early".
        self._run_returns("/w/a.txt\x001:x\n/w/a.txt\x002:x\n")

        result = sandbox.grep("x", max_count=1)

        assert result.matches is not None
        assert len(result.matches) == 1
        assert result.truncated is True

    @respx.mock
    def test_nonzero_exit_is_an_error(self) -> None:
        sandbox = _make_sandbox(respx.mock)
        respx.mock.post(f"{BASE_URL}/sessions/{SESSION_ID}/run").mock(
            return_value=httpx.Response(
                200,
                json={
                    "ok": True,
                    "result": {"output": "boom", "exitCode": 2},
                },
            ),
        )

        result = sandbox.grep("x", path="/missing")

        assert result.error is not None
        assert result.matches is None
