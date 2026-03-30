/**
 * DeepAgents + wasmsh — Node.js Example
 *
 * Creates an LLM agent backed by the wasmsh sandbox. The agent gets
 * shell execution, Python, and filesystem tools automatically.
 *
 * Prerequisites:
 *   npm install
 *   export ANTHROPIC_API_KEY=sk-ant-...
 *
 * Run:
 *   npx tsx example.ts
 */
import { createDeepAgent } from "deepagents";
import { HumanMessage } from "@langchain/core/messages";
import { WasmshSandbox } from "@langchain/wasmsh";

async function main() {
  // Create a sandboxed environment — shell, Python, and filesystem
  // all run inside wasmsh's virtual machine, no host OS access.
  const sandbox = await WasmshSandbox.createNode({
    workingDirectory: "/workspace",
  });

  console.log("Sandbox ready:", sandbox.id);

  // Seed some data for the agent to work with
  const csvData =
    "product,sales,region\n" +
    "Widget A,1200,North\n" +
    "Widget B,850,South\n" +
    "Widget C,2100,North\n" +
    "Widget D,670,East\n" +
    "Widget E,1500,South\n";

  await sandbox.uploadFiles([
    ["/workspace/sales.csv", new TextEncoder().encode(csvData)],
  ]);

  // Create a deep agent with the sandbox as backend.
  // The agent automatically gets: execute, read_file, write_file,
  // edit_file, ls, grep, glob tools.
  const agent = createDeepAgent({
    model: "claude-haiku-4-5-20251001",
    backend: sandbox,
  });

  console.log("\nAsking agent to analyze sales data...\n");

  // The agent decides how to accomplish the task — it may write a
  // Python script, use shell commands, or combine both.
  const result = await agent.invoke({
    messages: [
      new HumanMessage(
        "Analyze /workspace/sales.csv. Calculate total sales per region " +
          "and identify the best-selling product. Write a summary report " +
          "to /workspace/report.md.",
      ),
    ],
  });

  // Read the agent's output
  const report = await sandbox.execute("cat /workspace/report.md");
  console.log("=== Agent's Report ===");
  console.log(report.output);

  // Show what files the agent created
  const files = await sandbox.execute("find /workspace -type f");
  console.log("=== Files in sandbox ===");
  console.log(files.output);

  await sandbox.stop();
  console.log("Done.");
}

main().catch(console.error);
