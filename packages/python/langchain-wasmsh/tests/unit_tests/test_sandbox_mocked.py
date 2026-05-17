"""Mocked unit tests for WasmshSandbox — no Deno or Pyodide assets needed."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any
from unittest.mock import MagicMock, patch

import pytest

from langchain_wasmsh.sandbox import WasmshSandbox

# ── Helpers ──────────────────────────────────────────────────────────────────


def _make_mock_process(
    responses: list[dict[str, Any]] | None = None,
) -> MagicMock:
    """Create a mock subprocess.Popen that responds to JSON-RPC requests.

    Production sandboxes now require ``id`` on the response to match the
    request, so the mock rewrites every dequeued response's ``id`` to match
    the most recent stdin write. Tests can keep using fixed-id placeholders.
    """
    process = MagicMock()
    process.poll.return_value = None  # process is alive
    process.stdin = MagicMock()
    process.stderr = MagicMock()
    process.stderr.read.return_value = ""

    queue: list[dict[str, Any]] = []
    if responses:
        queue.extend(responses)

    # Track the last id seen on stdin so dequeued responses can echo it.
    last_request_id: list[int | None] = [None]

    def _capture_stdin(line: str) -> int:
        try:
            parsed = json.loads(line.strip() or "{}")
        except json.JSONDecodeError:
            return len(line)
        if isinstance(parsed, dict) and "id" in parsed:
            last_request_id[0] = parsed["id"]
        return len(line)

    process.stdin.write.side_effect = _capture_stdin

    call_index = 0

    def _readline() -> str:
        nonlocal call_index
        if call_index < len(queue):
            response = dict(queue[call_index])  # shallow copy
            call_index += 1
            response["id"] = (
                last_request_id[0]
                if last_request_id[0] is not None
                else response.get("id")
            )
            return json.dumps(response) + "\n"
        # Fallback: empty string signals EOF to the sandbox reader, which
        # surfaces as RuntimeError("wasmsh host terminated unexpectedly").
        # Tests that need to keep the host alive across many calls should
        # extend `responses=` instead of relying on a sentinel reply here.
        return ""

    process.stdout = MagicMock()
    process.stdout.readline = MagicMock(side_effect=_readline)

    # Implicit init response — every WasmshSandbox sends init() at boot.
    queue.insert(0, {"ok": True, "result": {"events": []}})

    return process


def _create_sandbox(
    responses: list[dict[str, Any]] | None = None,
    **kwargs: Any,
) -> tuple[WasmshSandbox, MagicMock]:
    """Create a WasmshSandbox with a mocked subprocess."""
    process = _make_mock_process(responses)
    with (
        patch("shutil.which", return_value="/usr/bin/deno"),
        patch("subprocess.Popen", return_value=process),
    ):
        sandbox = WasmshSandbox(**kwargs)
    return sandbox, process


def _stdin_payloads(process: MagicMock) -> list[dict[str, Any]]:
    """Extract all JSON-RPC payloads written to stdin."""
    payloads = []
    for call in process.stdin.write.call_args_list:
        line = call[0][0]
        payloads.append(json.loads(line))
    return payloads


# ── Init / constructor tests ────────────────────────────────────────────────


class TestInit:
    def test_sends_init_request(self) -> None:
        _, process = _create_sandbox()
        payloads = _stdin_payloads(process)
        assert payloads[0]["method"] == "init"

    def test_passes_step_budget(self) -> None:
        _, process = _create_sandbox(step_budget=5000)
        init_params = _stdin_payloads(process)[0]["params"]
        assert init_params["stepBudget"] == 5000

    def test_passes_initial_files_encoded(self) -> None:
        _, process = _create_sandbox(initial_files={"/workspace/a.txt": b"hello"})
        init_params = _stdin_payloads(process)[0]["params"]
        files = init_params["initialFiles"]
        assert len(files) == 1
        assert files[0]["path"] == "/workspace/a.txt"
        assert files[0]["contentBase64"] == "aGVsbG8="

    def test_encodes_string_initial_files(self) -> None:
        _, process = _create_sandbox(initial_files={"/workspace/b.txt": "world"})
        init_params = _stdin_payloads(process)[0]["params"]
        files = init_params["initialFiles"]
        assert files[0]["contentBase64"] == "d29ybGQ="

    def test_default_step_budget_is_zero(self) -> None:
        _, process = _create_sandbox()
        init_params = _stdin_payloads(process)[0]["params"]
        assert init_params["stepBudget"] == 0

    def test_no_runtime_found_raises(self) -> None:
        with (
            patch("shutil.which", return_value=None),
            pytest.raises(FileNotFoundError, match="Neither deno nor node"),
        ):
            WasmshSandbox()

    def test_id_has_wasmsh_prefix(self) -> None:
        sandbox, _ = _create_sandbox()
        assert sandbox.id.startswith("wasmsh-python-")
        sandbox.close()


# ── Execute tests ────────────────────────────────────────────────────────────


class TestExecute:
    def test_prepends_cd_to_working_directory(self) -> None:
        run_response = {
            "id": 2,
            "ok": True,
            "result": {"output": "hi\n", "exitCode": 0},
        }
        sandbox, process = _create_sandbox(responses=[run_response])
        sandbox.execute("echo hi")
        payloads = _stdin_payloads(process)
        run_payload = payloads[1]
        assert run_payload["method"] == "run"
        assert run_payload["params"]["command"] == "cd /workspace && echo hi"
        sandbox.close()

    def test_custom_working_directory(self) -> None:
        run_response = {
            "id": 2,
            "ok": True,
            "result": {"output": "", "exitCode": 0},
        }
        sandbox, process = _create_sandbox(
            responses=[run_response],
            working_directory="/home/user",
        )
        sandbox.execute("ls")
        payloads = _stdin_payloads(process)
        assert "cd /home/user && ls" in payloads[1]["params"]["command"]
        sandbox.close()

    def test_returns_output_and_exit_code(self) -> None:
        run_response = {
            "id": 2,
            "ok": True,
            "result": {"output": "hello\n", "exitCode": 0},
        }
        sandbox, _ = _create_sandbox(responses=[run_response])
        result = sandbox.execute("echo hello")
        assert result.output == "hello\n"
        assert result.exit_code == 0
        assert result.truncated is False
        sandbox.close()

    def test_handles_non_zero_exit(self) -> None:
        run_response = {
            "id": 2,
            "ok": True,
            "result": {"output": "error\n", "exitCode": 1},
        }
        sandbox, _ = _create_sandbox(responses=[run_response])
        result = sandbox.execute("false")
        assert result.exit_code == 1
        sandbox.close()


# ── Upload files tests ───────────────────────────────────────────────────────


class TestUploadFiles:
    def test_encodes_content_as_base64(self) -> None:
        write_response = {"id": 2, "ok": True, "result": {"events": []}}
        sandbox, process = _create_sandbox(responses=[write_response])
        sandbox.upload_files([("/workspace/a.txt", b"data")])
        payloads = _stdin_payloads(process)
        write_payload = payloads[1]
        assert write_payload["method"] == "writeFile"
        assert write_payload["params"]["contentBase64"] == "ZGF0YQ=="
        sandbox.close()

    def test_rejects_relative_path(self) -> None:
        sandbox, _ = _create_sandbox()
        result = sandbox.upload_files([("relative.txt", b"data")])
        assert result[0].error == "invalid_path"
        sandbox.close()

    def test_maps_diagnostic_not_found(self) -> None:
        write_response = {
            "id": 2,
            "ok": True,
            "result": {"events": [{"Diagnostic": ["Error", "path not found"]}]},
        }
        sandbox, _ = _create_sandbox(responses=[write_response])
        result = sandbox.upload_files([("/workspace/a.txt", b"")])
        assert result[0].error == "file_not_found"
        sandbox.close()

    def test_maps_diagnostic_directory(self) -> None:
        write_response = {
            "id": 2,
            "ok": True,
            "result": {"events": [{"Diagnostic": ["Error", "is a directory"]}]},
        }
        sandbox, _ = _create_sandbox(responses=[write_response])
        result = sandbox.upload_files([("/workspace/dir", b"")])
        assert result[0].error == "is_directory"
        sandbox.close()

    def test_maps_exception_to_invalid_path(self) -> None:
        sandbox, process = _create_sandbox()
        # readline returns "" for the upload (simulating host crash) then "" again
        # so close() also sees EOF and exits cleanly.
        process.stdout.readline.side_effect = ["", ""]
        result = sandbox.upload_files([("/workspace/a.txt", b"")])
        assert result[0].error is not None
        sandbox.close()


# ── Download files tests ─────────────────────────────────────────────────────


def _run_ok(output: str = "") -> dict[str, Any]:
    """Mock response for an execute/run call (used by download_files pre-check)."""
    return {
        "id": 0,  # id is ignored by mock
        "ok": True,
        "result": {
            "events": [],
            "stdout": output,
            "stderr": "",
            "output": output,
            "exitCode": 0,
        },
    }


class TestDownloadFiles:
    def test_decodes_base64_content(self) -> None:
        read_response = {
            "id": 3,
            "ok": True,
            "result": {"events": [], "contentBase64": "aGVsbG8="},
        }
        sandbox, _ = _create_sandbox(responses=[_run_ok(), read_response])
        result = sandbox.download_files(["/workspace/a.txt"])
        assert result[0].error is None
        assert result[0].content == b"hello"
        sandbox.close()

    def test_rejects_relative_path(self) -> None:
        sandbox, _ = _create_sandbox()
        result = sandbox.download_files(["relative.txt"])
        assert result[0].error == "invalid_path"
        assert result[0].content is None
        sandbox.close()

    def test_maps_diagnostic_not_found(self) -> None:
        read_response = {
            "id": 3,
            "ok": True,
            "result": {
                "events": [{"Diagnostic": ["Error", "not found"]}],
                "contentBase64": "",
            },
        }
        sandbox, _ = _create_sandbox(responses=[_run_ok(), read_response])
        result = sandbox.download_files(["/workspace/missing.txt"])
        assert result[0].error == "file_not_found"
        assert result[0].content is None
        sandbox.close()

    def test_maps_diagnostic_directory(self) -> None:
        # Pre-check detects directory via execute()
        sandbox, _ = _create_sandbox(responses=[_run_ok("DIR\n")])
        result = sandbox.download_files(["/workspace/dir"])
        assert result[0].error == "is_directory"
        sandbox.close()

    def test_maps_diagnostic_permission(self) -> None:
        read_response = {
            "id": 3,
            "ok": True,
            "result": {
                "events": [{"Diagnostic": ["Error", "permission denied"]}],
                "contentBase64": "",
            },
        }
        sandbox, _ = _create_sandbox(responses=[_run_ok(), read_response])
        result = sandbox.download_files(["/workspace/secret"])
        assert result[0].error == "permission_denied"
        sandbox.close()

    def test_maps_exception_to_error(self) -> None:
        sandbox, process = _create_sandbox()
        # Pre-check + readFile both fail; close() also sees EOF.
        process.stdout.readline.side_effect = ["", "", ""]
        result = sandbox.download_files(["/workspace/a.txt"])
        assert result[0].error is not None
        assert result[0].content is None
        sandbox.close()


# ── Close / lifecycle tests ──────────────────────────────────────────────────


class TestClose:
    def test_sends_close_request(self) -> None:
        close_response = {"id": 2, "ok": True, "result": {"closed": True}}
        sandbox, process = _create_sandbox(responses=[close_response])
        sandbox.close()
        payloads = _stdin_payloads(process)
        assert payloads[-1]["method"] == "close"

    def test_terminates_process(self) -> None:
        close_response = {"id": 2, "ok": True, "result": {"closed": True}}
        sandbox, process = _create_sandbox(responses=[close_response])
        sandbox.close()
        process.terminate.assert_called_once()
        process.wait.assert_called_once()

    def test_idempotent(self) -> None:
        close_response = {"id": 2, "ok": True, "result": {"closed": True}}
        sandbox, process = _create_sandbox(responses=[close_response])
        # After close, poll returns exit code (process dead)
        process.poll.side_effect = [None, 0, 0]
        sandbox.close()
        sandbox.close()  # should not raise
        process.terminate.assert_called_once()

    def test_stop_delegates_to_close(self) -> None:
        close_response = {"id": 2, "ok": True, "result": {"closed": True}}
        sandbox, process = _create_sandbox(responses=[close_response])
        sandbox.stop()
        process.terminate.assert_called_once()


# ── Error handling tests ─────────────────────────────────────────────────────


class TestErrorHandling:
    def test_host_unexpected_termination(self) -> None:
        sandbox, process = _create_sandbox()
        # Empty readline for execute(); another empty for close() to no-op.
        process.stdout.readline.side_effect = ["", ""]
        with pytest.raises(RuntimeError):
            sandbox.execute("echo")
        sandbox.close()

    def test_host_error_response(self) -> None:
        sandbox, process = _create_sandbox()
        # execute() request gets id=2 (init was 1). Echo that id back so the
        # error response matches the read loop's expected id.
        error_response = {"id": 2, "ok": False, "error": "something went wrong"}
        process.stdout.readline.side_effect = [
            json.dumps(error_response) + "\n",
            "",  # close() sees EOF and exits cleanly
        ]
        with pytest.raises(RuntimeError, match="something went wrong"):
            sandbox.execute("echo")
        sandbox.close()


class TestAssetPathResolution:
    """Symlinked checkouts must still produce a sandbox Deno will allow.

    Deno prefix-matches ``--allow-read`` against the canonical realpath the
    filesystem returns, so the sandbox has to resolve symlinks before
    handing the directory to Deno. Otherwise the first read of
    ``pyodide.asm.js`` raises a permission error — see the regression on
    venvs installed via a symlinked source tree.
    """

    def test_dist_dir_is_canonicalised(self, tmp_path: Any) -> None:
        # Create a real asset dir and a symlinked alias.
        real_assets = tmp_path / "real" / "assets"
        real_assets.mkdir(parents=True)
        (real_assets / "pyodide.asm.js").write_text("// stub\n")
        (real_assets / "node-host.mjs").write_text("// stub\n")
        link_root = tmp_path / "link"
        link_root.symlink_to(tmp_path / "real")
        symlinked_assets = link_root / "assets"

        with patch("langchain_wasmsh.sandbox.subprocess.Popen") as popen:
            process = _make_mock_process(
                responses=[{"id": 1, "ok": True, "result": {"events": []}}],
            )
            popen.return_value = process
            # Don't let the integration go further than constructor —
            # close() reads the EOF and exits cleanly.
            process.stdout.readline.side_effect = [
                json.dumps({"id": 1, "ok": True, "result": {"events": []}}) + "\n",
                "",
            ]
            sandbox = WasmshSandbox(dist_dir=symlinked_assets, runtime="deno")
            sandbox.close()

        cmd = popen.call_args.args[0]
        # The --allow-read prefix and --asset-dir must point to the
        # canonical path, not the symlinked input. Otherwise Deno would
        # deny when pyodide.asm.js loads.
        canonical = str(Path(real_assets).resolve())
        allow_read = next(arg for arg in cmd if arg.startswith("--allow-read="))
        assert allow_read == f"--allow-read={canonical}"
        # --asset-dir is the value after the literal flag.
        asset_dir_idx = cmd.index("--asset-dir")
        assert cmd[asset_dir_idx + 1] == canonical
