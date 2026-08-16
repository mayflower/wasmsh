"""The `create_deep_agent` constructor surface, with wasmsh active.

One test per row of the 0.7.4 integration surface: model, tools,
system_prompt, middleware, backend, interrupt_on, response_format,
state_schema, context_schema, checkpointer, cache, debug/name — plus the
filesystem tool list and execute artifacts those rows depend on.
"""

from __future__ import annotations

import asyncio
from dataclasses import dataclass
from typing import Any

import pytest
from deepagents import create_deep_agent
from deepagents.graph import DeepAgentState
from langchain.agents.middleware.types import AgentMiddleware
from langchain.tools import (
    ToolRuntime,  # noqa: TC002 -- needed at runtime for ToolNode's injection scan
)
from langchain_core.messages import AIMessage
from langchain_core.tools import tool
from langgraph.cache.memory import InMemoryCache
from langgraph.checkpoint.memory import InMemorySaver
from langgraph.store.memory import InMemoryStore
from langgraph.types import Command
from pydantic import BaseModel, Field

from langchain_wasmsh import WasmshInterpreterMiddleware
from langchain_wasmsh._prompt import append_system_prompt_block
from tests.integration_tests._harness import (
    ScriptedModel,
    ainvoke,
    call,
    invoke,
    last_tool_message,
    requires_assets,
    script,
    system_text,
)

pytestmark = requires_assets


class Report(BaseModel):
    """Structured return value for the tool-variety test."""

    score: int


@dataclass
class Identity:
    """Runtime context schema used by the context tests."""

    user_id: str
    assistant_id: str = "assistant-a"


# ── tools & execute artifacts ──────────────────────────────────────────


class TestFilesystemToolSurface:
    def test_full_0_7_4_tool_list_is_bound(self, shared_sandbox: Any) -> None:
        model = ScriptedModel(messages=script(AIMessage(content="done")))
        invoke(create_deep_agent(model=model, backend=shared_sandbox))
        # `delete` is conditional on the backend implementing it, and `task`
        # on the default general-purpose subagent existing.
        assert model.tool_names == [
            "ls",
            "read_file",
            "write_file",
            "edit_file",
            "delete",
            "glob",
            "grep",
            "execute",
            "task",
        ]

    def test_write_then_read_round_trips_through_the_middleware(
        self,
        shared_sandbox: Any,
    ) -> None:
        model = ScriptedModel(
            messages=script(
                call(
                    "write_file",
                    {"file_path": "/workspace/rt.txt", "content": "alpha\nbeta"},
                ),
                call("read_file", {"file_path": "/workspace/rt.txt"}, "call_2"),
                AIMessage(content="done"),
            ),
        )
        result = invoke(create_deep_agent(model=model, backend=shared_sandbox))
        read_back = last_tool_message(result, "read_file")
        # The gutter is added by the filesystem middleware, not the backend,
        # so seeing it proves the read went through the whole stack.
        assert "1\talpha" in read_back.content or "1  alpha" in read_back.content
        assert "beta" in read_back.content

    @pytest.mark.parametrize(
        ("command", "expected_exit"),
        [("true", 0), ("exit 3", 3)],
    )
    def test_execute_artifact_carries_the_exit_code(
        self,
        shared_sandbox: Any,
        command: str,
        expected_exit: int,
    ) -> None:
        model = ScriptedModel(
            messages=script(
                call("execute", {"command": command}),
                AIMessage(content="done"),
            ),
        )
        result = invoke(create_deep_agent(model=model, backend=shared_sandbox))
        message = last_tool_message(result, "execute")
        # A failing command is still `status="success"`: the command ran and
        # the model is expected to read the exit code, not the status.
        assert message.status == "success"
        assert message.artifact == {"exit_code": expected_exit}

    def test_delete_is_registered_and_removes_a_directory_tree(
        self,
        shared_sandbox: Any,
    ) -> None:
        shared_sandbox.upload_files(
            [
                ("/workspace/tree/a.txt", b"a"),
                ("/workspace/tree/sub/b.txt", b"b"),
            ],
        )
        model = ScriptedModel(
            messages=script(
                call("delete", {"file_path": "/workspace/tree"}),
                AIMessage(content="done"),
            ),
        )
        result = invoke(create_deep_agent(model=model, backend=shared_sandbox))
        assert last_tool_message(result, "delete").status == "success"
        assert shared_sandbox.execute("test -e /workspace/tree").exit_code != 0


class TestOrdinaryTools:
    def test_sync_structured_runtime_and_failing_tools(
        self,
        shared_sandbox: Any,
    ) -> None:
        @tool
        def sync_tool(x: int) -> str:
            """A sync tool."""
            return f"sync:{x}"

        @tool
        def structured_tool() -> Report:
            """Returns a pydantic model."""
            return Report(score=9)

        @tool
        def runtime_tool(runtime: ToolRuntime) -> str:
            """Reads the runtime context."""
            return f"user:{runtime.context.user_id}"

        model = ScriptedModel(
            messages=script(
                call("sync_tool", {"x": 1}, "c1"),
                call("structured_tool", {}, "c2"),
                call("runtime_tool", {}, "c3"),
                AIMessage(content="done"),
            ),
        )
        agent = create_deep_agent(
            model=model,
            backend=shared_sandbox,
            tools=[sync_tool, structured_tool, runtime_tool],
            context_schema=Identity,
        )
        result = invoke(agent, context=Identity(user_id="u-42"))

        assert last_tool_message(result, "sync_tool").content == "sync:1"
        assert "score" in last_tool_message(result, "structured_tool").content
        assert last_tool_message(result, "runtime_tool").content == "user:u-42"

    def test_a_raising_tool_propagates_rather_than_being_swallowed(
        self,
        shared_sandbox: Any,
    ) -> None:
        # 0.7.4 does not convert an ordinary tool exception into an error
        # ToolMessage; it surfaces. Asserted so a future upstream change to
        # that policy is noticed here rather than in production.
        @tool
        def failing_tool() -> str:
            """Always raises."""
            msg = "tool exploded"
            raise RuntimeError(msg)

        model = ScriptedModel(
            messages=script(
                call("failing_tool", {}),
                AIMessage(content="done"),
            ),
        )
        agent = create_deep_agent(
            model=model,
            backend=shared_sandbox,
            tools=[failing_tool],
        )
        with pytest.raises(RuntimeError, match="tool exploded"):
            invoke(agent)

    async def test_async_only_tool_runs_under_async_invocation(
        self,
        shared_sandbox: Any,
    ) -> None:
        # LangChain refuses to run a coroutine-only tool from a sync graph
        # run, so an async-only tool belongs to the async path.
        @tool
        async def async_tool(x: int) -> str:
            """An async tool."""
            await asyncio.sleep(0)
            return f"async:{x}"

        model = ScriptedModel(
            messages=script(
                call("async_tool", {"x": 2}),
                AIMessage(content="done"),
            ),
        )
        agent = create_deep_agent(
            model=model,
            backend=shared_sandbox,
            tools=[async_tool],
        )
        result = await ainvoke(agent)
        assert last_tool_message(result, "async_tool").content == "async:2"


# ── prompt assembly ────────────────────────────────────────────────────


class TestSystemPromptAssembly:
    def test_caller_prompt_and_memory_appear_once_in_upstream_order(
        self,
        shared_sandbox: Any,
    ) -> None:
        shared_sandbox.upload_files(
            [("/workspace/mem/AGENTS.md", b"REMEMBER-THE-PASSPHRASE\n")],
        )
        model = ScriptedModel(messages=script(AIMessage(content="done")))
        agent = create_deep_agent(
            model=model,
            backend=shared_sandbox,
            system_prompt="CALLER-PROMPT",
            memory=["/workspace/mem/AGENTS.md"],
        )
        invoke(agent)

        text = model.system_text
        assert text.count("CALLER-PROMPT") == 1
        assert text.count("REMEMBER-THE-PASSPHRASE") == 1
        # The caller's own prompt leads; assembled context follows it.
        assert text.index("CALLER-PROMPT") < text.index("REMEMBER-THE-PASSPHRASE")

    def test_missing_optional_memory_file_does_not_fail_the_run(
        self,
        shared_sandbox: Any,
    ) -> None:
        model = ScriptedModel(messages=script(AIMessage(content="done")))
        agent = create_deep_agent(
            model=model,
            backend=shared_sandbox,
            memory=["/workspace/mem/absent.md"],
        )
        assert invoke(agent)["messages"][-1].content == "done"


# ── middleware merging ─────────────────────────────────────────────────


class _MarkerMiddleware(AgentMiddleware):
    """Appends a marker to the system prompt so merging is observable."""

    def __init__(self, marker: str, name: str | None = None) -> None:
        super().__init__()
        self.marker = marker
        if name is not None:
            self._name_override = name

    @property
    def name(self) -> str:
        return getattr(self, "_name_override", type(self).__name__)

    def wrap_model_call(self, request: Any, handler: Any) -> Any:
        return handler(
            request.override(
                system_message=append_system_prompt_block(
                    request.system_message,
                    self.marker,
                ),
            ),
        )


class TestMiddlewareMerging:
    def test_extra_middleware_is_applied(self, shared_sandbox: Any) -> None:
        model = ScriptedModel(messages=script(AIMessage(content="done")))
        agent = create_deep_agent(
            model=model,
            backend=shared_sandbox,
            middleware=[_MarkerMiddleware("MARKER-ONE")],
        )
        invoke(agent)
        assert model.system_text.count("MARKER-ONE") == 1

    def test_matching_name_replaces_a_built_in_exactly_once(
        self,
        shared_sandbox: Any,
    ) -> None:
        # Upstream merges custom middleware by `.name`: a name already in the
        # stack replaces that slot in place rather than adding a second
        # entry. `SummarizationMiddleware` is in the 0.7.4 default stack.
        model = ScriptedModel(messages=script(AIMessage(content="done")))
        agent = create_deep_agent(
            model=model,
            backend=shared_sandbox,
            middleware=[_MarkerMiddleware("REPLACED", name="SummarizationMiddleware")],
        )
        invoke(agent)
        assert model.system_text.count("REPLACED") == 1

    def test_two_user_middlewares_sharing_a_name_are_rejected(
        self,
        shared_sandbox: Any,
    ) -> None:
        # Upstream refuses the ambiguity rather than silently keeping one.
        with pytest.raises(AssertionError, match="duplicate middleware"):
            create_deep_agent(
                model=ScriptedModel(messages=script(AIMessage(content="done"))),
                backend=shared_sandbox,
                middleware=[
                    _MarkerMiddleware("FIRST", name="SharedName"),
                    _MarkerMiddleware("SECOND", name="SharedName"),
                ],
            )


# ── state, context, checkpointer, store ────────────────────────────────


class TestStateAndContext:
    def test_custom_state_field_survives_a_run(self, shared_sandbox: Any) -> None:
        class CustomState(DeepAgentState):
            ticket_id: str

        model = ScriptedModel(messages=script(AIMessage(content="done")))
        agent = create_deep_agent(
            model=model,
            backend=shared_sandbox,
            state_schema=CustomState,
        )
        result = agent.invoke(
            {"messages": [{"role": "user", "content": "go"}], "ticket_id": "T-1"},
        )
        assert result["ticket_id"] == "T-1"

    def test_private_repl_state_is_not_exposed_as_caller_state(
        self,
        shared_sandbox: Any,
    ) -> None:
        model = ScriptedModel(messages=script(AIMessage(content="done")))
        agent = create_deep_agent(
            model=model,
            backend=shared_sandbox,
            middleware=[
                WasmshInterpreterMiddleware(sandbox_factory=lambda: shared_sandbox),
            ],
        )
        result = invoke(agent)
        # The snapshot is execution state for one thread, not agent state.
        assert "_wasmsh_snapshot_payload" not in result


class TestCheckpointer:
    def test_same_thread_continues_and_another_starts_clean(
        self,
        shared_sandbox: Any,
    ) -> None:
        saver = InMemorySaver()
        model = ScriptedModel(
            messages=script(
                AIMessage(content="first"),
                AIMessage(content="second"),
                AIMessage(content="fresh"),
            ),
        )
        agent = create_deep_agent(
            model=model,
            backend=shared_sandbox,
            checkpointer=saver,
        )
        thread_a = {"configurable": {"thread_id": "a"}}
        invoke(agent, "one", config=thread_a)
        second = invoke(agent, "two", config=thread_a)
        # Thread A accumulated both turns.
        assert len(second["messages"]) == 4

        thread_b = {"configurable": {"thread_id": "b"}}
        fresh = invoke(agent, "one", config=thread_b)
        assert len(fresh["messages"]) == 2

    def test_a_reconstructed_graph_resumes_the_same_thread(
        self,
        shared_sandbox: Any,
    ) -> None:
        saver = InMemorySaver()
        config = {"configurable": {"thread_id": "persisted"}}

        first_model = ScriptedModel(messages=script(AIMessage(content="first")))
        invoke(
            create_deep_agent(
                model=first_model,
                backend=shared_sandbox,
                checkpointer=saver,
            ),
            "one",
            config=config,
        )

        # New graph object, new model, same saver: the thread survives the
        # objects that created it.
        second_model = ScriptedModel(messages=script(AIMessage(content="second")))
        result = invoke(
            create_deep_agent(
                model=second_model,
                backend=shared_sandbox,
                checkpointer=saver,
            ),
            "two",
            config=config,
        )
        assert [m.content for m in result["messages"]] == [
            "one",
            "first",
            "two",
            "second",
        ]


class TestStoreAndCache:
    def test_a_runtime_tool_can_read_and_write_the_graph_store(
        self,
        shared_sandbox: Any,
    ) -> None:
        store = InMemoryStore()

        @tool
        def remember(value: str, runtime: ToolRuntime) -> str:
            """Persist a value in the graph store."""
            runtime.store.put(("notes",), "k", {"value": value})
            return runtime.store.get(("notes",), "k").value["value"]

        model = ScriptedModel(
            messages=script(
                call("remember", {"value": "kept"}),
                AIMessage(content="done"),
            ),
        )
        agent = create_deep_agent(
            model=model,
            backend=shared_sandbox,
            tools=[remember],
            store=store,
        )
        result = invoke(agent)
        assert last_tool_message(result, "remember").content == "kept"
        assert store.get(("notes",), "k").value == {"value": "kept"}

    def test_cache_enabled_graph_still_runs(self, shared_sandbox: Any) -> None:
        model = ScriptedModel(messages=script(AIMessage(content="cached-run")))
        agent = create_deep_agent(
            model=model,
            backend=shared_sandbox,
            cache=InMemoryCache(),
        )
        assert invoke(agent)["messages"][-1].content == "cached-run"


# ── structured output, HITL, naming ────────────────────────────────────


class TestStructuredResponse:
    def test_response_format_produces_a_typed_result(
        self,
        shared_sandbox: Any,
    ) -> None:
        class Summary(BaseModel):
            """Final structured answer."""

            headline: str = Field(description="One-line summary")
            confidence: int

        model = ScriptedModel(
            messages=script(
                call("Summary", {"headline": "all good", "confidence": 4}, "c1"),
            ),
        )
        agent = create_deep_agent(
            model=model,
            backend=shared_sandbox,
            response_format=Summary,
        )
        result = invoke(agent)
        assert result["structured_response"] == Summary(
            headline="all good",
            confidence=4,
        )


class TestInterruptOn:
    def test_a_gated_tool_pauses_and_resumes(self, shared_sandbox: Any) -> None:
        model = ScriptedModel(
            messages=script(
                call("execute", {"command": "echo gated"}),
                AIMessage(content="done"),
            ),
        )
        agent = create_deep_agent(
            model=model,
            backend=shared_sandbox,
            checkpointer=InMemorySaver(),
            interrupt_on={"execute": True},
        )
        config = {"configurable": {"thread_id": "hitl"}}
        paused = invoke(agent, "go", config=config)
        assert paused.get("__interrupt__"), "expected the run to pause"

        resumed = agent.invoke(
            Command(resume={"decisions": [{"type": "approve"}]}),
            config=config,
        )
        assert "gated" in last_tool_message(resumed, "execute").content
        assert resumed["messages"][-1].content == "done"


class TestPassThroughOptions:
    def test_name_and_debug_are_accepted(self, shared_sandbox: Any) -> None:
        model = ScriptedModel(messages=script(AIMessage(content="named")))
        agent = create_deep_agent(
            model=model,
            backend=shared_sandbox,
            name="wasmsh-agent",
            debug=True,
        )
        assert agent.name == "wasmsh-agent"
        assert invoke(agent)["messages"][-1].content == "named"


class TestAsyncParity:
    async def test_the_same_graph_runs_through_ainvoke(
        self,
        shared_sandbox: Any,
    ) -> None:
        model = ScriptedModel(
            messages=script(
                call(
                    "write_file",
                    {"file_path": "/workspace/async.txt", "content": "hi"},
                ),
                AIMessage(content="done"),
            ),
        )
        agent = create_deep_agent(model=model, backend=shared_sandbox)
        result = await ainvoke(agent)
        assert last_tool_message(result, "write_file").status == "success"
        assert "hi" in shared_sandbox.execute("cat /workspace/async.txt").output

    async def test_sync_and_async_prompt_assembly_agree(
        self,
        shared_sandbox: Any,
    ) -> None:
        shared_sandbox.upload_files([("/workspace/mem/sync.md", b"SHARED-MEMORY\n")])

        def build() -> tuple[Any, Any]:
            model = ScriptedModel(messages=script(AIMessage(content="done")))
            return model, create_deep_agent(
                model=model,
                backend=shared_sandbox,
                system_prompt="CALLER",
                memory=["/workspace/mem/sync.md"],
            )

        sync_model, sync_agent = build()
        invoke(sync_agent)
        async_model, async_agent = build()
        await ainvoke(async_agent)

        assert system_text(sync_model.system_messages[-1]) == system_text(
            async_model.system_messages[-1],
        )
