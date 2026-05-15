"""Per-thread Python REPL backed by a long-lived wasmsh sandbox.

One :class:`_ThreadREPL` instance owns one :class:`WasmshSandbox` (or
:class:`WasmshRemoteSandbox`) and serialises every interpreter call against
it. The launcher script (see :mod:`_launcher`) is uploaded once, then each
call writes the user's source to a fixed sandbox path and runs
``python3 <launcher> <code>``. The launcher prints a single marker line
containing a JSON envelope which the host parses into an :class:`Outcome`.

The :class:`_Registry` indexes REPLs by ``thread_id`` so the middleware's
``before_agent`` / ``after_agent`` / ``wrap_model_call`` hooks all resolve to
the same session for a given LangGraph thread.
"""

from __future__ import annotations

import asyncio
import json
import logging
import shlex
import threading
from collections.abc import Callable
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any, Protocol

from langchain_wasmsh._launcher import (
    CODE_PATH,
    GLOBALS_PATH,
    LAUNCHER_PATH,
    LAUNCHER_SCRIPT,
    RESULT_MARKER,
)
from langchain_wasmsh._skills import LoadedSkill, load_skill, scan_skill_references

if TYPE_CHECKING:
    from deepagents.backends.protocol import BackendProtocol
    from deepagents.middleware.skills import SkillMetadata


logger = logging.getLogger(__name__)


class SandboxLike(Protocol):
    """The subset of the wasmsh sandbox surface the REPL needs.

    Promoted from a private alias so callers (interpreter middleware, tests)
    can write `sandbox_factory: Callable[[], SandboxLike]` and accept either
    ``WasmshSandbox``, ``WasmshRemoteSandbox``, or any structural stand-in.
    """

    def execute(self, command: str, *, timeout: int | None = ...) -> Any: ...
    def upload_files(self, files: list[tuple[str, bytes]]) -> Any: ...
    def download_files(self, paths: list[str]) -> Any: ...
    def close(self) -> None: ...


# Back-compat alias for the leading-underscore name used elsewhere.
_SandboxLike = SandboxLike

SandboxFactory = Callable[[], SandboxLike]


@dataclass
class Outcome:
    """Structured result of one interpreter call.

    Field invariants (held by the construction sites; not validated):

    - When ``ok`` is ``True``: ``error`` / ``message`` / ``traceback`` are
      ``None``. ``value`` is the trailing-expression result (JSON-safe shape
      from the launcher's ``_safe_value``: primitives pass through, complex
      types become repr strings).
    - When ``ok`` is ``False``: ``error`` is the exception class name and
      ``message`` is its ``str()``. ``traceback`` may be present for runtime
      failures (absent for launcher / host errors). ``value`` is ``None``.

    ``stdout`` and ``stderr`` are always strings (possibly empty) in both cases.
    """

    ok: bool
    stdout: str = ""
    stderr: str = ""
    value: Any = None
    error: str | None = None
    message: str | None = None
    traceback: str | None = None

    @property
    def is_error(self) -> bool:
        """``True`` iff the call did not complete successfully."""
        return not self.ok

    @classmethod
    def from_envelope(cls, env: dict[str, Any]) -> Outcome:
        """Build an ``Outcome`` from the launcher's JSON envelope."""
        return cls(
            ok=bool(env.get("ok", False)),
            stdout=str(env.get("stdout", "") or ""),
            stderr=str(env.get("stderr", "") or ""),
            value=env.get("value"),
            error=env.get("error"),
            message=env.get("message"),
            traceback=env.get("traceback"),
        )

    @classmethod
    def host_error(cls, name: str, message: str) -> Outcome:
        """Build an ``Outcome`` for a failure that didn't reach the launcher."""
        return cls(ok=False, error=name, message=message)


def format_outcome(outcome: Outcome, *, max_result_chars: int) -> str:
    """Render an :class:`Outcome` for a LangChain ``ToolMessage`` body."""
    parts: list[str] = []
    if outcome.stdout:
        parts.append(_block("stdout", outcome.stdout, max_result_chars))
    if outcome.stderr:
        parts.append(_block("stderr", outcome.stderr, max_result_chars))
    if outcome.ok:
        if outcome.value is not None:
            # value may be a native python type now (str, int, list, dict);
            # render via json for stable serialisation, falling back to repr.
            try:
                rendered = json.dumps(outcome.value, ensure_ascii=False)
            except (TypeError, ValueError):
                rendered = repr(outcome.value)
            parts.append(_block("value", rendered, max_result_chars))
        if not parts:
            parts.append("<no output>")
    else:
        body = outcome.message or ""
        if outcome.traceback:
            body = body + "\n\n" + outcome.traceback if body else outcome.traceback
        label = f"error {outcome.error or 'Error'}"
        parts.append(_block(label, body, max_result_chars))
    return "\n\n".join(parts)


def _block(label: str, body: str, limit: int) -> str:
    truncated = body
    if len(truncated) > limit:
        truncated = truncated[: max(0, limit - 1)] + "…"
    return f"<{label}>\n{truncated}\n</{label}>"


@dataclass
class _SkillCache:
    """Tracks which skill packages have been staged into the sandbox VFS."""

    installed: dict[str, LoadedSkill] = field(default_factory=dict)

    def needs_install(self, requested: frozenset[str]) -> set[str]:
        return {name for name in requested if name not in self.installed}


class _ThreadREPL:
    """One REPL session for one LangGraph thread."""

    def __init__(self, factory: Any) -> None:
        self._factory = factory
        self._sandbox: _SandboxLike | None = None
        self._launcher_uploaded = False
        self._snapshot_pending: bytes | None = None
        self._skill_cache = _SkillCache()
        self._lock = threading.Lock()

    # ---- lifecycle -------------------------------------------------------

    def _ensure_sandbox(self) -> _SandboxLike:
        sandbox = self._sandbox
        if sandbox is None:
            sandbox = self._factory()
            self._sandbox = sandbox
            self._launcher_uploaded = False
        if not self._launcher_uploaded:
            sandbox.upload_files([(LAUNCHER_PATH, LAUNCHER_SCRIPT.encode("utf-8"))])
            self._launcher_uploaded = True
            if self._snapshot_pending is not None:
                sandbox.upload_files([(GLOBALS_PATH, self._snapshot_pending)])
                self._snapshot_pending = None
        return sandbox

    def close(self) -> None:
        """Close the underlying sandbox if it was started."""
        sandbox, self._sandbox = self._sandbox, None
        self._launcher_uploaded = False
        if sandbox is None:
            return
        try:
            sandbox.close()
        except Exception:  # noqa: BLE001 -- surface every failure path
            logger.warning("WasmshSandbox.close failed", exc_info=True)

    # ---- skill staging ---------------------------------------------------

    def _install_pending_skills(
        self,
        source: str,
        skills: dict[str, SkillMetadata] | None,
        backend: BackendProtocol | None,
    ) -> None:
        if not skills or backend is None:
            return
        referenced = scan_skill_references(source)
        if not referenced:
            return
        sandbox = self._ensure_sandbox()
        for package_name in self._skill_cache.needs_install(referenced):
            kebab = package_name.replace("_", "-")
            meta = skills.get(kebab) or skills.get(package_name)
            if meta is None:
                logger.debug("skill %r referenced but not in metadata", package_name)
                continue
            try:
                loaded = load_skill(meta, backend)
            except Exception as exc:  # noqa: BLE001 -- isolate one broken skill
                logger.warning("failed to load skill %r: %s", kebab, exc)
                continue
            sandbox.upload_files(list(loaded.files.items()))
            self._skill_cache.installed[loaded.package_name] = loaded

    # ---- eval ------------------------------------------------------------

    def eval_sync(
        self,
        code: str,
        *,
        skills: dict[str, SkillMetadata] | None = None,
        skills_backend: BackendProtocol | None = None,
        ptc_tools: dict[str, Any] | None = None,
    ) -> Outcome:
        """Run one interpreter call; safe to call from multiple threads.

        When ``ptc_tools`` is provided, the call is routed through
        ``sandbox.run_ptc`` so user code can ``await tools.<name>(...)`` —
        each ``host_call`` event is dispatched against ``ptc_tools[name]``.
        Otherwise the standard file-launcher shell path is used.
        """
        with self._lock:
            try:
                self._install_pending_skills(code, skills, skills_backend)
                sandbox = self._ensure_sandbox()
                if ptc_tools:
                    return self._eval_with_ptc(sandbox, code, ptc_tools)
                # Upload the user code to the fixed VFS path the launcher
                # reads from (wasmsh's python3 builtin does not pass argv).
                sandbox.upload_files([(CODE_PATH, code.encode("utf-8"))])
                command = f"python3 {shlex.quote(LAUNCHER_PATH)}"
                response = sandbox.execute(command)
            except Exception as exc:
                logger.exception("wasmsh REPL execute failed")
                return Outcome.host_error(type(exc).__name__, str(exc))
        return _parse_response(response)

    def _eval_with_ptc(
        self,
        sandbox: _SandboxLike,
        code: str,
        ptc_tools: dict[str, Any],
    ) -> Outcome:
        run_ptc = getattr(sandbox, "run_ptc", None)
        if run_ptc is None:
            return Outcome.host_error(
                "PTCUnsupported",
                "sandbox does not implement run_ptc",
            )
        dispatcher = _make_ptc_dispatcher(ptc_tools)
        try:
            envelope = run_ptc(
                code,
                tools=sorted(ptc_tools),
                on_host_call=dispatcher,
            )
        except (RuntimeError, OSError) as exc:
            # Narrow: transport / protocol / capability failures from
            # sandbox.run_ptc. Programmer bugs in the dispatcher must
            # surface as themselves, not be quietly wrapped here.
            logger.exception("wasmsh PTC run failed")
            return Outcome.host_error(type(exc).__name__, str(exc))
        return Outcome.from_envelope(envelope)

    async def eval_async(
        self,
        code: str,
        *,
        skills: dict[str, SkillMetadata] | None = None,
        skills_backend: BackendProtocol | None = None,
        ptc_tools: dict[str, Any] | None = None,
    ) -> Outcome:
        """Async wrapper around :meth:`eval_sync` (runs in a worker thread)."""
        return await asyncio.to_thread(
            self.eval_sync,
            code,
            skills=skills,
            skills_backend=skills_backend,
            ptc_tools=ptc_tools,
        )

    # ---- snapshot --------------------------------------------------------

    def create_snapshot(self) -> bytes | None:
        """Return the persisted globals pickle, or ``None`` if not present."""
        if self._sandbox is None:
            if self._snapshot_pending is not None:
                return self._snapshot_pending
            return None
        try:
            responses = self._sandbox.download_files([GLOBALS_PATH])
        except Exception:  # noqa: BLE001 -- surface every failure path
            logger.warning("snapshot read failed", exc_info=True)
            return None
        if not responses:
            return None
        resp = responses[0]
        if getattr(resp, "error", None) or getattr(resp, "content", None) is None:
            return None
        return resp.content

    def restore_snapshot(self, payload: bytes) -> None:
        """Stage ``payload`` to be uploaded on the next eval (or upload now)."""
        if self._sandbox is None:
            self._snapshot_pending = payload
            return
        self._sandbox.upload_files([(GLOBALS_PATH, payload)])


def _coerce_tool_output(value: Any) -> Any:
    """Convert a LangChain tool's return value into a JSON-serialisable shape.

    Mirrors the QuickJS adapter's coercion chain so behaviour stays familiar
    across interpreters: BaseModel → ``model_dump``; objects with ``to_dict``
    → that method; primitives pass through; anything else → ``str(value)``.
    """
    if value is None or isinstance(value, (bool, int, float, str)):
        return value
    if isinstance(value, (list, tuple)):
        return [_coerce_tool_output(v) for v in value]
    if isinstance(value, dict):
        return {str(k): _coerce_tool_output(v) for k, v in value.items()}
    model_dump = getattr(value, "model_dump", None)
    if callable(model_dump):
        try:
            return model_dump()
        except Exception:  # noqa: BLE001, S110 -- best-effort coercion fallback
            pass
    to_dict = getattr(value, "to_dict", None)
    if callable(to_dict):
        try:
            return to_dict()
        except Exception:  # noqa: BLE001, S110 -- best-effort coercion fallback
            pass
    return str(value)


def _make_ptc_dispatcher(
    tools: dict[str, Any],
) -> Any:
    """Build the ``on_host_call`` callable WasmshSandbox.run_ptc expects."""

    def dispatch(message: dict[str, Any]) -> dict[str, Any]:
        name = message.get("tool")
        if not isinstance(name, str) or name not in tools:
            return {
                "ok": False,
                "error": "UnknownToolError",
                "message": f"tool {name!r} is not on the PTC allowlist",
            }
        tool = tools[name]
        args = message.get("args") or {}
        if not isinstance(args, dict):
            return {
                "ok": False,
                "error": "TypeError",
                "message": "host_call args must be an object",
            }
        invoke = getattr(tool, "invoke", None)
        if not callable(invoke):
            return {
                "ok": False,
                "error": "ToolMisconfigured",
                "message": f"{name!r} is not a callable tool",
            }
        try:
            raw = invoke(args)
        except Exception as exc:  # noqa: BLE001 -- isolate one tool failure
            return {
                "ok": False,
                "error": type(exc).__name__,
                "message": str(exc),
            }
        return {"ok": True, "value": _coerce_tool_output(raw)}

    return dispatch


def _parse_response(response: Any) -> Outcome:
    """Extract the launcher's marker JSON from a sandbox ``ExecuteResponse``."""
    output = getattr(response, "output", "") or ""
    exit_code = getattr(response, "exit_code", None)
    marker_index = output.rfind(RESULT_MARKER)
    if marker_index < 0:
        msg = "missing launcher marker"
        if output.strip():
            msg += f": {output.strip()[:200]}"
        if exit_code is not None and exit_code != 0:
            msg += f" (exit_code={exit_code})"
        return Outcome.host_error("LauncherError", msg)
    json_start = marker_index + len(RESULT_MARKER)
    newline = output.find("\n", json_start)
    payload = output[json_start:] if newline < 0 else output[json_start:newline]
    try:
        envelope = json.loads(payload)
    except json.JSONDecodeError as exc:
        return Outcome.host_error("LauncherError", f"invalid JSON envelope: {exc}")
    if not isinstance(envelope, dict):
        return Outcome.host_error("LauncherError", "envelope is not an object")
    return Outcome.from_envelope(envelope)


class _Registry:
    """Thread-id → :class:`_ThreadREPL` index with safe eviction."""

    def __init__(self, factory: Any) -> None:
        self._factory = factory
        self._sessions: dict[str, _ThreadREPL] = {}
        self._lock = threading.Lock()

    def get(self, thread_id: str) -> _ThreadREPL:
        with self._lock:
            repl = self._sessions.get(thread_id)
            if repl is None:
                repl = _ThreadREPL(self._factory)
                self._sessions[thread_id] = repl
            return repl

    def get_if_exists(self, thread_id: str) -> _ThreadREPL | None:
        with self._lock:
            return self._sessions.get(thread_id)

    def evict(self, thread_id: str) -> None:
        with self._lock:
            repl = self._sessions.pop(thread_id, None)
        if repl is not None:
            repl.close()

    def close(self) -> None:
        with self._lock:
            sessions = list(self._sessions.values())
            self._sessions.clear()
        for repl in sessions:
            repl.close()
