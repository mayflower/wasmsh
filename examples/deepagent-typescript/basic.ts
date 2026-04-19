/**
 * Minimal langchain-wasmsh example (no LLM).
 *
 * Uses WasmshSandbox directly to run bash + Python in the same virtual
 * filesystem, without going through a Deep Agent.
 *
 * Run:
 *   pnpm exec tsx basic.ts
 */
import { WasmshSandbox } from "@mayflowergmbh/langchain-wasmsh";

async function main() {
  const sandbox = await WasmshSandbox.createNode({
    initialFiles: {
      "/workspace/data.txt": "hello from wasmsh",
    },
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
