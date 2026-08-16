"""Optional sandbox providers for the Deep Agents Code CLI.

Deep Agents Code discovers third-party sandbox backends through the
``deepagents_code.sandbox_providers`` entry-point group. This module supplies
two of them.

It is deliberately **not** imported by :mod:`langchain_wasmsh`. The provider
protocol lives in ``deepagents-code``, a separate distribution with its own
release cadence and its own Deep Agents pin, and normal adapter use must not
depend on it. Importing this module without that package installed raises a
clear :class:`ImportError` rather than failing somewhere deeper.

Two providers, not one
~~~~~~~~~~~~~~~~~~~~~~

``wasmsh``
    A local in-process sandbox. It has no server-side identity to reconnect
    to, so ``supports_sandbox_id`` is ``False``. Instances created by one
    provider object are tracked by id only so that ``delete(sandbox_id=…)``
    can close the right one; the map is per-provider-object and holds no
    global state.

``wasmsh-remote``
    A dispatcher-backed session, which *does* have a durable id, so
    ``supports_sandbox_id`` is ``True``. Passing an existing ``sandbox_id``
    reconnects to that dispatcher session. Cleanup only closes a session this
    provider created — reconnecting to someone else's session and then
    deleting it on exit would be a surprising way to lose work.

A single provider covering both would have to lie about
``supports_sandbox_id``, which is exactly the flag the CLI uses to decide
whether reconnecting is possible.

Setup scripts
~~~~~~~~~~~~~

Deep Agents Code runs a setup script with ``bash -c <script>``. wasmsh
resolves ``bash`` (and ``sh``) to its own shell rather than shipping a stub
that swallows what it cannot run: unsupported syntax produces a parse error,
not a silent success. Scripts that stay inside wasmsh's supported Bash subset
work; anything relying on a feature wasmsh does not implement fails loudly.
See `SUPPORTED.md` for the current surface.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Any

try:
    from deepagents_code.integrations.sandbox_provider import (
        SandboxNotFoundError,
        SandboxProvider,
        SandboxProviderMetadata,
    )
except ImportError as exc:  # pragma: no cover - exercised by the import test
    msg = (
        "langchain_wasmsh.dcode_provider requires the optional `deepagents-code` "
        "package. Install it with `pip install deepagents-code`."
    )
    raise ImportError(msg) from exc

from langchain_wasmsh.remote import WasmshRemoteSandbox
from langchain_wasmsh.sandbox import WasmshSandbox

if TYPE_CHECKING:
    from deepagents.backends.protocol import SandboxBackendProtocol

WORKING_DIR = "/workspace"
"""Directory Deep Agents Code should treat as the project root."""

_INSTALL_HINT_PACKAGE = "langchain-wasmsh"


class WasmshSandboxProvider(SandboxProvider):
    """Local in-process wasmsh sandboxes for Deep Agents Code."""

    @property
    def metadata(self) -> SandboxProviderMetadata:
        """Describe this provider without constructing a sandbox."""
        from deepagents_code.integrations.sandbox_provider import (  # noqa: PLC0415
            SandboxInstallHint,
        )

        return SandboxProviderMetadata(
            name="wasmsh",
            working_dir=WORKING_DIR,
            install=SandboxInstallHint(kind="package", name=_INSTALL_HINT_PACKAGE),
            # A local sandbox is a subprocess, not a durable remote resource:
            # there is nothing to reconnect to after this process exits.
            supports_sandbox_id=False,
            supports_snapshot_name=False,
            backend_module="langchain_wasmsh",
        )

    def __init__(self) -> None:
        """Start with no live instances tracked."""
        self._live: dict[str, WasmshSandbox] = {}

    def get_or_create(
        self,
        *,
        sandbox_id: str | None = None,
        **kwargs: Any,
    ) -> SandboxBackendProtocol:
        """Return a live sandbox, creating one when no id is given.

        Args:
            sandbox_id: Id of a sandbox this provider object created. Any
                other value is a miss: a local sandbox cannot be reattached
                across processes, and silently creating a fresh one instead
                would hand the caller an empty filesystem it believed was
                populated.
            **kwargs: Forwarded to `WasmshSandbox` (`allowed_hosts`,
                `step_budget`, `initial_files`, `runtime`, …).

        Returns:
            A live `WasmshSandbox`.

        Raises:
            SandboxNotFoundError: If `sandbox_id` is not one this provider
                object is currently tracking.
        """
        if sandbox_id is not None:
            existing = self._live.get(sandbox_id)
            if existing is None:
                msg = (
                    f"No live wasmsh sandbox with id {sandbox_id!r}. Local "
                    "sandboxes cannot be reattached across processes; use the "
                    "`wasmsh-remote` provider for sessions that outlive the "
                    "client."
                )
                raise SandboxNotFoundError(msg)
            return existing

        kwargs.setdefault("working_directory", WORKING_DIR)
        sandbox = WasmshSandbox(**kwargs)
        self._live[sandbox.id] = sandbox
        return sandbox

    def delete(self, *, sandbox_id: str, **kwargs: Any) -> None:
        """Close a sandbox this provider created.

        Deleting an unknown id is a no-op rather than an error: cleanup runs
        on paths where the sandbox may already be gone, and raising there
        would turn a tidy shutdown into a failure.
        """
        del kwargs
        sandbox = self._live.pop(sandbox_id, None)
        if sandbox is not None:
            sandbox.close()


class WasmshRemoteSandboxProvider(SandboxProvider):
    """Dispatcher-backed wasmsh sessions for Deep Agents Code."""

    @property
    def metadata(self) -> SandboxProviderMetadata:
        """Describe this provider without contacting a dispatcher."""
        from deepagents_code.integrations.sandbox_provider import (  # noqa: PLC0415
            SandboxInstallHint,
        )

        return SandboxProviderMetadata(
            name="wasmsh-remote",
            working_dir=WORKING_DIR,
            install=SandboxInstallHint(kind="package", name=_INSTALL_HINT_PACKAGE),
            # A dispatcher session has a durable id and outlives the client.
            supports_sandbox_id=True,
            supports_snapshot_name=False,
            backend_module="langchain_wasmsh",
        )

    def __init__(self) -> None:
        """Track which sessions this provider object created."""
        self._live: dict[str, WasmshRemoteSandbox] = {}
        self._created: set[str] = set()

    def get_or_create(
        self,
        *,
        sandbox_id: str | None = None,
        **kwargs: Any,
    ) -> SandboxBackendProtocol:
        """Attach to a dispatcher session, creating one when no id is given.

        Args:
            sandbox_id: Existing dispatcher session id to reuse. When given,
                the session is *not* marked as created by this provider, so
                cleanup will leave it running.
            **kwargs: Forwarded to `WasmshRemoteSandbox`. `dispatcher_url` is
                required and may also be supplied as `WASMSH_DISPATCHER_URL`
                by the caller's configuration; `headers`, `allowed_hosts`,
                `step_budget`, and `timeout` are all accepted.

        Returns:
            A live `WasmshRemoteSandbox`.

        Raises:
            ValueError: If no dispatcher URL was supplied.
        """
        if sandbox_id is not None and sandbox_id in self._live:
            return self._live[sandbox_id]

        dispatcher_url = kwargs.pop("dispatcher_url", None)
        if not dispatcher_url:
            msg = (
                "The `wasmsh-remote` provider needs a dispatcher URL. Pass "
                "`dispatcher_url` in the provider params, e.g. "
                '`params = { dispatcher_url = "http://wasmsh-dispatcher:8080" }`.'
            )
            raise ValueError(msg)

        kwargs.setdefault("working_directory", WORKING_DIR)
        sandbox = WasmshRemoteSandbox(
            dispatcher_url,
            session_id=sandbox_id,
            **kwargs,
        )
        self._live[sandbox.id] = sandbox
        if sandbox_id is None:
            self._created.add(sandbox.id)
        return sandbox

    def delete(self, *, sandbox_id: str, **kwargs: Any) -> None:
        """Close a session, but only end one this provider created.

        Reconnecting to an existing session and then destroying it on exit
        would be a surprising way to lose someone else's work, so a session
        supplied by the caller is released locally and left running on the
        dispatcher.
        """
        del kwargs
        sandbox = self._live.pop(sandbox_id, None)
        if sandbox is None:
            return
        if sandbox_id in self._created:
            self._created.discard(sandbox_id)
            sandbox.close()


__all__ = [
    "WORKING_DIR",
    "WasmshRemoteSandboxProvider",
    "WasmshSandboxProvider",
]
