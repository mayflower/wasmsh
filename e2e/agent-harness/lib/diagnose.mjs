/**
 * Failure diagnosis — asks the LLM to classify why a task failed,
 * using the full tool call trace for accurate root-cause analysis.
 */
import { ChatAnthropic } from "@langchain/anthropic";
import { formatToolTrace } from "./execute.mjs";

const SYSTEM_PROMPT = `You are diagnosing a test failure in wasmsh, a WASM shell sandbox.
The sandbox provides bash-like shell (88 utilities including awk, sed, grep, jq, find, tar, curl, etc.) and Python 3.13 via Pyodide.

An LLM agent was given a task. It used tools (execute, write_file, read_file, edit_file, ls, grep, glob) against the sandbox. The verification command did not output "PASS".

You will see the FULL tool call trace — every command the agent ran and every response the sandbox returned.

Classify the failure as exactly one of:
- sandbox_bug: A shell command or utility returned wrong output, threw an unexpected error, or is missing/unimplemented. Look for: "command not found", "not implemented", wrong output from a utility, Python import errors for stdlib modules, unexpected exit codes from correct commands.
- llm_mistake: The agent used wrong logic, wrong syntax, or misunderstood the task. The sandbox responded correctly to what the agent asked, but the agent asked the wrong thing.
- test_issue: The generated task or verification command was flawed (impossible to complete, wrong verification logic, seed files missing, contradictory requirements).
- timeout: The task exceeded time limits.

IMPORTANT: Focus on the tool outputs. If a command the agent ran returned an error message from wasmsh (e.g., "command not found", unexpected empty output from a valid command, segfault, or wrong computation from a utility), that's sandbox_bug even if the agent could have worked around it.

Respond as a single JSON object:
{"classification": "sandbox_bug|llm_mistake|test_issue|timeout", "reason": "1-2 sentence explanation of root cause", "failed_command": "the specific command that failed, or null", "wasmsh_component": "utility or feature name, or null", "suggested_fix": "what to fix in wasmsh, or null"}`;

export async function diagnoseFailure(task, result) {
  if (result.error === "timeout") {
    return {
      classification: "timeout",
      reason: "Task exceeded the 5-minute time limit",
      failed_command: null,
      wasmsh_component: null,
      suggested_fix: null,
    };
  }

  const model = new ChatAnthropic({
    model: "claude-haiku-4-5-20251001",
  });

  const traceText = formatToolTrace(result.toolTrace || []);

  const context = `TASK: ${task.description}

SEED FILES: ${JSON.stringify(task.seed_files || [])}

VERIFICATION COMMAND: ${task.verification}
VERIFICATION STDOUT: ${result.verification.stdout || "(empty)"}
VERIFICATION EXIT CODE: ${result.verification.exitCode}

FILES IN /workspace AFTER AGENT: ${result.filesAfter || "(empty)"}

FULL TOOL CALL TRACE (${(result.toolTrace || []).length} calls):
${traceText}

${result.error ? `RUNTIME ERROR: ${result.error}` : ""}`;

  const response = await model.invoke([
    { role: "system", content: SYSTEM_PROMPT },
    { role: "user", content: context },
  ]);

  const text = typeof response.content === "string"
    ? response.content
    : response.content.map((b) => b.text || "").join("");

  try {
    const jsonMatch = text.match(/\{[\s\S]*\}/);
    if (!jsonMatch) throw new Error("no JSON found");
    return JSON.parse(jsonMatch[0]);
  } catch {
    return {
      classification: "test_issue",
      reason: `Diagnosis parse error: ${text.slice(0, 200)}`,
      failed_command: null,
      wasmsh_component: null,
      suggested_fix: null,
    };
  }
}
