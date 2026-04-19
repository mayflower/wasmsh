/**
 * Minimal langchain-wasmsh remote sandbox example (no LLM).
 *
 * Same surface as basic.ts but routed through a running wasmsh
 * dispatcher (e.g. the stack in deploy/docker/compose.dispatcher-test.yml).
 *
 * Run (from repo root):
 *   docker compose -f deploy/docker/compose.dispatcher-test.yml up -d --wait
 *   WASMSH_DISPATCHER_URL=http://localhost:8080 \
 *     pnpm --filter wasmsh-deepagent-typescript-example run remote-basic
 */
import { WasmshRemoteSandbox } from "@mayflowergmbh/langchain-wasmsh";

async function main() {
  const url = process.env.WASMSH_DISPATCHER_URL;
  if (!url) {
    console.error(
      "Set WASMSH_DISPATCHER_URL to your wasmsh dispatcher (e.g. http://localhost:8080).",
    );
    process.exit(1);
  }

  const sandbox = await WasmshRemoteSandbox.create({
    dispatcherUrl: url,
    initialFiles: { "/workspace/data.txt": "hello from remote wasmsh" },
  });

  try {
    const result = await sandbox.execute(
      `cat data.txt && python3 -c "print(open('/workspace/data.txt').read().strip())"`,
    );
    console.log(result.output);
  } finally {
    await sandbox.stop();
  }
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
