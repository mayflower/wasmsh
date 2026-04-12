/**
 * Task generation — asks the LLM to invent random sandbox tasks.
 */
import { ChatAnthropic } from "@langchain/anthropic";

const CATEGORIES = [
  "file-ops",
  "text-processing",
  "python",
  "python-packages",
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

The sandbox supports micropip for installing pure-Python packages at runtime. The agent can install packages via:
  python3 -c "import micropip; await micropip.install('package-name')"
Or by writing a .py script that imports micropip. Available packages include popular pure-Python libraries from PyPI and Pyodide's CDN: six, attrs, click, packaging, beautifulsoup4, networkx, idna, certifi, pyyaml, more-itertools, decorator, wrapt, toml, tomli, chardet, pytz, markupsafe, jinja2, pluggy, pyparsing, and many other pure-Python wheels.

Generate exactly COUNT diverse tasks as a JSON array. Each task object must have:
- "id": unique short slug (lowercase, hyphens)
- "category": one of CATEGORIES
- "description": clear natural language description of what the agent should do (2-4 sentences). Always use absolute paths under /workspace/. Be specific about expected output format.
- "seed_files": array of {"path": "/workspace/...", "content": "..."} to pre-load (can be empty [])
- "verification": a single shell command that prints exactly "PASS" if the task was completed correctly, or "FAIL" followed by a reason. Use simple checks: file existence, grep, diff, python3 -c to validate JSON/YAML, etc.

Rules:
- Tasks must be completable by an LLM agent with execute, write_file, read_file tools
- Cover different utilities, Python stdlib modules (json, csv, math, os, re, collections, itertools, pathlib, textwrap, html, urllib.parse, hashlib, struct, sqlite3, xml.etree, configparser, argparse, dataclasses, typing, functools, contextlib), pipes, redirects, loops, error handling
- Vary difficulty from simple (single command) to complex (multi-step pipeline)
- Include edge cases: filenames with spaces, empty files, large output, unicode
- The verification command must be self-contained — it should not depend on the agent's approach, only the outcome
- Do NOT use utilities or features that don't exist in the sandbox (no apt, npm, pip, git, docker, ssh, nc, nmap)
- For "python": use ONLY stdlib modules (no micropip installs). Exercise stdlib depth: pathlib, dataclasses, textwrap, html.parser, sqlite3, struct, xml.etree.ElementTree, configparser, etc.
- For "python-packages": ALWAYS install at least one package via micropip. Use real packages: beautifulsoup4 (parse HTML), pyyaml (parse/generate YAML), attrs/dataclasses patterns, jinja2 (templates), networkx (graphs), click (CLI), six (py2/3 compat), toml/tomli (TOML parsing), markupsafe (escaping), chardet (encoding detection), pyparsing (text parsing), more-itertools (iteration). Tasks should demonstrate WHY you'd use the package, not just import it.
- For "shell-advanced": use heredocs (<<EOF), here-strings (<<<), nested command substitution, process substitution <(cmd), arithmetic (( )), arrays, declare -A, case statements, trap, while read loops, brace expansion, parameter expansion with defaults and substitution
- For "combined": mix shell + Python in pipelines. May use micropip packages if the category is also "combined".
- IMPORTANT: Avoid repeating the same patterns — each task should stress a DIFFERENT utility, shell feature, or Python package
- IMPORTANT: seed_files content must be SHORT plain text (< 500 chars each). NEVER embed base64-encoded binary data, tar/gz archives, or zip files in seed_files — the agent can create those at runtime. If a task needs an archive, describe it in the task description and let the agent build it.

Output ONLY the raw JSON array. No markdown fences, no explanation.`;

export async function generateTasks(count, options = {}) {
  const category = options.category;
  const model = new ChatAnthropic({
    model: "claude-haiku-4-5-20251001",
    temperature: 1.0,
    maxTokens: 16384,
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
