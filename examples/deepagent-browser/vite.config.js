import { dirname, resolve } from "node:path";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import { defineConfig } from "vite";
import sirv from "sirv";

const __dirname = dirname(fileURLToPath(import.meta.url));
const require = createRequire(import.meta.url);

// Resolve langchain and wasmsh-pyodide paths
const langchainDir = dirname(require.resolve("langchain/package.json"));
const wasmshPkgDir = dirname(
  require.resolve("@mayflowergmbh/wasmsh-pyodide/package.json"),
);

export default defineConfig({
  plugins: [
    {
      name: "wasmsh-assets",
      configureServer(server) {
        // Serve Pyodide assets and browser-worker.js at /assets/
        server.middlewares.use(
          "/assets",
          sirv(resolve(wasmshPkgDir, "assets"), { dev: true }),
        );
        server.middlewares.use(
          "/assets",
          sirv(wasmshPkgDir, { dev: true }),
        );
      },
    },
  ],
  resolve: {
    alias: [
      // langchain browser entry omits agent middleware — use full entry
      { find: /^langchain$/, replacement: resolve(langchainDir, "dist/index.js") },
      { find: /^langchain\/(.+)/, replacement: resolve(langchainDir, "dist/$1") },
      // Node.js polyfills for browser
      { find: /^node:async_hooks$/, replacement: resolve(__dirname, "shims/async-hooks.js") },
      { find: /^(node:)?path$/, replacement: "path-browserify" },
    ],
  },
  define: {
    "process.platform": JSON.stringify("browser"),
    "process.env": JSON.stringify({}),
  },
});
