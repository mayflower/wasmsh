"""Integration tests for WasmshRemoteSandbox against a live dispatcher.

Requires a running wasmsh dispatcher reachable at `WASMSH_DISPATCHER_URL`
and at least one runner bound to it. The stack produced by
`deploy/docker/compose.dispatcher-test.yml` is the canonical fixture.

The same two suite deviations apply here as in `test_integration.py`, for
the same reasons — the remote runner executes the identical wasmsh shell, so
a conformance gap on one transport is a gap on both. See that module's
docstring for the reasoning; this one only restates the mechanics.
"""

from __future__ import annotations

import os
from typing import TYPE_CHECKING

import pytest
from langchain_tests.integration_tests import SandboxIntegrationTests

from langchain_wasmsh import WasmshRemoteSandbox
from tests.integration_tests.test_integration import enforces_permissions

if TYPE_CHECKING:
    from collections.abc import Iterator

    from deepagents.backends.protocol import SandboxBackendProtocol

_DISPATCHER_URL = os.environ.get("WASMSH_DISPATCHER_URL")


@pytest.mark.skipif(
    not _DISPATCHER_URL,
    reason="set WASMSH_DISPATCHER_URL to a running dispatcher to enable",
)
class TestWasmshRemoteSandboxStandard(SandboxIntegrationTests):
    @pytest.fixture(scope="class")
    def sandbox(self) -> Iterator[SandboxBackendProtocol]:
        assert _DISPATCHER_URL is not None
        backend = WasmshRemoteSandbox(_DISPATCHER_URL)
        try:
            yield backend
        finally:
            backend.close()

    @pytest.fixture(autouse=True)
    def _skip_without_permission_support(
        self,
        request: pytest.FixtureRequest,
        sandbox_backend: SandboxBackendProtocol,
    ) -> None:
        """Skip the permission test on a runner that cannot enforce modes.

        The runner image may still be built from a wasmsh revision predating
        the VFS permission model; probe rather than assume, so this clears
        itself once the image is rebuilt.
        """
        if request.node.name != "test_download_error_permission_denied":
            return
        if not enforces_permissions(sandbox_backend):
            pytest.skip(
                "dispatcher runner predates VFS permission enforcement; "
                "rebuild the runner image from a current wasmsh revision",
            )

    @pytest.mark.xfail(
        reason=(
            "langchain-tests 1.1.9 asserts pre-0.7 create-only write; every "
            "deepagents 0.7.x backend overwrites by design. Covered instead "
            "by TestRemoteWriteOverwriteContract."
        ),
        strict=True,
    )
    def test_write_existing_file_fails(
        self,
        sandbox_backend: SandboxBackendProtocol,
        sandbox_test_root: str,
    ) -> None:
        super().test_write_existing_file_fails(sandbox_backend, sandbox_test_root)


@pytest.mark.skipif(
    not _DISPATCHER_URL,
    reason="set WASMSH_DISPATCHER_URL to a running dispatcher to enable",
)
class TestRemoteWriteOverwriteContract:
    """The 0.7.x `write` contract the stale suite assertion contradicts."""

    @pytest.fixture
    def sandbox(self) -> Iterator[WasmshRemoteSandbox]:
        assert _DISPATCHER_URL is not None
        backend = WasmshRemoteSandbox(_DISPATCHER_URL)
        try:
            yield backend
        finally:
            backend.close()

    def test_write_replaces_existing_content(
        self,
        sandbox: WasmshRemoteSandbox,
    ) -> None:
        path = "/tmp/overwrite/remote.txt"  # noqa: S108 -- sandbox VFS, not the host
        assert sandbox.write(path, "First content").error is None

        result = sandbox.write(path, "Second content")

        assert result.error is None
        assert result.path == path
        assert sandbox.execute(f"cat {path}").output.strip() == "Second content"
