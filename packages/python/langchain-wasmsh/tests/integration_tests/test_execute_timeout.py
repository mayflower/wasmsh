"""`execute(timeout=N)` as a real deadline, not an advisory number.

Pyodide runs the shell synchronously inside the WebAssembly module and the
host has no safe cancellation point, so the deadline cannot be enforced by
interrupting the command. It is enforced the only honest way left: kill the
host, report GNU `timeout(1)`'s exit code 124, and refuse every later call
on that sandbox.

Refusing later calls is the load-bearing part. A session whose interpreter
is still executing an abandoned command has unknowable state, and quietly
continuing to read files out of it would hand the model a plausible answer
built on a half-finished write.
"""

from __future__ import annotations

import time

import pytest

from langchain_wasmsh import (
    WasmshSandbox,  # noqa: TC001 -- used as a runtime fixture annotation
)
from langchain_wasmsh._file_ops import TIMEOUT_EXIT_CODE
from langchain_wasmsh.sandbox import WasmshSessionTerminatedError
from tests.integration_tests._harness import requires_assets

pytestmark = requires_assets

SLOW_COMMAND = 'python3 -c "import time; time.sleep(30)"'


class TestExecuteTimeout:
    def test_a_missed_deadline_reports_124_without_waiting_for_the_command(
        self,
        sandbox: WasmshSandbox,
    ) -> None:
        started = time.monotonic()
        result = sandbox.execute(SLOW_COMMAND, timeout=2)
        elapsed = time.monotonic() - started

        assert result.exit_code == TIMEOUT_EXIT_CODE
        assert "timed out after 2s" in result.output
        # The 30-second command did not run to completion.
        assert elapsed < 15

    def test_the_session_is_unusable_afterwards(
        self,
        sandbox: WasmshSandbox,
    ) -> None:
        sandbox.execute(SLOW_COMMAND, timeout=2)
        with pytest.raises(WasmshSessionTerminatedError, match="destroyed"):
            sandbox.execute("echo still here")

    def test_file_operations_after_a_timeout_report_errors_not_stale_data(
        self,
        sandbox: WasmshSandbox,
    ) -> None:
        sandbox.upload_files([("/workspace/before.txt", b"written before")])
        sandbox.execute(SLOW_COMMAND, timeout=2)

        # Per-file error responses rather than an exception, matching the
        # partial-success contract of the transfer APIs.
        downloaded = sandbox.download_files(["/workspace/before.txt"])
        assert downloaded[0].error is not None
        assert downloaded[0].content is None

    def test_a_command_that_finishes_in_time_is_unaffected(
        self,
        sandbox: WasmshSandbox,
    ) -> None:
        result = sandbox.execute("echo quick", timeout=30)
        assert result.exit_code == 0
        assert "quick" in result.output
        # No watchdog residue: the session keeps working.
        assert sandbox.execute("echo again").exit_code == 0

    @pytest.mark.parametrize("timeout", [None, 0])
    def test_no_deadline_means_no_watchdog(
        self,
        sandbox: WasmshSandbox,
        timeout: int | None,
    ) -> None:
        # `None` is "backend default" and `0` is "no timeout" in the protocol.
        result = sandbox.execute("echo unbounded", timeout=timeout)
        assert result.exit_code == 0
        assert "unbounded" in result.output

    async def test_the_async_path_enforces_the_same_deadline(
        self,
        sandbox: WasmshSandbox,
    ) -> None:
        result = await sandbox.aexecute(SLOW_COMMAND, timeout=2)
        assert result.exit_code == TIMEOUT_EXIT_CODE
