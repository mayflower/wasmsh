"""One graph that uses everything at once, then proves what persisted.

The other modules each isolate one interface. This one is the opposite: a
single `create_deep_agent` call wiring wasmsh as the executable backend,
routed `StoreBackend` namespaces for profile / memory / skills / policy, a
checkpointer, a store, a custom runtime context, layered skill sources, a
read-only policy rule, the wasmsh interpreter, a runtime-aware tool, a
subagent, and a structured final response — driven through one turn that
touches all of them.

Composition is where these features stop being independent. A prompt-block
regression only shows up once memory, skills, and the interpreter all
contribute; a routing mistake only shows up once several `StoreBackend`
prefixes share one `CompositeBackend`; a scoping mistake only shows up when
a second user runs the same graph.

The second half is the part that matters most: a fresh thread with a fresh
sandbox process must still see the persisted profile and memory, and a
different user must see none of it.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

import pytest
from deepagents import FilesystemPermission, create_deep_agent
from deepagents.backends.composite import CompositeBackend
from deepagents.backends.store import StoreBackend
from langchain.tools import (
    ToolRuntime,  # noqa: TC002 -- needed at runtime for ToolNode's injection scan
)
from langchain_core.messages import AIMessage
from langchain_core.tools import tool
from langgraph.checkpoint.memory import InMemorySaver
from langgraph.store.base import (
    BaseStore,  # noqa: TC002 -- runtime annotation on helper signatures
)
from langgraph.store.memory import InMemoryStore
from pydantic import BaseModel, Field

from langchain_wasmsh import WasmshInterpreterMiddleware, WasmshSandbox
from tests.integration_tests._harness import (
    ScriptedModel,
    call,
    invoke,
    last_tool_message,
    requires_assets,
    script,
)

pytestmark = requires_assets


@dataclass
class Identity:
    """Runtime identity that drives every routed namespace."""

    user_id: str
    assistant_id: str
    org_id: str


class Answer(BaseModel):
    """The structured result the graph must produce."""

    verdict: str = Field(description="One-word verdict")
    evidence: str = Field(description="What the run established")


@tool
def whoami(runtime: ToolRuntime) -> str:
    """Report the caller identity the graph is running under."""
    return (
        f"{runtime.context.user_id}/"
        f"{runtime.context.assistant_id}/"
        f"{runtime.context.org_id}"
    )


def routed(sandbox: WasmshSandbox, store: BaseStore) -> CompositeBackend:
    """The canonical split: wasmsh executes, the store remembers."""
    return CompositeBackend(
        default=sandbox,
        routes={
            "/profiles/": StoreBackend(
                store=store,
                namespace=lambda rt: ("golden", "profile", rt.context.user_id),
            ),
            "/memories/user/": StoreBackend(
                store=store,
                namespace=lambda rt: ("golden", "memory-user", rt.context.user_id),
            ),
            "/skills/agent/": StoreBackend(
                store=store,
                namespace=lambda rt: (
                    "golden",
                    "skills-agent",
                    rt.context.assistant_id,
                ),
            ),
            "/skills/user/": StoreBackend(
                store=store,
                namespace=lambda rt: ("golden", "skills-user", rt.context.user_id),
            ),
            "/policies/": StoreBackend(
                store=store,
                namespace=lambda rt: ("golden", "policy", rt.context.org_id),
            ),
        },
    )


def build_agent(
    sandbox: WasmshSandbox,
    store: BaseStore,
    *,
    model: ScriptedModel,
    checkpointer: Any = None,
    subagent_model: ScriptedModel | None = None,
) -> Any:
    """The one graph under test, built the same way every time."""
    return create_deep_agent(
        model=model,
        backend=routed(sandbox, store),
        context_schema=Identity,
        checkpointer=checkpointer,
        store=store,
        tools=[whoami],
        memory=["/profiles/user.md", "/memories/user/AGENTS.md", "/policies/AGENTS.md"],
        # Later sources override earlier same-named skills.
        skills=["/skills/agent/", "/skills/user/"],
        permissions=[
            FilesystemPermission(
                operations=["write"],
                paths=["/policies/**", "/skills/agent/**"],
                mode="deny",
            ),
        ],
        middleware=[
            WasmshInterpreterMiddleware(
                # Its own session: the interpreter closes the sandbox it owns
                # when the turn ends, which must not take the backend with it.
                sandbox_factory=WasmshSandbox,
                mode="thread",
            ),
        ],
        subagents=[
            {
                "name": "auditor",
                "description": "Checks a claim and reports back",
                "system_prompt": "You audit claims.",
                "model": subagent_model
                or ScriptedModel(messages=script(AIMessage(content="audit clean"))),
            },
        ],
        response_format=Answer,
    )


SKILL_AGENT = """\
---
name: audit
description: AGENT-SCOPED-AUDIT-SKILL
---

# audit

Steps for auditing.
"""

SKILL_USER = """\
---
name: audit
description: USER-SCOPED-AUDIT-SKILL
---

# audit

The user's own audit steps.
"""


def seed_store(sandbox: WasmshSandbox, store: BaseStore, identity: Identity) -> None:
    """Populate the routed namespaces through the graph's own file tools."""
    writes = {
        "/profiles/user.md": "PROFILE-PREFERS-BRIEF-ANSWERS",
        "/memories/user/AGENTS.md": "USER-MEMORY-V1",
        "/policies/AGENTS.md": "ORG-POLICY-NO-PII",
        "/skills/agent/audit/SKILL.md": SKILL_AGENT,
        "/skills/user/audit/SKILL.md": SKILL_USER,
    }
    model = ScriptedModel(
        messages=script(
            *[
                call("write_file", {"file_path": path, "content": body}, f"seed{i}")
                for i, (path, body) in enumerate(writes.items())
            ],
            AIMessage(content="seeded"),
        ),
    )
    # Seeded without permissions in force: the read-only policy protects the
    # agent from itself at run time, not the operator at provisioning time.
    seeder = create_deep_agent(
        model=model,
        backend=routed(sandbox, store),
        context_schema=Identity,
        store=store,
    )
    result = invoke(seeder, context=identity)
    for message in result["messages"]:
        if getattr(message, "name", None) == "write_file":
            assert message.status == "success", message.content


@pytest.fixture
def store() -> InMemoryStore:
    return InMemoryStore()


@pytest.fixture
def identity() -> Identity:
    return Identity(user_id="alice", assistant_id="assistant-1", org_id="acme")


class TestFullComposition:
    def test_one_turn_uses_every_wired_feature(
        self,
        store: InMemoryStore,
        identity: Identity,
    ) -> None:
        sandbox = WasmshSandbox()
        try:
            seed_store(sandbox, store, identity)

            model = ScriptedModel(
                messages=script(
                    # Read the profile that memory already loaded, proving the
                    # routed store is readable through the file tools too.
                    call("read_file", {"file_path": "/profiles/user.md"}, "c1"),
                    # Select a skill by reading its instructions.
                    call(
                        "read_file",
                        {"file_path": "/skills/user/audit/SKILL.md", "limit": 1000},
                        "c2",
                    ),
                    # Execute Python in the wasmsh interpreter.
                    call("py_eval", {"code": "print(6 * 7)"}, "c3"),
                    # Call the runtime-aware tool.
                    call("whoami", {}, "c4"),
                    # Delegate to the subagent.
                    call(
                        "task",
                        {"subagent_type": "auditor", "description": "check it"},
                        "c5",
                    ),
                    # Refused: the policy route is write-denied.
                    call(
                        "write_file",
                        {
                            "file_path": "/policies/AGENTS.md",
                            "content": "OVERWRITTEN",
                        },
                        "c6",
                    ),
                    # Update durable user memory.
                    call(
                        "write_file",
                        {
                            "file_path": "/memories/user/AGENTS.md",
                            "content": "USER-MEMORY-V2",
                        },
                        "c7",
                    ),
                    # Finish with the structured response.
                    call(
                        "Answer",
                        {"verdict": "clear", "evidence": "all steps ran"},
                        "c8",
                    ),
                ),
            )
            agent = build_agent(
                sandbox,
                store,
                model=model,
                checkpointer=InMemorySaver(),
            )
            result = invoke(
                agent,
                context=identity,
                config={"configurable": {"thread_id": "golden"}},
            )

            prompt = model.system_text
            # Memory: all three sources, once each.
            assert prompt.count("PROFILE-PREFERS-BRIEF-ANSWERS") == 1
            assert prompt.count("USER-MEMORY-V1") == 1
            assert prompt.count("ORG-POLICY-NO-PII") == 1
            # Skills: the later source wins the name collision.
            assert "USER-SCOPED-AUDIT-SKILL" in prompt
            assert "AGENT-SCOPED-AUDIT-SKILL" not in prompt
            # Interpreter: exactly one prompt fragment, not one per turn.
            assert prompt.count("### Interpreter") == 1

            reads = [
                m.content
                for m in result["messages"]
                if getattr(m, "name", None) == "read_file"
            ]
            assert any("PROFILE-PREFERS-BRIEF-ANSWERS" in c for c in reads)
            assert any("The user's own audit steps" in c for c in reads)

            assert "42" in last_tool_message(result, "py_eval").content
            assert (
                last_tool_message(result, "whoami").content == "alice/assistant-1/acme"
            )
            assert "audit clean" in last_tool_message(result, "task").content

            writes = [
                m
                for m in result["messages"]
                if getattr(m, "name", None) == "write_file"
            ]
            policy_write, memory_write = writes[0], writes[1]
            assert policy_write.status == "error"
            assert memory_write.status == "success"

            assert result["structured_response"] == Answer(
                verdict="clear",
                evidence="all steps ran",
            )
        finally:
            sandbox.close()

    def test_a_fresh_thread_and_sandbox_still_see_the_persisted_memory(
        self,
        store: InMemoryStore,
        identity: Identity,
    ) -> None:
        first = WasmshSandbox()
        try:
            seed_store(first, store, identity)
            model = ScriptedModel(
                messages=script(
                    call(
                        "write_file",
                        {
                            "file_path": "/memories/user/AGENTS.md",
                            "content": "USER-MEMORY-V2",
                        },
                    ),
                    call("Answer", {"verdict": "ok", "evidence": "saved"}, "c2"),
                ),
            )
            invoke(
                build_agent(first, store, model=model, checkpointer=InMemorySaver()),
                context=identity,
                config={"configurable": {"thread_id": "t1"}},
            )
        finally:
            first.close()

        # Everything the first sandbox held in its VFS is gone. Only what the
        # store holds can survive — which is the whole point of the split.
        second = WasmshSandbox()
        try:
            reader = ScriptedModel(
                messages=script(
                    call("Answer", {"verdict": "ok", "evidence": "loaded"}, "c1"),
                ),
            )
            invoke(
                build_agent(second, store, model=reader),
                context=identity,
                config={"configurable": {"thread_id": "t2"}},
            )
            assert "PROFILE-PREFERS-BRIEF-ANSWERS" in reader.system_text
            assert "USER-MEMORY-V2" in reader.system_text
            assert "USER-MEMORY-V1" not in reader.system_text
        finally:
            second.close()

    def test_another_user_sees_none_of_it(
        self,
        store: InMemoryStore,
        identity: Identity,
    ) -> None:
        sandbox = WasmshSandbox()
        try:
            seed_store(sandbox, store, identity)

            intruder = ScriptedModel(
                messages=script(
                    call("whoami", {}, "c1"),
                    call("Answer", {"verdict": "ok", "evidence": "isolated"}, "c2"),
                ),
            )
            other = Identity(
                user_id="mallory",
                assistant_id="assistant-1",
                org_id="other-corp",
            )
            result = invoke(
                build_agent(sandbox, store, model=intruder),
                context=other,
            )
            prompt = intruder.system_text
            assert "PROFILE-PREFERS-BRIEF-ANSWERS" not in prompt
            assert "USER-MEMORY-V1" not in prompt
            # Different organization: the policy is not theirs either.
            assert "ORG-POLICY-NO-PII" not in prompt
            assert (
                last_tool_message(result, "whoami").content
                == "mallory/assistant-1/other-corp"
            )
        finally:
            sandbox.close()
