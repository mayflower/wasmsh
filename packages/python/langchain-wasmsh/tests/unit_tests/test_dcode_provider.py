"""Deep Agents Code sandbox providers.

Skipped unless the optional `deepagents-code` package is installed. It is a
separate distribution with its own release cadence and its own Deep Agents
pin, so it is not a base dependency and its absence must not fail the suite.

The remote provider is tested against a mocked dispatcher; the local one
boots real sandboxes, so those tests are gated on the Pyodide assets.
"""

from __future__ import annotations

import importlib.metadata as md
import json
import shlex
import shutil

import httpx
import pytest
import respx

pytest.importorskip(
    "deepagents_code",
    reason="optional `deepagents-code` package is not installed",
)

# Imported after the skip guard: the module only exists when the optional
# package is installed, so a top-of-file import would fail collection.
from deepagents_code.integrations.sandbox_provider import (
    SandboxNotFoundError,
)
from deepagents_code.integrations.sandbox_registry import (
    SandboxRegistry,
)

from langchain_wasmsh.dcode_provider import (
    WORKING_DIR,
    WasmshRemoteSandboxProvider,
    WasmshSandboxProvider,
)

try:
    from wasmsh_pyodide_runtime import get_dist_dir

    _assets = get_dist_dir().joinpath("pyodide.asm.wasm").exists()
except (ImportError, FileNotFoundError):  # pragma: no cover - packaging guard
    _assets = False

requires_local_runtime = pytest.mark.skipif(
    not _assets or (shutil.which("deno") is None and shutil.which("node") is None),
    reason="needs Pyodide assets and a Deno or Node runtime",
)

BASE_URL = "http://dispatcher.test"


def _session_route(router: respx.MockRouter, session_id: str) -> respx.Route:
    return router.post(f"{BASE_URL}/sessions").mock(
        return_value=httpx.Response(
            201,
            json={"ok": True, "session": {"sessionId": session_id}},
        ),
    )


class TestMetadata:
    def test_local_provider_does_not_claim_reattachable_ids(self) -> None:
        # A local sandbox is a subprocess. Claiming `supports_sandbox_id`
        # would tell the CLI it can reconnect to something that no longer
        # exists once the process is gone.
        metadata = WasmshSandboxProvider().metadata
        assert metadata.name == "wasmsh"
        assert metadata.working_dir == WORKING_DIR
        assert metadata.supports_sandbox_id is False
        assert metadata.backend_module == "langchain_wasmsh"

    def test_remote_provider_claims_reattachable_ids(self) -> None:
        metadata = WasmshRemoteSandboxProvider().metadata
        assert metadata.name == "wasmsh-remote"
        assert metadata.working_dir == WORKING_DIR
        assert metadata.supports_sandbox_id is True

    def test_install_hint_names_this_package(self) -> None:
        hint = WasmshSandboxProvider().metadata.install
        assert hint is not None
        assert hint.kind == "package"
        assert hint.name == "langchain-wasmsh"


class TestEntryPointRegistration:
    def test_both_providers_are_discoverable(self) -> None:
        names = {
            entry.name
            for entry in md.entry_points(group="deepagents_code.sandbox_providers")
        }
        assert {"wasmsh", "wasmsh-remote"} <= names

    def test_the_registry_can_construct_them(self) -> None:
        registry = SandboxRegistry()
        assert isinstance(registry.create_provider("wasmsh"), WasmshSandboxProvider)
        assert isinstance(
            registry.create_provider("wasmsh-remote"),
            WasmshRemoteSandboxProvider,
        )

    def test_the_registry_reads_our_metadata(self) -> None:
        metadata = SandboxRegistry().provider_metadata("wasmsh-remote")
        assert metadata.working_dir == WORKING_DIR
        assert metadata.supports_sandbox_id is True


@requires_local_runtime
class TestLocalProvider:
    def test_creates_a_usable_sandbox_rooted_at_the_working_dir(self) -> None:
        provider = WasmshSandboxProvider()
        sandbox = provider.get_or_create()
        try:
            assert sandbox.execute("pwd").output.strip() == WORKING_DIR
        finally:
            provider.delete(sandbox_id=sandbox.id)

    def test_an_id_from_this_provider_returns_the_same_instance(self) -> None:
        provider = WasmshSandboxProvider()
        sandbox = provider.get_or_create()
        try:
            assert provider.get_or_create(sandbox_id=sandbox.id) is sandbox
        finally:
            provider.delete(sandbox_id=sandbox.id)

    def test_an_unknown_id_is_reported_not_silently_replaced(self) -> None:
        # Creating a fresh sandbox here would hand the caller an empty
        # filesystem it believed was populated.
        with pytest.raises(SandboxNotFoundError, match="cannot be reattached"):
            WasmshSandboxProvider().get_or_create(sandbox_id="wasmsh-python-ghost")

    def test_delete_closes_the_instance(self) -> None:
        provider = WasmshSandboxProvider()
        sandbox = provider.get_or_create()
        provider.delete(sandbox_id=sandbox.id)
        assert sandbox._process.poll() is not None

    def test_deleting_an_unknown_id_is_a_no_op(self) -> None:
        # Cleanup runs on paths where the sandbox may already be gone.
        WasmshSandboxProvider().delete(sandbox_id="nope")

    def test_construction_kwargs_reach_the_sandbox(self) -> None:
        provider = WasmshSandboxProvider()
        sandbox = provider.get_or_create(
            initial_files={"/workspace/seeded.txt": b"hello"},
        )
        try:
            assert "hello" in sandbox.execute("cat /workspace/seeded.txt").output
        finally:
            provider.delete(sandbox_id=sandbox.id)


class TestRemoteProvider:
    @respx.mock
    def test_creates_a_dispatcher_session(self) -> None:
        route = _session_route(respx.mock, "sess-1")
        provider = WasmshRemoteSandboxProvider()
        sandbox = provider.get_or_create(dispatcher_url=BASE_URL)
        assert route.called
        assert sandbox.id == "sess-1"

    @respx.mock
    def test_a_missing_dispatcher_url_is_an_actionable_error(self) -> None:
        with pytest.raises(ValueError, match="dispatcher URL"):
            WasmshRemoteSandboxProvider().get_or_create()

    @respx.mock
    def test_delete_closes_a_session_this_provider_created(self) -> None:
        _session_route(respx.mock, "sess-2")
        close = respx.mock.post(f"{BASE_URL}/sessions/sess-2/close").mock(
            return_value=httpx.Response(200, json={"ok": True}),
        )
        respx.mock.delete(f"{BASE_URL}/sessions/sess-2").mock(
            return_value=httpx.Response(200, json={"ok": True}),
        )

        provider = WasmshRemoteSandboxProvider()
        sandbox = provider.get_or_create(dispatcher_url=BASE_URL)
        provider.delete(sandbox_id=sandbox.id)

        assert close.called

    @respx.mock
    def test_delete_leaves_a_caller_supplied_session_running(self) -> None:
        # Reconnecting to someone else's session and then destroying it on
        # exit would be a surprising way to lose their work.
        _session_route(respx.mock, "sess-3")
        close = respx.mock.post(f"{BASE_URL}/sessions/sess-3/close").mock(
            return_value=httpx.Response(200, json={"ok": True}),
        )

        provider = WasmshRemoteSandboxProvider()
        provider.get_or_create(dispatcher_url=BASE_URL, sandbox_id="sess-3")
        provider.delete(sandbox_id="sess-3")

        assert not close.called

    @respx.mock
    def test_reattaching_to_a_live_session_returns_the_same_instance(self) -> None:
        _session_route(respx.mock, "sess-4")
        provider = WasmshRemoteSandboxProvider()
        first = provider.get_or_create(dispatcher_url=BASE_URL)
        assert provider.get_or_create(sandbox_id=first.id) is first

    @respx.mock
    def test_provider_kwargs_reach_the_session_payload(self) -> None:
        route = _session_route(respx.mock, "sess-5")
        WasmshRemoteSandboxProvider().get_or_create(
            dispatcher_url=BASE_URL,
            allowed_hosts=["pypi.org"],
            step_budget=1234,
        )
        payload = json.loads(route.calls[0].request.content)
        assert payload["allowed_hosts"] == ["pypi.org"]
        assert payload["step_budget"] == 1234


@requires_local_runtime
class TestSetupShell:
    def test_bash_dash_c_runs_the_wasmsh_shell(self) -> None:
        # Deep Agents Code runs setup scripts as `bash -c <script>`. wasmsh
        # resolves `bash` to its own shell rather than shipping a stub that
        # swallows what it cannot run.
        provider = WasmshSandboxProvider()
        sandbox = provider.get_or_create()
        try:
            script = (
                "set -e\n"
                "mkdir -p /workspace/setup\n"
                'echo "from setup" > /workspace/setup/out.txt\n'
                "cat /workspace/setup/out.txt\n"
            )
            result = sandbox.execute(f"bash -c {shlex.quote(script)}")
            assert result.exit_code == 0
            assert "from setup" in result.output
        finally:
            provider.delete(sandbox_id=sandbox.id)

    def test_unsupported_syntax_fails_loudly_rather_than_silently(self) -> None:
        # The property that makes shipping this provider honest: a script
        # wasmsh cannot run reports a failure instead of a fake success.
        provider = WasmshSandboxProvider()
        sandbox = provider.get_or_create()
        try:
            result = sandbox.execute(f"bash -c {shlex.quote('if then fi ;;')}")
            assert result.exit_code != 0
        finally:
            provider.delete(sandbox_id=sandbox.id)
