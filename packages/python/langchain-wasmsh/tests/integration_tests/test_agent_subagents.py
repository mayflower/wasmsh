"""The exact 0.7.4 subagent inheritance contract, with wasmsh active.

Subagents are easy to reason about wrongly: it is tempting to assume a
subagent is a smaller copy of its parent and inherits whatever the parent
had. It does not. Each stack gets a specific, different set of things, and
the differences are where capability leaks and surprise losses live — a
subagent that silently sees the parent's memory, or one that silently loses
the permission rules protecting a store route.

So every row is asserted rather than inferred:

- **backend**: shared by the main agent and declarative subagents.
- **tools**: a declarative subagent inherits the parent's tools only when it
  omits `tools`.
- **permissions**: inherited when omitted, replaced wholesale when given.
- **memory**: not inherited by any subagent — `SubAgent` has no `memory`
  field in 0.7.4.
- **compiled**: used exactly as supplied; nothing ambient reaches it.
- **private state**: REPL snapshots and upstream's private memory/skill
  fields never cross into a subagent or back out.
"""

from __future__ import annotations

from typing import Any

import pytest
from deepagents import CompiledSubAgent, FilesystemPermission, create_deep_agent
from deepagents.backends.composite import CompositeBackend
from deepagents.backends.state import StateBackend
from deepagents.backends.store import StoreBackend
from langchain_core.messages import AIMessage
from langchain_core.tools import tool
from langgraph.store.memory import InMemoryStore
from pydantic import BaseModel

from langchain_wasmsh import WasmshInterpreterMiddleware
from tests.integration_tests._harness import (
    AgentContext,
    ScriptedModel,
    call,
    invoke,
    last_tool_message,
    requires_assets,
    script,
    system_text,
)

pytestmark = requires_assets


def task_call(subagent_type: str, description: str = "do it") -> AIMessage:
    return call("task", {"subagent_type": subagent_type, "description": description})


def subagent(name: str, model: ScriptedModel, **extra: Any) -> dict[str, Any]:
    return {
        "name": name,
        "description": f"{name} subagent",
        "system_prompt": "You are a subagent.",
        "model": model,
        **extra,
    }


@pytest.fixture
def store() -> InMemoryStore:
    return InMemoryStore()


def routed(sandbox: Any, store: InMemoryStore) -> CompositeBackend:
    namespace = ("deepagents", "subagents")
    return CompositeBackend(
        default=sandbox,
        routes={
            "/policies/": StoreBackend(store=store, namespace=lambda _rt: namespace),
            "/memories/": StoreBackend(store=store, namespace=lambda _rt: namespace),
        },
    )


def stored_paths(store: InMemoryStore) -> set[str]:
    """Every key currently held in the routed store namespace.

    A subagent's tool messages stay inside its own run, so the parent's
    message list cannot show whether a nested write succeeded. The store is
    the observable that outlives the nested graph. `CompositeBackend` strips
    the route prefix before handing the path to `StoreBackend`, so keys here
    are route-relative (`/p.md`, not `/policies/p.md`).
    """
    return {item.key for item in store.search(("deepagents", "subagents"), limit=1000)}


class TestBackendSharing:
    def test_a_declarative_subagent_writes_to_the_parents_sandbox(
        self,
        shared_sandbox: Any,
    ) -> None:
        sub_model = ScriptedModel(
            messages=script(
                call(
                    "write_file",
                    {"file_path": "/workspace/from_sub.txt", "content": "sub wrote"},
                ),
                AIMessage(content="sub done"),
            ),
        )
        parent = ScriptedModel(
            messages=script(task_call("writer"), AIMessage(content="done")),
        )
        agent = create_deep_agent(
            model=parent,
            backend=shared_sandbox,
            subagents=[subagent("writer", sub_model)],
        )
        invoke(agent)
        assert (
            "sub wrote" in shared_sandbox.execute("cat /workspace/from_sub.txt").output
        )


class TestToolInheritance:
    def test_omitting_tools_inherits_the_parents(self, shared_sandbox: Any) -> None:
        sub_model = ScriptedModel(messages=script(AIMessage(content="sub done")))
        parent = ScriptedModel(
            messages=script(task_call("inheritor"), AIMessage(content="done")),
        )
        invoke(
            create_deep_agent(
                model=parent,
                backend=shared_sandbox,
                subagents=[subagent("inheritor", sub_model)],
            ),
        )
        assert "write_file" in sub_model.tool_names
        assert "execute" in sub_model.tool_names

    def test_explicit_tools_replace_the_parents_ordinary_tools(
        self,
        shared_sandbox: Any,
    ) -> None:
        @tool
        def parent_tool() -> str:
            """A tool only the parent has."""
            return "parent"

        @tool
        def only_tool() -> str:
            """The subagent's own tool."""
            return "ok"

        sub_model = ScriptedModel(messages=script(AIMessage(content="sub done")))
        parent = ScriptedModel(
            messages=script(task_call("narrow"), AIMessage(content="done")),
        )
        invoke(
            create_deep_agent(
                model=parent,
                backend=shared_sandbox,
                tools=[parent_tool],
                subagents=[subagent("narrow", sub_model, tools=[only_tool])],
            ),
        )
        assert "only_tool" in sub_model.tool_names
        assert "parent_tool" not in sub_model.tool_names
        # `tools` replaces the parent's *ordinary* tools only. The file and
        # shell tools come from `FilesystemMiddleware`, which is scaffolding
        # on every stack, so a narrow `tools` list does not take them away.
        assert "execute" in sub_model.tool_names


class TestPermissionInheritance:
    DENY_POLICIES = FilesystemPermission(
        operations=["write"],
        paths=["/policies/**"],
        mode="deny",
    )

    def test_omitting_permissions_inherits_the_parents(
        self,
        shared_sandbox: Any,
        store: InMemoryStore,
    ) -> None:
        sub_model = ScriptedModel(
            messages=script(
                call(
                    "write_file",
                    {"file_path": "/policies/p.md", "content": "sub tried"},
                ),
                AIMessage(content="sub done"),
            ),
        )
        parent = ScriptedModel(
            messages=script(task_call("inheritor"), AIMessage(content="done")),
        )
        invoke(
            create_deep_agent(
                model=parent,
                backend=routed(shared_sandbox, store),
                context_schema=AgentContext,
                store=store,
                permissions=[self.DENY_POLICIES],
                subagents=[subagent("inheritor", sub_model)],
            ),
            context=AgentContext(),
        )
        # The parent's deny rule reached the subagent stack: nothing landed.
        assert "/p.md" not in stored_paths(store)

    def test_explicit_permissions_replace_the_parents(
        self,
        shared_sandbox: Any,
        store: InMemoryStore,
    ) -> None:
        sub_model = ScriptedModel(
            messages=script(
                call(
                    "write_file",
                    {"file_path": "/policies/allowed.md", "content": "sub wrote"},
                ),
                AIMessage(content="sub done"),
            ),
        )
        parent = ScriptedModel(
            messages=script(task_call("free"), AIMessage(content="done")),
        )
        invoke(
            create_deep_agent(
                model=parent,
                backend=routed(shared_sandbox, store),
                context_schema=AgentContext,
                store=store,
                permissions=[self.DENY_POLICIES],
                subagents=[
                    subagent(
                        "free",
                        sub_model,
                        # A different rule entirely: replacement, not merge,
                        # so /policies/ is no longer protected for this stack.
                        permissions=[
                            FilesystemPermission(
                                operations=["write"],
                                paths=["/memories/**"],
                                mode="deny",
                            ),
                        ],
                    ),
                ],
            ),
            context=AgentContext(),
        )
        assert "/allowed.md" in stored_paths(store)


class TestMemoryIsNotInherited:
    def test_top_level_memory_does_not_reach_a_declarative_subagent(
        self,
        shared_sandbox: Any,
    ) -> None:
        # `SubAgent` has no `memory` field in 0.7.4. A subagent that wants
        # memory must own a `MemoryMiddleware` or be a compiled graph that
        # does; nothing is inherited implicitly.
        shared_sandbox.upload_files(
            [("/workspace/mem/parent.md", b"PARENT-ONLY-MEMORY\n")],
        )
        sub_model = ScriptedModel(messages=script(AIMessage(content="sub done")))
        parent = ScriptedModel(
            messages=script(task_call("plain"), AIMessage(content="done")),
        )
        invoke(
            create_deep_agent(
                model=parent,
                backend=shared_sandbox,
                memory=["/workspace/mem/parent.md"],
                subagents=[subagent("plain", sub_model)],
            ),
        )
        assert "PARENT-ONLY-MEMORY" in parent.system_text
        assert "PARENT-ONLY-MEMORY" not in sub_model.system_text


class TestPrivateStateIsolation:
    def test_the_interpreter_and_its_snapshot_stay_in_the_parent_stack(
        self,
        shared_sandbox: Any,
    ) -> None:
        sub_model = ScriptedModel(messages=script(AIMessage(content="sub done")))
        parent = ScriptedModel(
            messages=script(
                call("py_eval", {"code": "marker = 1"}, "c1"),
                task_call("plain"),
                AIMessage(content="done"),
            ),
        )
        result = invoke(
            create_deep_agent(
                model=parent,
                backend=shared_sandbox,
                middleware=[
                    WasmshInterpreterMiddleware(sandbox_factory=lambda: shared_sandbox),
                ],
                subagents=[subagent("plain", sub_model)],
            ),
        )
        # Top-level custom middleware is not inherited by a declarative
        # subagent, and the private snapshot is execution state for one
        # thread rather than something that travels as agent state.
        assert "py_eval" not in sub_model.tool_names
        assert "_wasmsh_snapshot_payload" not in result


class TestCompiledSubagent:
    def test_a_compiled_subagent_is_used_exactly_as_supplied(
        self,
        shared_sandbox: Any,
    ) -> None:
        inner_model = ScriptedModel(messages=script(AIMessage(content="compiled ran")))
        # Its own graph and its own backend: no wasmsh, no parent
        # permissions, no parent memory. Nothing ambient reaches it.
        inner = create_deep_agent(model=inner_model, backend=StateBackend())
        parent = ScriptedModel(
            messages=script(task_call("compiled"), AIMessage(content="done")),
        )
        result = invoke(
            create_deep_agent(
                model=parent,
                backend=shared_sandbox,
                subagents=[
                    CompiledSubAgent(
                        name="compiled",
                        description="a pre-compiled graph",
                        runnable=inner,
                    ),
                ],
            ),
        )
        assert "compiled ran" in last_tool_message(result, "task").content
        assert "execute" not in inner_model.tool_names


class TestTaskTool:
    def test_task_is_present_by_default(self, shared_sandbox: Any) -> None:
        model = ScriptedModel(messages=script(AIMessage(content="done")))
        invoke(create_deep_agent(model=model, backend=shared_sandbox))
        assert "task" in model.tool_names

    async def test_async_task_invocation_works(self, shared_sandbox: Any) -> None:
        sub_model = ScriptedModel(messages=script(AIMessage(content="async sub")))
        parent = ScriptedModel(
            messages=script(task_call("worker"), AIMessage(content="done")),
        )
        agent = create_deep_agent(
            model=parent,
            backend=shared_sandbox,
            subagents=[subagent("worker", sub_model)],
        )
        result = await agent.ainvoke({"messages": [{"role": "user", "content": "go"}]})
        assert "async sub" in last_tool_message(result, "task").content

    def test_an_unknown_subagent_type_is_reported_not_raised(
        self,
        shared_sandbox: Any,
    ) -> None:
        parent = ScriptedModel(
            messages=script(task_call("does-not-exist"), AIMessage(content="done")),
        )
        result = invoke(create_deep_agent(model=parent, backend=shared_sandbox))
        assert "cannot invoke subagent" in last_tool_message(result, "task").content

    def test_a_structured_subagent_result_reaches_the_parent(
        self,
        shared_sandbox: Any,
    ) -> None:
        class Finding(BaseModel):
            """Structured subagent answer."""

            verdict: str

        sub_model = ScriptedModel(
            messages=script(call("Finding", {"verdict": "clear"}, "s1")),
        )
        parent = ScriptedModel(
            messages=script(task_call("judge"), AIMessage(content="done")),
        )
        result = invoke(
            create_deep_agent(
                model=parent,
                backend=shared_sandbox,
                subagents=[
                    subagent("judge", sub_model, response_format=Finding),
                ],
            ),
        )
        assert "clear" in last_tool_message(result, "task").content


class TestSubagentPromptIsolation:
    def test_a_subagent_gets_its_own_system_prompt(
        self,
        shared_sandbox: Any,
    ) -> None:
        sub_model = ScriptedModel(messages=script(AIMessage(content="sub done")))
        parent = ScriptedModel(
            messages=script(task_call("specialist"), AIMessage(content="done")),
        )
        invoke(
            create_deep_agent(
                model=parent,
                backend=shared_sandbox,
                system_prompt="PARENT-PROMPT",
                subagents=[
                    {
                        "name": "specialist",
                        "description": "specialist",
                        "system_prompt": "SUBAGENT-PROMPT",
                        "model": sub_model,
                    },
                ],
            ),
        )
        sub_text = system_text(sub_model.system_messages[-1])
        assert "SUBAGENT-PROMPT" in sub_text
        assert "PARENT-PROMPT" not in sub_text
