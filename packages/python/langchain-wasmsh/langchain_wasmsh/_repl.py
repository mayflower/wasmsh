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
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any, Protocol

from langchain_wasmsh._launcher import (
    CODE_PATH,
    GLOBALS_PATH,
    LAUNCHER_PATH,
    LAUNCHER_SCRIPT,
    RESULT_MARKER,
)
from langchain_wasmsh._ptc import (
    PTCCallBudgetExceededError,
    coerce_tool_output_for_ptc,
    inject_tool_args_for_ptc,
    normalize_tool_input,
    synth_tool_call_id,
    tool_uses_injected_tool_call_id,
)
from langchain_wasmsh._skills import (
    SkillBundleCache,
    load_skill,
    resolve_importable_skills,
    scan_skill_references,
)

if TYPE_CHECKING:
    from deepagents.backends.protocol import BackendProtocol
    from deepagents.middleware.skills import SkillMetadata
    from langchain_core.tools import BaseTool


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


class _ThreadREPL:
    """One REPL session for one LangGraph thread."""

    def __init__(self, factory: Any) -> None:
        self._factory = factory
        self._sandbox: _SandboxLike | None = None
        self._launcher_uploaded = False
        self._snapshot_pending: bytes | None = None
        self._skill_cache = SkillBundleCache()
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
        """Stage every skill this program imports, if not already current.

        Only skills the source actually references are fetched, so metadata
        discovery stays outside the sandbox and an unused skill in the
        library costs nothing. A skill that fails to load is logged and
        skipped: the import will fail inside the interpreter with an ordinary
        `ModuleNotFoundError`, which the model can react to, rather than
        taking down the whole evaluation.
        """
        if not skills or backend is None:
            return
        referenced = scan_skill_references(source)
        if not referenced:
            return
        importable = resolve_importable_skills(skills)
        sandbox = self._ensure_sandbox()
        for package_name in sorted(referenced):
            meta = importable.get(package_name)
            if meta is None:
                logger.debug(
                    "`skills.%s` is referenced but no loaded skill maps to it",
                    package_name,
                )
                continue
            try:
                loaded = load_skill(meta, backend)
            except Exception as exc:  # noqa: BLE001 -- isolate one broken skill
                logger.warning("failed to load skill %r: %s", meta["name"], exc)
                continue
            # Fingerprint comparison, not a name check: a skill whose bytes
            # changed re-stages, and one that merely got referenced again in
            # the same session does not pay for another upload.
            if self._skill_cache.is_current(loaded):
                continue
            sandbox.upload_files(list(loaded.files.items()))
            self._skill_cache.record(loaded)

    # ---- eval ------------------------------------------------------------

    def eval_sync(  # noqa: PLR0913 -- one call carries the whole PTC context
        self,
        code: str,
        *,
        skills: dict[str, SkillMetadata] | None = None,
        skills_backend: BackendProtocol | None = None,
        ptc_tools: dict[str, BaseTool] | None = None,
        outer_runtime: Any = None,
        outer_loop: asyncio.AbstractEventLoop | None = None,
        max_ptc_calls: int | None = None,
    ) -> Outcome:
        """Run one interpreter call; safe to call from multiple threads.

        When ``ptc_tools`` is provided, the call is routed through
        ``sandbox.run_ptc`` so user code can ``await tools.<name>(...)`` —
        each ``host_call`` event is dispatched against ``ptc_tools[name]``.
        Otherwise the standard file-launcher shell path is used.

        ``outer_runtime`` is the ``ToolRuntime`` of the ``py_eval`` call that
        started this program; it is what nested tools receive (with a child
        ``tool_call_id``). ``outer_loop`` is the event loop the agent is
        running on, when there is one, so an async tool executes there
        instead of on a throwaway loop.
        """
        with self._lock:
            try:
                self._install_pending_skills(code, skills, skills_backend)
                sandbox = self._ensure_sandbox()
                if ptc_tools:
                    return self._eval_with_ptc(
                        sandbox,
                        code,
                        _PTCSession(
                            tools=ptc_tools,
                            outer_runtime=outer_runtime,
                            outer_loop=outer_loop,
                            max_ptc_calls=max_ptc_calls,
                        ),
                    )
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
        session: _PTCSession,
    ) -> Outcome:
        run_ptc = getattr(sandbox, "run_ptc", None)
        if run_ptc is None:
            return Outcome.host_error(
                "PTCUnsupported",
                "sandbox does not implement run_ptc",
            )
        try:
            envelope = run_ptc(
                code,
                tools=sorted(session.tools),
                on_host_call=session.dispatch,
            )
        except (RuntimeError, OSError) as exc:
            # Narrow: transport / protocol / capability failures from
            # sandbox.run_ptc. Programmer bugs in the dispatcher must
            # surface as themselves, not be quietly wrapped here.
            logger.exception("wasmsh PTC run failed")
            return Outcome.host_error(type(exc).__name__, str(exc))
        return Outcome.from_envelope(envelope)

    async def eval_async(  # noqa: PLR0913 -- mirrors eval_sync
        self,
        code: str,
        *,
        skills: dict[str, SkillMetadata] | None = None,
        skills_backend: BackendProtocol | None = None,
        ptc_tools: dict[str, BaseTool] | None = None,
        outer_runtime: Any = None,
        max_ptc_calls: int | None = None,
    ) -> Outcome:
        """Async wrapper around :meth:`eval_sync` (runs in a worker thread).

        The running loop is captured before handing off so nested async
        tools are scheduled back onto the agent's own loop rather than a
        fresh one created inside the worker thread.
        """
        return await asyncio.to_thread(
            self.eval_sync,
            code,
            skills=skills,
            skills_backend=skills_backend,
            ptc_tools=ptc_tools,
            outer_runtime=outer_runtime,
            outer_loop=asyncio.get_running_loop(),
            max_ptc_calls=max_ptc_calls,
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


@dataclass
class _PTCSession:
    """One evaluation's PTC context: allowlist, runtime, loop, call budget.

    Created per ``eval`` call, so the budget resets each time the model runs
    a program rather than accumulating across a conversation.
    """

    tools: dict[str, BaseTool]
    outer_runtime: Any = None
    outer_loop: asyncio.AbstractEventLoop | None = None
    max_ptc_calls: int | None = None
    calls_made: int = 0

    def dispatch(self, message: dict[str, Any]) -> dict[str, Any]:
        """Handle one ``host_call`` event and return its result envelope.

        Never raises: a failure here would abort the JSON-RPC exchange and
        strand the sandbox mid-program, so everything is reported back as an
        envelope the interpreter turns into a Python exception the generated
        code can catch.
        """
        name = message.get("tool")
        if not isinstance(name, str) or name not in self.tools:
            return _ptc_error(
                "UnknownToolError",
                f"tool {name!r} is not on the PTC allowlist",
            )
        tool = self.tools[name]

        try:
            self._consume_budget(name)
        except PTCCallBudgetExceededError as exc:
            logger.warning("PTC call budget exceeded on tool %r", name)
            return _ptc_error(type(exc).__name__, str(exc))

        args = normalize_tool_input(message.get("args"))
        try:
            enriched = inject_tool_args_for_ptc(
                tool,
                args,
                self.outer_runtime,
                (call_id := synth_tool_call_id(tool.name)),
            )
            raw = self._invoke(tool, enriched, call_id)
        except Exception as exc:  # noqa: BLE001 -- isolate one tool failure
            # The envelope reaches the sandbox so the model can recover, but
            # the original stack and call context are lost in that
            # conversion. Emit a warning so host applications wiring up
            # `logging.getLogger("langchain_wasmsh")` get the full picture.
            logger.warning(
                "PTC tool %r raised; envelope returned to sandbox",
                name,
                extra={
                    "wasmsh_ptc_call_id": message.get("id"),
                    "wasmsh_ptc_tool": name,
                },
                exc_info=True,
            )
            return _ptc_error(type(exc).__name__, str(exc))
        return {"ok": True, "value": coerce_tool_output_for_ptc(raw)}

    def _consume_budget(self, tool_name: str) -> None:
        if self.max_ptc_calls is None:
            return
        if self.calls_made >= self.max_ptc_calls:
            raise PTCCallBudgetExceededError(
                limit=self.max_ptc_calls,
                tool_name=tool_name,
            )
        self.calls_made += 1

    def _invoke(self, tool: BaseTool, args: dict[str, Any], call_id: str) -> Any:
        """Run one tool, from whichever thread the dispatcher is on.

        `arun` is used for every tool, sync ones included: LangChain routes a
        sync-only tool to a worker thread itself, so one path covers both
        kinds without having to classify the tool first.

        `tool_call_id` is passed only when the tool declares
        `InjectedToolCallId`. Supplying it otherwise makes LangChain wrap the
        return value in a `ToolMessage` with string-coerced content, which
        would flatten the structured results the interpreter is meant to see.
        """
        tool_call_id = call_id if tool_uses_injected_tool_call_id(tool) else None

        if self.outer_loop is not None:
            # Async agent path: the dispatcher runs on an `asyncio.to_thread`
            # worker while the agent's loop is idle, so scheduling back onto
            # it keeps loop-bound tools working and cannot deadlock.
            future = asyncio.run_coroutine_threadsafe(
                tool.arun(args, tool_call_id=tool_call_id),
                self.outer_loop,
            )
            return future.result()

        try:
            asyncio.get_running_loop()
        except RuntimeError:
            pass
        else:
            msg = (
                "PTC dispatch reached a thread with a running event loop but "
                "no outer loop was captured; this means eval_sync was called "
                "from async code. Use eval_async instead."
            )
            raise RuntimeError(msg)

        if _is_async_only(tool):
            return asyncio.run(tool.arun(args, tool_call_id=tool_call_id))
        # Sync agent path with a sync tool: run it inline rather than routing
        # through `arun`, which would hand it to an executor thread. Staying
        # on this thread keeps thread-local context (and the sandbox reentry
        # guard's thread check) meaningful.
        return tool.run(args, tool_call_id=tool_call_id)


def _ptc_error(error: str, message: str) -> dict[str, Any]:
    return {"ok": False, "error": error, "message": message}


def _is_async_only(tool: BaseTool) -> bool:
    """Return whether ``tool`` only provides a coroutine implementation."""
    return getattr(tool, "func", None) is None and (
        getattr(tool, "coroutine", None) is not None
    )


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
