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
- sandbox_bug: The sandbox didn't behave like a real bash+Python environment would. This includes: "command not found" for standard commands, syntax not parsed that bash would accept (process substitution, arrays, etc.), utilities producing wrong output, Python stdlib modules failing to import, unexpected exit codes from correct commands, shell features not working. If a verification command fails because wasmsh can't parse valid bash syntax, that's sandbox_bug. If a utility exists but gives wrong results, that's sandbox_bug.
- llm_mistake: The sandbox behaved exactly like real bash would, but the agent wrote incorrect logic. The agent's commands would also fail on a real Linux system.
- test_issue: ONLY use this if the task is literally impossible (contradictory requirements, expects files that don't exist and weren't seeded). Do NOT use this just because the verification uses advanced bash features — if wasmsh can't run valid bash, that's sandbox_bug.
- timeout: The task exceeded time limits.

IMPORTANT: Bias toward sandbox_bug. The whole point is to find gaps in wasmsh. If there's any doubt whether the sandbox or the agent is at fault, classify as sandbox_bug. Only use llm_mistake if you're certain the same commands would fail on a real Linux bash shell.

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
