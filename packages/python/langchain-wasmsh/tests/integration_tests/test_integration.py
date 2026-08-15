"""The exact `langchain-tests==1.1.9` sandbox conformance suite.

Run once per local host runtime (Node and Deno) so a divergence between the
two — Deno's permission model restricts the subprocess differently — cannot
hide behind a single green leg.

Two deviations from the suite are expected and each is scoped to one test:

`test_download_error_permission_denied`
    Emscripten's VFS accepts `chmod` and then ignores it, so a
    permission-denied read cannot be provoked at all.

`test_write_existing_file_fails`
    The suite still asserts Deep Agents' pre-0.7 create-only `write`.
    `deepagents==0.7.4` changed `BaseSandbox.write` to overwrite (its
    preflight only creates parent directories), so *every* 0.7.4 sandbox
    fails this assertion, including upstream's own backends. Verified
    directly against `deepagents.backends.local_shell.LocalShellBackend`.
"""

from __future__ import annotations

import shutil
from typing import TYPE_CHECKING

import pytest
from langchain_tests.integration_tests import SandboxIntegrationTests

from langchain_wasmsh import WasmshSandbox

if TYPE_CHECKING:
    from collections.abc import Iterator

    from deepagents.backends.protocol import SandboxBackendProtocol

try:
    from wasmsh_pyodide_runtime import get_dist_dir

    _assets_available = get_dist_dir().joinpath("pyodide.asm.wasm").exists()
except (ImportError, FileNotFoundError):
    _assets_available = False

_ASSETS_REASON = (
    "Pyodide assets not built (run just build-pyodide && just package-pyodide-runtime)"
)

_WRITE_OVERWRITE_REASON = (
    "langchain-tests 1.1.9 still asserts pre-0.7 create-only write; "
    "deepagents 0.7.4 BaseSandbox.write overwrites by design"
)

_CHMOD_REASON = (
    "Emscripten VFS does not enforce chmod — permissions are silently ignored"
)


@pytest.mark.skipif(not _assets_available, reason=_ASSETS_REASON)
class _WasmshStandardSuite(SandboxIntegrationTests):
    """Shared body of the standard suite; subclasses pick the host runtime."""

    runtime: str

    @pytest.fixture(scope="class")
    def sandbox(self) -> Iterator[SandboxBackendProtocol]:
        backend = WasmshSandbox(runtime=self.runtime)
        try:
            yield backend
        finally:
            backend.close()

    @pytest.mark.xfail(reason=_CHMOD_REASON, strict=True)
    def test_download_error_permission_denied(
        self,
        sandbox_backend: SandboxBackendProtocol,
    ) -> None:
        super().test_download_error_permission_denied(sandbox_backend)

    @pytest.mark.xfail(reason=_WRITE_OVERWRITE_REASON, strict=True)
    def test_write_existing_file_fails(
        self,
        sandbox_backend: SandboxBackendProtocol,
        sandbox_test_root: str,
    ) -> None:
        super().test_write_existing_file_fails(sandbox_backend, sandbox_test_root)


@pytest.mark.skipif(shutil.which("node") is None, reason="node is not installed")
class TestWasmshSandboxStandardNode(_WasmshStandardSuite):
    runtime = "node"


@pytest.mark.skipif(shutil.which("deno") is None, reason="deno is not installed")
class TestWasmshSandboxStandardDeno(_WasmshStandardSuite):
    runtime = "deno"
