"""
DeepAgents + wasmsh — Python Example

Creates an LLM agent backed by the wasmsh sandbox. The agent gets
shell execution, Python, and filesystem tools automatically.

Prerequisites:
    pip install -r requirements.txt
    export ANTHROPIC_API_KEY=sk-ant-...

Run:
    python example.py
"""

from langchain_core.messages import HumanMessage
from langchain_wasmsh import WasmshSandbox


def main() -> None:
    # Create a sandboxed environment — shell, Python, and filesystem
    # all run inside wasmsh's virtual machine, no host OS access.
    sandbox = WasmshSandbox()
    print(f"Sandbox ready: {sandbox.id}")

    # Seed some data for the agent to work with
    csv_data = (
        "product,sales,region\n"
        "Widget A,1200,North\n"
        "Widget B,850,South\n"
        "Widget C,2100,North\n"
        "Widget D,670,East\n"
        "Widget E,1500,South\n"
    )
    sandbox.upload_files([("/workspace/sales.csv", csv_data.encode())])

    # Create a deep agent with the sandbox as backend.
    # The agent automatically gets: execute, read_file, write_file,
    # edit_file, ls, grep, glob tools.
    from deepagents.graph import create_deep_agent

    agent = create_deep_agent(
        model="claude-haiku-4-5-20251001",
        backend=sandbox,
    )

    print("\nAsking agent to analyze sales data...\n")

    # The agent decides how to accomplish the task — it may write a
    # Python script, use shell commands, or combine both.
    agent.invoke(
        {
            "messages": [
                HumanMessage(
                    content=(
                        "Analyze /workspace/sales.csv. Calculate total sales per region "
                        "and identify the best-selling product. Write a summary report "
                        "to /workspace/report.md."
                    )
                )
            ]
        }
    )

    # Read the agent's output
    report = sandbox.execute("cat /workspace/report.md")
    print("=== Agent's Report ===")
    print(report.output)

    # Show what files the agent created
    files = sandbox.execute("find /workspace -type f")
    print("=== Files in sandbox ===")
    print(files.output)

    sandbox.close()
    print("Done.")


if __name__ == "__main__":
    main()
