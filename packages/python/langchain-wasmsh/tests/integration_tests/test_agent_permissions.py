"""Filesystem permissions, enforced by upstream `FilesystemMiddleware`.

This adapter adds no permission system of its own; these tests prove wasmsh
composes with upstream's, and pin the shape of that composition — which is
narrower than it first looks.

`deepagents==0.7.4` refuses `permissions` outright when the backend can run
commands, unless every rule path sits under a `CompositeBackend` route. That
is deliberate on upstream's side: a rule guarding a path the agent can also
reach through `execute` would be advisory, not enforced. So permissions in a
wasmsh deployment protect the *routed* prefixes — profiles, memories,
skills, policies, all served by a non-executing `StoreBackend` — while the
wasmsh workspace stays an unguarded execution area.

Everything below therefore runs against the canonical composition, through
real file tools on a real graph, because permission rules are applied in the
middleware between the model and the backend.
"""

from __future__ import annotations

from typing import Any

import pytest
from deepagents import FilesystemPermission, create_deep_agent
from deepagents.backends.composite import CompositeBackend
from deepagents.backends.store import StoreBackend
from langchain_core.messages import AIMessage
from langgraph.checkpoint.memory import InMemorySaver
from langgraph.store.memory import InMemoryStore
from langgraph.types import Command

from tests.integration_tests._harness import (
    AgentContext,
    ScriptedModel,
    call,
    invoke,
    last_tool_message,
    requires_assets,
    script,
)

pytestmark = requires_assets

POLICY_READ_ONLY = FilesystemPermission(
    operations=["write"],
    paths=["/policies/**"],
    mode="deny",
)
SECRETS_UNREADABLE = FilesystemPermission(
    operations=["read"],
    paths=["/secrets/**"],
    mode="deny",
)
REVIEWED_WRITES = FilesystemPermission(
    operations=["write"],
    paths=["/reviewed/**"],
    mode="interrupt",
)


@pytest.fixture
def store() -> InMemoryStore:
    return InMemoryStore()


def routed(sandbox: Any, store: InMemoryStore) -> CompositeBackend:
    namespace = ("deepagents", "perms")
    return CompositeBackend(
        default=sandbox,
        routes={
            "/policies/": StoreBackend(store=store, namespace=lambda _rt: namespace),
            "/secrets/": StoreBackend(store=store, namespace=lambda _rt: namespace),
            "/reviewed/": StoreBackend(store=store, namespace=lambda _rt: namespace),
        },
    )


def build(
    sandbox: Any,
    store: InMemoryStore,
    permissions: list[FilesystemPermission],
    *messages: Any,
    checkpointer: Any = None,
) -> tuple[ScriptedModel, Any]:
    model = ScriptedModel(messages=script(*messages))
    agent = create_deep_agent(
        model=model,
        backend=routed(sandbox, store),
        context_schema=AgentContext,
        store=store,
        permissions=permissions,
        checkpointer=checkpointer,
    )
    return model, agent


def seed(sandbox: Any, store: InMemoryStore, path: str, content: str) -> None:
    """Write a routed file with no permissions in force."""
    _, writer = build(
        sandbox,
        store,
        [],
        call("write_file", {"file_path": path, "content": content}),
        AIMessage(content="ok"),
    )
    assert last_tool_message(invoke(writer), "write_file").status == "success"


class TestExecutableBackendGuard:
    def test_permissions_on_a_bare_wasmsh_backend_are_refused(
        self,
        shared_sandbox: Any,
    ) -> None:
        # The documented limitation, asserted rather than described: a rule
        # over a path the agent can also reach through `execute` would be a
        # false promise, so upstream refuses to build the graph at all.
        with pytest.raises(NotImplementedError, match="command execution"):
            create_deep_agent(
                model=ScriptedModel(messages=script(AIMessage(content="x"))),
                backend=shared_sandbox,
                permissions=[POLICY_READ_ONLY],
            )

    def test_a_rule_outside_every_route_is_refused(
        self,
        shared_sandbox: Any,
        store: InMemoryStore,
    ) -> None:
        with pytest.raises(NotImplementedError, match="command execution"):
            create_deep_agent(
                model=ScriptedModel(messages=script(AIMessage(content="x"))),
                backend=routed(shared_sandbox, store),
                context_schema=AgentContext,
                permissions=[
                    FilesystemPermission(
                        operations=["write"],
                        paths=["/workspace/**"],
                        mode="deny",
                    ),
                ],
            )


class TestWritePermissions:
    def test_a_denied_write_is_refused_and_changes_nothing(
        self,
        shared_sandbox: Any,
        store: InMemoryStore,
    ) -> None:
        seed(shared_sandbox, store, "/policies/rules.md", "do not edit me")
        _, agent = build(
            shared_sandbox,
            store,
            [POLICY_READ_ONLY],
            call(
                "write_file",
                {"file_path": "/policies/rules.md", "content": "hacked"},
            ),
            call("read_file", {"file_path": "/policies/rules.md"}, "c2"),
            AIMessage(content="done"),
        )
        result = invoke(agent)
        assert last_tool_message(result, "write_file").status == "error"
        assert "do not edit me" in last_tool_message(result, "read_file").content

    def test_an_allowed_write_elsewhere_still_works(
        self,
        shared_sandbox: Any,
        store: InMemoryStore,
    ) -> None:
        _, agent = build(
            shared_sandbox,
            store,
            [POLICY_READ_ONLY],
            call(
                "write_file",
                {"file_path": "/workspace/notes/free.md", "content": "fine"},
            ),
            AIMessage(content="done"),
        )
        assert last_tool_message(invoke(agent), "write_file").status == "success"

    def test_a_denied_edit_is_refused(
        self,
        shared_sandbox: Any,
        store: InMemoryStore,
    ) -> None:
        seed(shared_sandbox, store, "/policies/edit.md", "do not edit me")
        _, agent = build(
            shared_sandbox,
            store,
            [POLICY_READ_ONLY],
            call(
                "edit_file",
                {
                    "file_path": "/policies/edit.md",
                    "old_string": "do not edit me",
                    "new_string": "edited",
                },
            ),
            AIMessage(content="done"),
        )
        assert last_tool_message(invoke(agent), "edit_file").status == "error"

    def test_reading_a_write_denied_path_is_still_allowed(
        self,
        shared_sandbox: Any,
        store: InMemoryStore,
    ) -> None:
        seed(shared_sandbox, store, "/policies/readable.md", "policy text")
        _, agent = build(
            shared_sandbox,
            store,
            [POLICY_READ_ONLY],
            call("read_file", {"file_path": "/policies/readable.md"}),
            AIMessage(content="done"),
        )
        message = last_tool_message(invoke(agent), "read_file")
        assert message.status == "success"
        assert "policy text" in message.content


class TestReadPermissions:
    def test_a_denied_read_returns_no_content(
        self,
        shared_sandbox: Any,
        store: InMemoryStore,
    ) -> None:
        seed(shared_sandbox, store, "/secrets/keys.txt", "TOP-SECRET")
        _, agent = build(
            shared_sandbox,
            store,
            [SECRETS_UNREADABLE],
            call("read_file", {"file_path": "/secrets/keys.txt"}),
            AIMessage(content="done"),
        )
        message = last_tool_message(invoke(agent), "read_file")
        assert message.status == "error"
        assert "TOP-SECRET" not in message.content


class TestDeleteClassification:
    def test_delete_is_governed_by_write_rules(
        self,
        shared_sandbox: Any,
        store: InMemoryStore,
    ) -> None:
        # `delete` is destructive, so a write-deny must cover it; a rule
        # naming only "write" that let deletes through would be a trap.
        seed(shared_sandbox, store, "/policies/keep.md", "keep me")
        _, agent = build(
            shared_sandbox,
            store,
            [POLICY_READ_ONLY],
            call("delete", {"file_path": "/policies/keep.md"}),
            call("read_file", {"file_path": "/policies/keep.md"}, "c2"),
            AIMessage(content="done"),
        )
        result = invoke(agent)
        assert last_tool_message(result, "delete").status == "error"
        assert "keep me" in last_tool_message(result, "read_file").content

    def test_deleting_a_parent_of_a_protected_path_is_refused(
        self,
        shared_sandbox: Any,
        store: InMemoryStore,
    ) -> None:
        # A recursive delete of the parent would take the protected subtree
        # with it, so the rule is intersected conservatively.
        seed(shared_sandbox, store, "/policies/nested/deep.md", "protected")
        _, agent = build(
            shared_sandbox,
            store,
            [
                FilesystemPermission(
                    operations=["write"],
                    paths=["/policies/nested/**"],
                    mode="deny",
                ),
            ],
            call("delete", {"file_path": "/policies"}),
            call("read_file", {"file_path": "/policies/nested/deep.md"}, "c2"),
            AIMessage(content="done"),
        )
        result = invoke(agent)
        assert last_tool_message(result, "delete").status == "error"
        assert "protected" in last_tool_message(result, "read_file").content


class TestInterruptMode:
    def test_an_interrupt_rule_pauses_then_resumes_the_write(
        self,
        shared_sandbox: Any,
        store: InMemoryStore,
    ) -> None:
        _, agent = build(
            shared_sandbox,
            store,
            [REVIEWED_WRITES],
            call(
                "write_file",
                {"file_path": "/reviewed/a.md", "content": "needs review"},
            ),
            AIMessage(content="done"),
            checkpointer=InMemorySaver(),
        )
        config = {"configurable": {"thread_id": "perm-hitl"}}
        paused = invoke(agent, config=config)
        assert paused.get("__interrupt__"), "expected the write to pause"

        resumed = agent.invoke(
            Command(resume={"decisions": [{"type": "approve"}]}),
            config=config,
        )
        assert last_tool_message(resumed, "write_file").status == "success"


class TestDocumentedLimits:
    def test_permissions_do_not_constrain_execute(
        self,
        shared_sandbox: Any,
        store: InMemoryStore,
    ) -> None:
        # The reason upstream scopes permissions to non-executing routes:
        # anything with a shell reaches the workspace regardless. Durable,
        # policy-bearing data belongs in a store namespace, not the VFS.
        _, agent = build(
            shared_sandbox,
            store,
            [POLICY_READ_ONLY],
            call(
                "execute",
                {"command": "mkdir -p /workspace/x && echo bypassed > /workspace/x/f"},
            ),
            AIMessage(content="done"),
        )
        result = invoke(agent)
        assert last_tool_message(result, "execute").status == "success"
        assert "bypassed" in shared_sandbox.execute("cat /workspace/x/f").output
