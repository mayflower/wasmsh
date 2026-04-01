/**
 * Task generation — asks the LLM to invent random sandbox tasks.
 */
import { ChatAnthropic } from "@langchain/anthropic";

const CATEGORIES = [
  "file-ops",
  "text-processing",
  "python",
  "shell-scripting",
  "data-pipeline",
  "archive",
  "system",
  "encoding",
  "shell-advanced",
  "combined",
];

const SYSTEM_PROMPT = `You are a test generator for a WASM shell sandbox called wasmsh.
It provides a bash-like shell with 88 utilities (cat, ls, grep, sed, awk, jq, yq, find, tar, gzip, curl, wget, bc, xxd, diff, patch, tree, rg, fd, sort, uniq, cut, tr, head, tail, wc, base64, md5sum, sha256sum, etc.) and Python 3.13 via Pyodide (python3 -c "..." or writing .py files and executing them).

Generate exactly COUNT diverse tasks as a JSON array. Each task object must have:
- "id": unique short slug (lowercase, hyphens)
- "category": one of CATEGORIES
- "description": clear natural language description of what the agent should do (2-4 sentences). Always use absolute paths under /workspace/. Be specific about expected output format.
- "seed_files": array of {"path": "/workspace/...", "content": "..."} to pre-load (can be empty [])
- "verification": a single shell command that prints exactly "PASS" if the task was completed correctly, or "FAIL" followed by a reason. Use simple checks: file existence, grep, diff, python3 -c to validate JSON, etc.

Rules:
- Tasks must be completable by an LLM agent with execute, write_file, read_file tools
- Cover different utilities, Python modules (json, csv, math, os, re, collections, itertools), pipes, redirects, loops, error handling
- Vary difficulty from simple (single command) to complex (multi-step pipeline)
- Include edge cases: filenames with spaces, empty files, large output, unicode
- The verification command must be self-contained — it should not depend on the agent's approach, only the outcome
- Do NOT use utilities or features that don't exist in the sandbox (no apt, npm, pip, git, docker, ssh, nc, nmap)
- Python has no pip packages — only stdlib modules
- For "shell-advanced": use heredocs (<<EOF), here-strings (<<<), nested command substitution $(cmd $(cmd)), process substitution <(cmd), arithmetic $(( )), arrays, declare -A, case statements, trap, while read loops, brace expansion {a,b,c}, parameter expansion ${var:-default}, ${var//pattern/replace}
- For "combined": combine shell + Python in a pipeline (e.g., shell generates data, Python processes it, shell verifies)
- IMPORTANT: Avoid repeating the same patterns — each task should stress a DIFFERENT utility or shell feature

Output ONLY the raw JSON array. No markdown fences, no explanation.`;

export async function generateTasks(count, options = {}) {
  const category = options.category;
  const model = new ChatAnthropic({
    model: "claude-haiku-4-5-20251001",
    temperature: 1.0,
    maxTokens: 8192,
  });

  const categories = category
    ? [category]
    : CATEGORIES;

  const prompt = SYSTEM_PROMPT
    .replace("COUNT", String(count))
    .replace("CATEGORIES", JSON.stringify(categories));

  const response = await model.invoke([
    { role: "system", content: prompt },
    { role: "user", content: `Generate ${count} random tasks across categories: ${categories.join(", ")}` },
  ]);

  const text = typeof response.content === "string"
    ? response.content
    : response.content.map((b) => b.text || "").join("");

  // Strip markdown fences if the model included them
  const cleaned = text.replace(/^```(?:json)?\n?/, "").replace(/\n?```$/, "").trim();

  let tasks;
  try {
    tasks = JSON.parse(cleaned);
  } catch (e) {
    // Try to extract the largest valid JSON array
    const match = cleaned.match(/\[[\s\S]*\]/);
    if (match) {
      try {
        tasks = JSON.parse(match[0]);
      } catch {
        throw new Error(`Task generation returned invalid JSON: ${e.message}\n${cleaned.slice(0, 500)}`);
      }
    } else {
      throw new Error(`Task generation returned invalid JSON: ${e.message}\n${cleaned.slice(0, 500)}`);
    }
  }

  if (!Array.isArray(tasks)) {
    throw new Error("Task generation did not return an array");
  }

  return tasks;
}

export { CATEGORIES };
