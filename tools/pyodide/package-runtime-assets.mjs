import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(__dirname, "../..");
const sourceDir = path.join(repoRoot, "dist", "pyodide-custom");
const npmPackageDir = path.join(repoRoot, "packages", "npm", "wasmsh-pyodide");
const pythonPackageDir = path.join(
  repoRoot,
  "packages",
  "python",
  "wasmsh-pyodide-runtime",
  "wasmsh_pyodide_runtime",
);

function copyDir(source, target) {
  fs.rmSync(target, { recursive: true, force: true });
  fs.mkdirSync(target, { recursive: true });
  for (const entry of fs.readdirSync(source, { withFileTypes: true })) {
    const sourcePath = path.join(source, entry.name);
    const targetPath = path.join(target, entry.name);
    if (entry.isDirectory()) {
      copyDir(sourcePath, targetPath);
    } else {
      fs.copyFileSync(sourcePath, targetPath);
    }
  }
}

if (!fs.existsSync(sourceDir)) {
  throw new Error(`Pyodide dist not found at ${sourceDir}`);
}

copyDir(sourceDir, path.join(npmPackageDir, "assets"));
copyDir(sourceDir, path.join(pythonPackageDir, "assets"));
fs.copyFileSync(path.join(npmPackageDir, "node-host.mjs"), path.join(pythonPackageDir, "assets", "node-host.mjs"));
copyDir(path.join(npmPackageDir, "lib"), path.join(pythonPackageDir, "assets", "lib"));
fs.writeFileSync(
  path.join(npmPackageDir, "assets", "package.json"),
  `${JSON.stringify({ type: "commonjs" }, null, 2)}\n`,
);

console.log("Packaged wasmsh Pyodide runtime assets:");
console.log(`  npm:    ${path.join(npmPackageDir, "assets")}`);
console.log(`  python: ${path.join(pythonPackageDir, "assets")}`);
