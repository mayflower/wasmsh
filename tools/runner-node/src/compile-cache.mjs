import { mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { resolve } from "node:path";

const defaultCacheDir = resolve(
  process.env.WASMSH_NODE_COMPILE_CACHE_DIR ?? tmpdir(),
  "wasmsh-node-compile-cache",
);

export function applyCompileCacheEnv(env = process.env) {
  mkdirSync(defaultCacheDir, { recursive: true });
  return {
    ...env,
    NODE_COMPILE_CACHE: env.NODE_COMPILE_CACHE ?? defaultCacheDir,
  };
}
