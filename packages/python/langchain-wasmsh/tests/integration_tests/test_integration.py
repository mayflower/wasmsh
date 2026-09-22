"""The exact `langchain-tests==1.1.9` sandbox conformance suite.

Run once per local host runtime (Node and Deno) so a divergence between the
two — Deno's permission model restricts the subprocess differently — cannot
hide behind a single green leg.

One inherited assertion no longer matches the release it tests.
`test_write_existing_file_fails` expects Deep Agents' pre-0.7 create-only
`write`; `BaseSandbox.write`'s own docstring now reads "creating or
overwriting it if it already exists" and its preflight only creates parent
directories. Verified against every backend upstream ships — `BaseSandbox`,
`StateBackend`, `StoreBackend`, `FilesystemBackend` — and all four overwrite.
Making wasmsh create-only to satisfy it would make wasmsh the only backend
violating the documented contract, so the assertion is marked `xfail`, which
is the only deviation mechanism this suite sanctions
(`test_no_overrides_DO_NOT_OVERRIDE` rejects a rewritten body outright).

The behaviour itself is not left untested: `TestWriteOverwriteContract` below
asserts what 0.7.x actually promises, and would fail if wasmsh ever stopped
overwriting.

The suite's six non-recursive `glob` assertions have the same problem from
Deep Agents 0.7.7 on: `BaseSandbox.glob` started returning absolute paths
(`_absolutize_glob_path`), while `langchain-tests==1.1.9` still expects bare
names such as `file1.txt`. wasmsh runs upstream's glob unchanged, so those
assertions are marked `xfail` only when the installed release is 0.7.7 or
newer; on the 0.7.4 floor they still pass. `TestGlobPathContract` asserts the
newer behaviour directly.

`test_download_error_permission_denied` needs a wasmsh runtime whose VFS
enforces permission bits. That landed with the `chmod` implementation in this
repo, so it passes against a locally built dist; an environment still on a
published runtime from before that release skips it, with the probe below
deciding which. A skip here is temporary by construction — it clears itself
the moment the runtime package is released — and the enforcement itself is
covered independently by the `wasmsh-fs` unit tests and the `chmod_*` cases
in `tests/suite/`.
"""

# ruff: noqa: S108 -- paths are inside the sandbox VFS, not the host

from __future__ import annotations

import shutil
from typing import TYPE_CHECKING

import pytest
from deepagents import __version__ as deepagents_version
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

# Deep Agents 0.7.7 changed `BaseSandbox.glob` to return absolute paths; the
# pinned conformance suite predates that and asserts bare file names.
_GLOB_RETURNS_ABSOLUTE_PATHS = tuple(
    int(part) for part in deepagents_version.split(".")[:3]
) >= (0, 7, 7)
_STALE_GLOB_ASSERTIONS = frozenset(
    {
        "test_glob",
        "test_glob_basic_pattern",
        "test_glob_with_directories",
        "test_glob_hidden_files_explicitly",
        "test_glob_with_character_class",
        "test_glob_with_question_mark",
    },
)


def enforces_permissions(backend: SandboxBackendProtocol) -> bool:
    """Whether this runtime's VFS actually honours `chmod`.

    Asked by doing it rather than by comparing version strings: the runtime
    package and the assets inside it can move independently, and the only
    thing the test cares about is the observable behaviour.
    """
    probe = "/tmp/.wasmsh_permission_probe"
    backend.execute(f"echo probe > {probe} && chmod 000 {probe}")
    try:
        return backend.download_files([probe])[0].error == "permission_denied"
    finally:
        backend.execute(f"chmod 644 {probe} && rm -f {probe}")


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

    @pytest.fixture(autouse=True)
    def _skip_without_permission_support(
        self,
        request: pytest.FixtureRequest,
        sandbox_backend: SandboxBackendProtocol,
    ) -> None:
        """Skip the permission test on a runtime that cannot enforce modes.

        Implemented as a fixture rather than by overriding the test, because
        `test_no_overrides_DO_NOT_OVERRIDE` rejects a rewritten body and
        would force a blanket `xfail` — which would keep reporting a failure
        long after the runtime gained the capability.
        """
        if request.node.name != "test_download_error_permission_denied":
            return
        if not enforces_permissions(sandbox_backend):
            pytest.skip(
                "installed wasmsh runtime predates VFS permission enforcement; "
                "rebuild with `just build-pyodide && just package-pyodide-runtime`",
            )

    @pytest.fixture(autouse=True)
    def _xfail_stale_glob_assertions(self, request: pytest.FixtureRequest) -> None:
        """Mark the relative-path glob assertions `xfail` on deepagents >= 0.7.7.

        A fixture rather than an override for the same reason as above, and
        conditional rather than blanket so the 0.7.4 floor keeps asserting
        the assertions it still satisfies. `strict=True` turns an upstream
        reversal into a loud failure instead of a silent xpass.
        """
        if request.node.name not in _STALE_GLOB_ASSERTIONS:
            return
        if _GLOB_RETURNS_ABSOLUTE_PATHS:
            request.node.add_marker(
                pytest.mark.xfail(
                    reason=(
                        "langchain-tests 1.1.9 expects bare glob names; "
                        "deepagents >= 0.7.7 returns absolute paths by design. "
                        "Covered instead by TestGlobPathContract."
                    ),
                    strict=True,
                ),
            )

    @pytest.mark.xfail(
        reason=(
            "langchain-tests 1.1.9 asserts pre-0.7 create-only write; every "
            "deepagents 0.7.x backend overwrites by design. Covered instead "
            "by TestWriteOverwriteContract."
        ),
        strict=True,
    )
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


@pytest.mark.skipif(not _assets_available, reason=_ASSETS_REASON)
@pytest.mark.skipif(shutil.which("node") is None, reason="node is not installed")
@pytest.mark.skipif(
    not _GLOB_RETURNS_ABSOLUTE_PATHS,
    reason="deepagents < 0.7.7 returns glob matches relative to the search root",
)
class TestGlobPathContract:
    """The 0.7.7+ `glob` contract the stale suite assertions contradict."""

    @pytest.fixture(scope="class")
    def sandbox(self) -> Iterator[WasmshSandbox]:
        backend = WasmshSandbox(runtime="node")
        try:
            yield backend
        finally:
            backend.close()

    def test_matches_are_absolute_and_rooted_at_the_search_path(
        self,
        sandbox: WasmshSandbox,
    ) -> None:
        base = "/tmp/glob_contract"
        sandbox.execute(f"mkdir -p {base}/nested")
        sandbox.write(f"{base}/one.txt", "1")
        sandbox.write(f"{base}/nested/two.txt", "2")
        sandbox.write(f"{base}/skip.py", "3")

        result = sandbox.glob("*.txt", path=base)

        assert result.error is None
        assert result.matches is not None
        paths = sorted(info["path"] for info in result.matches)
        assert paths == [f"{base}/nested/two.txt", f"{base}/one.txt"]


@pytest.mark.skipif(not _assets_available, reason=_ASSETS_REASON)
@pytest.mark.skipif(shutil.which("node") is None, reason="node is not installed")
class TestWriteOverwriteContract:
    """The 0.7.x `write` contract the stale suite assertion contradicts.

    Kept here rather than as a suite override, which
    `test_no_overrides_DO_NOT_OVERRIDE` forbids. If wasmsh ever regressed to
    create-only `write`, this fails while the xfail above would flip to
    XPASS — either way the change is reported.
    """

    def test_write_replaces_existing_content(self, sandbox: WasmshSandbox) -> None:
        path = "/tmp/overwrite/existing.txt"
        assert sandbox.write(path, "First content").error is None

        result = sandbox.write(path, "Second content")

        assert result.error is None
        assert result.path == path
        assert sandbox.execute(f"cat {path}").output.strip() == "Second content"

    async def test_awrite_replaces_existing_content(
        self,
        sandbox: WasmshSandbox,
    ) -> None:
        path = "/tmp/overwrite/existing_async.txt"
        assert (await sandbox.awrite(path, "First")).error is None
        assert (await sandbox.awrite(path, "Second")).error is None
        assert sandbox.execute(f"cat {path}").output.strip() == "Second"
