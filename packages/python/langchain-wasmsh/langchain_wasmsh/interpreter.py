"""``WasmshInterpreterMiddleware`` — a Python REPL middleware for Deep Agents.

Persistent Python interpreter exposed as a single agent tool, backed by a
long-lived wasmsh sandbox (Pyodide running inside WebAssembly). The shape
mirrors :class:`langchain_quickjs.CodeInterpreterMiddleware`:

- one tool (``py_eval`` by default) accepting ``code: str``;
- state (variables, imports, defined functions) persists across calls and
  across agent turns via a globals-pickle snapshot stored in agent state;
- optional skills loading via a paired ``SkillsMiddleware`` + the same
  ``BackendProtocol`` so ``import skills.<name>`` works inside user code;
- programmatic tool calling (PTC): pass ``ptc=[...]`` to expose selected
  agent tools inside the sandbox as ``tools.<snake_name>`` awaitables.
  Routed through the sandbox's ``runPtc`` JSON-RPC method.

Unlike the QuickJS interpreter, the sandbox here gives **real host-memory
isolation** (the sandbox is a separate WebAssembly module / subprocess with
its own VFS), plus a full filesystem under ``/workspace`` and access to the
wasmsh shell utilities via ``subprocess.run``.
"""

from __future__ import annotations

import asyncio
import contextlib
import logging
import uuid
import warnings
from collections.abc import Awaitable, Callable
from typing import TYPE_CHECKING, Annotated, Any, ClassVar, NotRequired, get_args

from langchain.agents.middleware.types import (
    AgentMiddleware,
    AgentState,
    ContextT,
    ModelRequest,
    ModelResponse,
    PrivateStateAttr,
    ResponseT,
)
from langchain.tools import (
    ToolRuntime,  # noqa: TC002 -- needed at runtime for langgraph's ToolNode injection scan
)
from langchain_core._api import beta
from langchain_core.messages import ToolMessage
from langchain_core.tools import BaseTool, StructuredTool
from langgraph.config import get_config
from pydantic import BaseModel, Field

from langchain_wasmsh._prompt import (
    PersistenceMode,
    append_system_prompt_block,
    render_repl_system_prompt,
    to_snake_case,
)
from langchain_wasmsh._ptc import (
    DEFAULT_MAX_PTC_CALLS,
    PTCOption,
    filter_tools_for_ptc,
    render_ptc_prompt,
)
from langchain_wasmsh._repl import (
    Outcome,
    SandboxLike,
    _Registry,
    format_outcome,
)
from langchain_wasmsh.sandbox import WasmshSandbox

if TYPE_CHECKING:
    from deepagents.backends.protocol import BackendProtocol
    from deepagents.middleware.skills import SkillMetadata
    from langchain_core.messages import SystemMessage
    from langgraph.runtime import Runtime


logger = logging.getLogger(__name__)

_DEFAULT_TIMEOUT = 30.0
_DEFAULT_MAX_RESULT_CHARS = 4_000
_DEFAULT_TOOL_NAME = "py_eval"
_DEFAULT_MAX_SNAPSHOT_BYTES = 8 * 1024 * 1024  # 8 MiB pickle cap
_VALID_MODES: tuple[str, ...] = get_args(PersistenceMode)
_MODE_UNSET = object()

# Module-level so `Annotated[str, _CODE_DOC]` can be resolved by
# `typing.get_type_hints(...)`, which langgraph's ToolNode invokes when
# scanning for InjectedToolCallId. A closure variable would not be visible.
_CODE_DOC = "Python source to run in the persistent wasmsh REPL. State persists."


SandboxFactory = Callable[[], SandboxLike]
"""Factory returning anything that satisfies the wasmsh REPL sandbox surface.

Concrete returns may be ``WasmshSandbox`` or ``WasmshRemoteSandbox`` (once the
dispatcher path lands ``run_ptc``), or any structural stand-in used in tests.
"""


class WasmshReplState(AgentState):
    """State schema for :class:`WasmshInterpreterMiddleware`."""

    _wasmsh_snapshot_payload: NotRequired[Annotated[bytes | None, PrivateStateAttr]]


class _PyEvalSchema(BaseModel):
    """Input schema for the Python interpreter tool."""

    code: str = Field(
        description=(
            "Python source to run in the persistent wasmsh REPL. "
            "State (variables, imports, defined functions) persists across "
            "calls and across turns. Use `print(...)` for intermediate output; "
            "a final bare expression is also returned automatically."
        ),
    )


def _resolve_thread_id(fallback: str) -> str:
    """Return the most specific session key LangGraph exposes for this run.

    Preference order:

    1. ``configurable.thread_id`` — the real conversation thread, present
       whenever a checkpointer is configured.
    2. ``run_id`` — no thread, but still unique per invocation, so two
       concurrent checkpointer-less runs get their own interpreter instead
       of sharing (and corrupting) one set of REPL globals.
    3. ``fallback`` — a per-middleware-instance id, used only outside any
       LangGraph run (direct tool invocation in tests, for instance).
    """
    try:
        config = get_config()
    except RuntimeError:
        return fallback
    if not config:
        return fallback
    thread_id = config.get("configurable", {}).get("thread_id")
    if thread_id is not None:
        return str(thread_id)
    run_id = config.get("run_id")
    if run_id is not None:
        return f"run:{run_id}"
    return fallback


def _default_sandbox_factory() -> WasmshSandbox:
    """Build a default wasmsh sandbox with sensible REPL defaults."""
    return WasmshSandbox()


def _resolve_mode(
    mode: PersistenceMode | None,
    snapshot_between_turns: bool | Any,  # noqa: FBT001 -- mirrors the deprecated kwarg
) -> PersistenceMode:
    """Reconcile the ``mode`` argument with its deprecated boolean alias.

    Accepting both without a conflict check would let a caller write
    ``mode="call", snapshot_between_turns=True`` and silently get one of the
    two, so a combination that expresses two different intents is rejected
    outright rather than resolved by precedence.
    """
    legacy_given = snapshot_between_turns is not _MODE_UNSET
    if mode is not None and legacy_given:
        msg = (
            "Pass either `mode` or the deprecated `snapshot_between_turns`, "
            "not both. `snapshot_between_turns=True` is `mode='thread'` and "
            "`False` is `mode='turn'`."
        )
        raise ValueError(msg)
    if legacy_given:
        if not isinstance(snapshot_between_turns, bool):
            msg = "`snapshot_between_turns` must be a bool"
            raise TypeError(msg)
        warnings.warn(
            "`snapshot_between_turns` is deprecated and will be removed in the "
            "next minor release; use mode='thread' (True) or mode='turn' "
            "(False) instead.",
            DeprecationWarning,
            stacklevel=3,
        )
        return "thread" if snapshot_between_turns else "turn"
    if mode is None:
        return "thread"
    if mode not in _VALID_MODES:
        msg = f"`mode` must be one of {_VALID_MODES}, got {mode!r}"
        raise ValueError(msg)
    return mode


@beta()
class WasmshInterpreterMiddleware(
    AgentMiddleware[WasmshReplState, ContextT, ResponseT],
):
    """Persistent Python interpreter middleware backed by wasmsh.

    Each LangGraph thread gets its own wasmsh sandbox session. How long that
    session lives is chosen with ``mode``; with the default ``"thread"`` it
    survives across turns via a globals-pickle held in private agent state.

    Args:
        sandbox_factory: Callable returning a fresh ``WasmshSandbox`` for one
            REPL session. Defaults to ``WasmshSandbox()``. Override to set
            ``allowed_hosts``, ``initial_files``, ``step_budget``, etc.
        timeout: Per-call wall-clock timeout in seconds advertised to the
            model. The sandbox enforces budgets via ``step_budget``; this is
            a prompt-only knob today.
        tool_name: Name of the tool exposed to the model. Default ``py_eval``.
        max_result_chars: Truncate each result block (stdout, stderr, value,
            error) to this many characters before sending to the model.
        ptc: Programmatic tool calling. Pass a list of agent tool names or
            ``BaseTool`` instances; each will be exposed inside the sandbox
            as an awaitable on the ``tools`` namespace (snake-cased). PTC
            calls bypass the regular ``ToolNode`` path, so ``interrupt_on``
            approval is NOT enforced for them — treat the allowlist as your
            permission boundary. ``None`` (default) disables PTC.
        max_ptc_calls: Cap on nested tool calls within a single ``py_eval``
            evaluation. The budget resets per evaluation. Default 256;
            ``None`` disables the cap. Exists so a loop in generated code
            cannot issue unbounded tool calls inside one turn.
        skills_backend: Optional ``BackendProtocol``. When set and a paired
            ``SkillsMiddleware`` populates ``skills_metadata``, skills with
            Python source under their directory become importable inside the
            REPL via ``import skills.<name>``.
        mode: Interpreter lifetime, matching the convention the LangChain
            interpreter middlewares share:

            - ``"thread"`` (default): globals persist across calls *and*
              turns, carried between turns as a pickle in private state.
            - ``"turn"``: globals persist across calls inside one agent turn,
              then the session is evicted. Nothing is written to state.
            - ``"call"``: every ``py_eval`` call gets a fresh interpreter.
        snapshot_between_turns: Deprecated alias kept for one minor release.
            ``True`` maps to ``mode="thread"``, ``False`` to ``mode="turn"``.
            Passing it together with ``mode`` is an error.
        max_snapshot_bytes: Drop the snapshot if the pickle exceeds this many
            bytes. Default 8 MiB.

    Example:
        ```python
        from deepagents import create_deep_agent
        from langchain_wasmsh import WasmshInterpreterMiddleware

        agent = create_deep_agent(
            model="claude-sonnet-4-6",
            middleware=[WasmshInterpreterMiddleware()],
        )
        ```
    """

    state_schema = WasmshReplState

    serialized_name: ClassVar[str] = "WasmshInterpreterMiddleware"
    """Stable public alias for profile exclusions and config round-trips.

    `HarnessProfile.excluded_middleware` matches either a class or an
    `AgentMiddleware.name`, and `HarnessProfileConfig.from_harness_profile`
    can only serialize a class entry when the class advertises this alias.
    Declaring it explicitly — rather than letting it default to the class
    `__name__` — is what makes `excluded_middleware={WasmshInterpreterMiddleware}`
    round-trip through `to_dict()`/`from_dict()`, and pins the name against a
    future rename.
    """

    def __init__(  # noqa: PLR0913 -- public API mirrors CodeInterpreterMiddleware
        self,
        *,
        sandbox_factory: SandboxFactory | None = None,
        timeout: float = _DEFAULT_TIMEOUT,
        tool_name: str = _DEFAULT_TOOL_NAME,
        max_result_chars: int = _DEFAULT_MAX_RESULT_CHARS,
        ptc: PTCOption | None = None,
        skills_backend: BackendProtocol | None = None,
        max_ptc_calls: int | None = DEFAULT_MAX_PTC_CALLS,
        mode: PersistenceMode | None = None,
        snapshot_between_turns: bool | Any = _MODE_UNSET,
        max_snapshot_bytes: int | None = _DEFAULT_MAX_SNAPSHOT_BYTES,
    ) -> None:
        """Build the middleware; see the class docstring for parameter details."""
        super().__init__()
        if ptc is not None and not isinstance(ptc, list):
            msg = "`ptc` must be a list of tool names / BaseTool instances or None"
            raise TypeError(msg)
        if max_snapshot_bytes is not None and max_snapshot_bytes < 1:
            msg = "`max_snapshot_bytes` must be >= 1 or None"
            raise ValueError(msg)
        if max_ptc_calls is not None and max_ptc_calls < 1:
            msg = "`max_ptc_calls` must be >= 1 or None"
            raise ValueError(msg)
        self._mode = _resolve_mode(mode, snapshot_between_turns)
        self._sandbox_factory: SandboxFactory = (
            sandbox_factory or _default_sandbox_factory
        )
        self._timeout = timeout
        self._tool_name = tool_name
        self._max_result_chars = max_result_chars
        self._ptc = ptc
        self._max_ptc_calls = max_ptc_calls
        self._skills_backend = skills_backend
        self._max_snapshot_bytes = max_snapshot_bytes
        self._registry = _Registry(self._sandbox_factory)
        self._base_system_prompt = render_repl_system_prompt(
            tool_name=tool_name,
            timeout=timeout,
            max_result_chars=max_result_chars,
            mode=self._mode,
        )
        self._ptc_prompt_cache: tuple[frozenset[str], str] | None = None
        # Per-thread snapshot of the exposed PTC tools for this turn. Keyed
        # by thread_id (resolved from langgraph config or the fallback). The
        # eval tool reads it; `_prepare_for_call` repopulates it before each
        # model call.
        self._exposed_ptc_tools: dict[str, dict[str, BaseTool]] = {}
        self._fallback_thread_id = f"wasmsh_session_{uuid.uuid4().hex[:8]}"
        self.tools: list[BaseTool] = [self._build_tool()]

    # ---- tool construction ----------------------------------------------

    def _build_tool(self) -> BaseTool:
        tool_name = self._tool_name
        registry = self._registry
        max_chars = self._max_result_chars
        fallback_id = self._fallback_thread_id
        middleware = self

        def _wrap(outcome: Outcome, tool_call_id: str | None) -> ToolMessage:
            return ToolMessage(
                content=format_outcome(outcome, max_result_chars=max_chars),
                tool_call_id=tool_call_id,
                name=tool_name,
            )

        def sync_eval(
            runtime: ToolRuntime[None, Any],
            code: Annotated[str, _CODE_DOC],
        ) -> ToolMessage:
            thread_id = _resolve_thread_id(fallback_id)
            repl = registry.get(thread_id)
            skills = middleware._skills_for_eval(runtime)
            ptc_tools = middleware._exposed_ptc_tools.get(thread_id)
            try:
                outcome = repl.eval_sync(
                    code,
                    skills=skills,
                    skills_backend=middleware._skills_backend,
                    ptc_tools=ptc_tools,
                    outer_runtime=runtime,
                    max_ptc_calls=middleware._max_ptc_calls,
                )
            finally:
                if middleware._mode == "call":
                    registry.evict(thread_id)
            return _wrap(outcome, runtime.tool_call_id)

        async def async_eval(
            runtime: ToolRuntime[None, Any],
            code: Annotated[str, _CODE_DOC],
        ) -> ToolMessage:
            thread_id = _resolve_thread_id(fallback_id)
            repl = registry.get(thread_id)
            skills = middleware._skills_for_eval(runtime)
            ptc_tools = middleware._exposed_ptc_tools.get(thread_id)
            try:
                outcome = await repl.eval_async(
                    code,
                    skills=skills,
                    skills_backend=middleware._skills_backend,
                    ptc_tools=ptc_tools,
                    outer_runtime=runtime,
                    max_ptc_calls=middleware._max_ptc_calls,
                )
            finally:
                if middleware._mode == "call":
                    await asyncio.to_thread(registry.evict, thread_id)
            return _wrap(outcome, runtime.tool_call_id)

        # Let StructuredTool infer the schema from the function signature so
        # langgraph's ToolNode can see the `runtime: ToolRuntime[...]`
        # annotation and inject it; passing an explicit args_schema that
        # only lists `code` causes runtime to be stripped before the call.
        sync_eval.__doc__ = _CODE_DOC
        async_eval.__doc__ = _CODE_DOC
        return StructuredTool.from_function(
            name=tool_name,
            description=(
                "Execute Python in a persistent wasmsh sandbox REPL. "
                "Variables, imports, and defined functions persist across calls. "
                "A virtual filesystem is available; shell utilities are reachable "
                "via subprocess. The sandbox is WebAssembly-isolated; outbound "
                "network calls are blocked unless explicitly allowlisted."
            ),
            func=sync_eval,
            coroutine=async_eval,
            metadata={"ls_code_input_language": "python"},
        )

    def _skills_for_eval(
        self,
        runtime: ToolRuntime[None, Any],
    ) -> dict[str, SkillMetadata] | None:
        if self._skills_backend is None:
            return None
        metadata_list = (
            runtime.state.get("skills_metadata", []) if runtime.state else []
        )
        return {m["name"]: m for m in metadata_list}

    # ---- lifecycle hooks -------------------------------------------------

    def before_agent(
        self,
        state: WasmshReplState,
        runtime: Runtime[ContextT],  # noqa: ARG002 -- middleware hook signature
    ) -> dict[str, Any] | None:
        """Restore the globals-pickle into the current thread's REPL."""
        if self._mode != "thread":
            return None
        payload = state.get("_wasmsh_snapshot_payload")
        if payload is None:
            return None
        thread_id = _resolve_thread_id(self._fallback_thread_id)
        repl = self._registry.get(thread_id)
        try:
            repl.restore_snapshot(payload)
        except Exception:  # noqa: BLE001 -- best-effort snapshot path
            # A corrupt or unreadable snapshot must not take the rest of the
            # checkpoint with it: clear only this private field and continue
            # with an empty interpreter.
            logger.warning(
                "Failed to restore wasmsh snapshot for thread_id=%s",
                thread_id,
                exc_info=True,
            )
            return {"_wasmsh_snapshot_payload": None}
        return None

    async def abefore_agent(
        self,
        state: WasmshReplState,
        runtime: Runtime[ContextT],
    ) -> dict[str, Any] | None:
        """Async variant of :meth:`before_agent`.

        Restoring uploads the pickle into the sandbox over a blocking
        stdio/HTTP round-trip, so it runs on a worker thread rather than
        stalling the event loop the agent is scheduled on.
        """
        return await asyncio.to_thread(self.before_agent, state, runtime)

    def wrap_model_call(
        self,
        request: ModelRequest[ContextT],
        handler: Callable[[ModelRequest[ContextT]], ModelResponse[ResponseT]],
    ) -> ModelResponse[ResponseT]:
        """Inject the REPL system prompt on every model call."""
        prompt = self._prepare_for_call(request)
        return handler(
            request.override(
                system_message=self._extend(request.system_message, prompt),
            ),
        )

    async def awrap_model_call(
        self,
        request: ModelRequest[ContextT],
        handler: Callable[
            [ModelRequest[ContextT]],
            Awaitable[ModelResponse[ResponseT]],
        ],
    ) -> ModelResponse[ResponseT]:
        """Async variant of :meth:`wrap_model_call`."""
        prompt = self._prepare_for_call(request)
        return await handler(
            request.override(
                system_message=self._extend(request.system_message, prompt),
            ),
        )

    def _prepare_for_call(self, request: ModelRequest[ContextT]) -> str:
        if self._ptc is None:
            return self._base_system_prompt
        request_tools: list[BaseTool] = list(getattr(request, "tools", []) or [])
        exposed = filter_tools_for_ptc(
            request_tools,
            self._ptc,
            self_tool_name=self._tool_name,
        )
        # Snake-case key: that's the attribute name user code sees on `tools`.
        thread_id = _resolve_thread_id(self._fallback_thread_id)
        self._exposed_ptc_tools[thread_id] = {to_snake_case(t.name): t for t in exposed}
        exposed_names = frozenset(t.name for t in exposed)
        if self._ptc_prompt_cache is None or self._ptc_prompt_cache[0] != exposed_names:
            self._ptc_prompt_cache = (
                exposed_names,
                render_ptc_prompt(exposed, tool_name=self._tool_name),
            )
        return self._base_system_prompt + self._ptc_prompt_cache[1]

    def _extend(
        self,
        system_message: SystemMessage | None,
        prompt: str,
    ) -> SystemMessage:
        """Append the interpreter prompt without flattening what came before.

        By the time this runs, memory, skills, harness-profile and
        prompt-caching middleware may all have contributed structured content
        blocks — including `cache_control` markers. Rebuilding the message
        from `.content` as one string would drop them, so the append is
        block-aware; see
        :func:`langchain_wasmsh._prompt.append_system_prompt_block`.
        """
        return append_system_prompt_block(system_message, prompt)

    def after_agent(
        self,
        state: WasmshReplState,  # noqa: ARG002 -- middleware hook signature
        runtime: Runtime[ContextT],  # noqa: ARG002 -- middleware hook signature
    ) -> dict[str, Any] | None:
        """Snapshot the REPL globals (if any) and evict the thread slot."""
        thread_id = _resolve_thread_id(self._fallback_thread_id)
        self._exposed_ptc_tools.pop(thread_id, None)
        if self._mode != "thread":
            # "turn" and "call" both end the turn with no interpreter state
            # in the checkpoint. "call" has already evicted after each eval;
            # evicting again is a no-op.
            self._registry.evict(thread_id)
            return None
        repl = self._registry.get_if_exists(thread_id)
        if repl is None:
            return None
        update: dict[str, Any] = {}
        try:
            payload = repl.create_snapshot()
            update = self._snapshot_update(payload=payload, thread_id=thread_id)
        except Exception:  # noqa: BLE001 -- best-effort snapshot path
            logger.warning(
                "Failed to read wasmsh snapshot for thread_id=%s",
                thread_id,
                exc_info=True,
            )
            update = {"_wasmsh_snapshot_payload": None}
        finally:
            self._registry.evict(thread_id)
        return update

    async def aafter_agent(
        self,
        state: WasmshReplState,
        runtime: Runtime[ContextT],
    ) -> dict[str, Any] | None:
        """Async variant of :meth:`after_agent`.

        Reading the snapshot back out of the sandbox and closing the session
        are blocking transfers, so they run on a worker thread.
        """
        return await asyncio.to_thread(self.after_agent, state, runtime)

    def _snapshot_update(
        self,
        *,
        payload: bytes | None,
        thread_id: str,
    ) -> dict[str, bytes | None]:
        if payload is None:
            return {"_wasmsh_snapshot_payload": None}
        cap = self._max_snapshot_bytes
        if cap is not None and len(payload) > cap:
            logger.warning(
                "Dropping wasmsh snapshot for thread_id=%s "
                "(size=%d bytes exceeds max_snapshot_bytes=%d)",
                thread_id,
                len(payload),
                self._max_snapshot_bytes,
            )
            return {"_wasmsh_snapshot_payload": None}
        return {"_wasmsh_snapshot_payload": payload}

    def __del__(self) -> None:
        """Best-effort registry shutdown on GC."""
        with contextlib.suppress(Exception):
            self._registry.close()
