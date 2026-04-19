"""Integration tests for WasmshRemoteSandbox against a live dispatcher.

Requires a running wasmsh dispatcher reachable at `WASMSH_DISPATCHER_URL`
and at least one runner bound to it. The stack produced by
`deploy/docker/compose.dispatcher-test.yml` is the canonical fixture.
"""

from __future__ import annotations

import os
from typing import TYPE_CHECKING

import pytest
from langchain_tests.integration_tests import SandboxIntegrationTests

from langchain_wasmsh import WasmshRemoteSandbox

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

    @pytest.mark.xfail(
        reason=(
            "Emscripten VFS does not enforce chmod — permissions are silently ignored"
        ),
    )
    def test_download_error_permission_denied(
        self,
        sandbox_backend: SandboxBackendProtocol,
    ) -> None:
        super().test_download_error_permission_denied(sandbox_backend)
