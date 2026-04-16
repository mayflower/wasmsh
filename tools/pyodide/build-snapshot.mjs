import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { buildSnapshot } from "../../packages/npm/wasmsh-pyodide/lib/snapshot/builder.mjs";

const __dirname = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(__dirname, "../..");

function parseArgs(argv) {
  const options = {
    assetDir: resolve(repoRoot, "packages/npm/wasmsh-pyodide/assets"),
    outputDir: resolve(repoRoot, "dist/pyodide-snapshot"),
  };
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === "--asset-dir" && argv[index + 1]) {
      options.assetDir = resolve(argv[index + 1]);
      index += 1;
    } else if (arg === "--output-dir" && argv[index + 1]) {
      options.outputDir = resolve(argv[index + 1]);
      index += 1;
    }
  }
  return options;
}

const options = parseArgs(process.argv.slice(2));
const artifact = await buildSnapshot(options);
process.stdout.write(`${JSON.stringify({
  ok: true,
  outputDir: options.outputDir,
  snapshotDigest: artifact.manifest.snapshot_digest,
})}\n`);
