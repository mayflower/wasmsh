/**
 * wasmsh Browser Example
 *
 * Prerequisites:
 *   wasm-pack build crates/wasmsh-browser --target web --release --out-dir ../../pkg/web
 *
 * Serve this directory with any HTTP server:
 *   python3 -m http.server 8080
 *   Then open http://localhost:8080/examples/web/
 */

import wasmInit, { WasmShell } from "../../pkg/web/wasmsh_browser.js";

const output = document.getElementById("output");
const cmdInput = document.getElementById("cmd");

let shell = null;

// -- Helpers --

function appendOutput(text, className) {
  const span = document.createElement("span");
  span.className = className;
  span.textContent = text;
  output.appendChild(span);
  output.scrollTop = output.scrollHeight;
}

function decodeBytes(arr) {
  return new TextDecoder().decode(new Uint8Array(arr));
}

function processEvents(events) {
  for (const evt of events) {
    if ("Stdout" in evt) {
      appendOutput(decodeBytes(evt.Stdout), "stdout");
    } else if ("Stderr" in evt) {
      appendOutput(decodeBytes(evt.Stderr), "stderr");
    } else if ("Diagnostic" in evt) {
      const [level, msg] = evt.Diagnostic;
      appendOutput("[" + level + "] " + msg + "\n", "stderr");
    }
  }
}

function runCommand(cmd) {
  appendOutput("$ " + cmd + "\n", "cmd");
  try {
    const json = shell.exec(cmd);
    const events = JSON.parse(json);
    processEvents(events);
  } catch (e) {
    appendOutput("Error: " + e.message + "\n", "stderr");
  }
}

// -- Initialization --

async function startup() {
  appendOutput("Loading wasmsh...\n", "info");

  try {
    await wasmInit();
    shell = new WasmShell();
    const initJson = shell.init(BigInt(0));
    const initEvents = JSON.parse(initJson);
    const version = initEvents.find(function (e) { return "Version" in e; });
    appendOutput(
      "wasmsh " + (version ? version.Version : "?") + " ready. Type shell commands below.\n\n",
      "info"
    );

    // Run demo commands
    runCommand("echo 'Welcome to wasmsh in the browser!'");
    runCommand("uname -a");
    runCommand("fruits=(apple banana cherry); echo ${fruits[@]}");
    runCommand("echo $((6 * 7))");
  } catch (e) {
    appendOutput("Failed to load: " + e.message + "\n", "stderr");
    appendOutput(
      "\nMake sure you built the web target:\n" +
        "  wasm-pack build crates/wasmsh-browser --target web --release --out-dir ../../pkg/web\n" +
        "And serve this directory via HTTP (not file://).\n",
      "info"
    );
  }
}

// -- Input handling --

cmdInput.addEventListener("keydown", function (e) {
  if (e.key === "Enter" && shell) {
    const cmd = cmdInput.value.trim();
    if (cmd) {
      runCommand(cmd);
      cmdInput.value = "";
    }
  }
});

startup();
