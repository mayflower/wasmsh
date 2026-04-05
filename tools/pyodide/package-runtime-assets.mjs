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

function readPackagedExtras(dir) {
  const extras = new Map();
  if (!fs.existsSync(dir)) {
    return extras;
  }
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    if (!entry.isFile()) {
      continue;
    }
    if (entry.name === "pyodide-lock.json" || entry.name.endsWith(".whl")) {
      extras.set(entry.name, fs.readFileSync(path.join(dir, entry.name)));
    }
  }
  return extras;
}

function writePackagedExtras(dir, extras) {
  for (const [name, bytes] of extras) {
    fs.writeFileSync(path.join(dir, name), bytes);
  }
}

if (!fs.existsSync(sourceDir)) {
  throw new Error(`Pyodide dist not found at ${sourceDir}`);
}

const npmAssetsDir = path.join(npmPackageDir, "assets");
const pythonAssetsDir = path.join(pythonPackageDir, "assets");
const packagedExtras = readPackagedExtras(npmAssetsDir);

copyDir(sourceDir, npmAssetsDir);
writePackagedExtras(npmAssetsDir, packagedExtras);

copyDir(sourceDir, pythonAssetsDir);
writePackagedExtras(pythonAssetsDir, packagedExtras);

fs.copyFileSync(path.join(npmPackageDir, "node-host.mjs"), path.join(pythonAssetsDir, "node-host.mjs"));
copyDir(path.join(npmPackageDir, "lib"), path.join(pythonAssetsDir, "lib"));
const packageJsonContent = `${JSON.stringify({ type: "commonjs" }, null, 2)}\n`;
fs.writeFileSync(path.join(npmAssetsDir, "package.json"), packageJsonContent);
fs.writeFileSync(path.join(pythonAssetsDir, "package.json"), packageJsonContent);

console.log("Packaged wasmsh Pyodide runtime assets:");
console.log(`  npm:    ${npmAssetsDir}`);
console.log(`  python: ${pythonAssetsDir}`);
