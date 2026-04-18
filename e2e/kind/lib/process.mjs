import { spawn } from "node:child_process";

export class ProcessError extends Error {
  constructor(message, { command, args, exitCode, signal, stderr, stdout }) {
    super(message);
    this.name = "ProcessError";
    this.command = command;
    this.args = args;
    this.exitCode = exitCode;
    this.signal = signal;
    this.stderr = stderr;
    this.stdout = stdout;
  }
}

export function runCommand(command, args, options = {}) {
  const {
    cwd,
    env,
    input,
    stdio = ["pipe", "pipe", "pipe"],
    inherit = false,
    timeoutMs,
    allowedExitCodes = [0],
  } = options;

  return new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      cwd,
      env: env ? { ...process.env, ...env } : process.env,
      stdio: inherit ? "inherit" : stdio,
    });

    let stdout = "";
    let stderr = "";
    let timedOut = false;
    let timer = null;

    if (!inherit) {
      child.stdout?.setEncoding("utf8");
      child.stderr?.setEncoding("utf8");
      child.stdout?.on("data", (chunk) => {
        stdout += chunk;
      });
      child.stderr?.on("data", (chunk) => {
        stderr += chunk;
      });
    }

    if (input !== undefined && child.stdin) {
      child.stdin.end(input);
    } else if (child.stdin) {
      child.stdin.end();
    }

    if (timeoutMs) {
      timer = setTimeout(() => {
        timedOut = true;
        child.kill("SIGKILL");
      }, timeoutMs);
    }

    child.on("error", (error) => {
      if (timer) clearTimeout(timer);
      reject(error);
    });
    child.on("close", (code, signal) => {
      if (timer) clearTimeout(timer);
      if (timedOut) {
        reject(
          new ProcessError(`${command} timed out after ${timeoutMs}ms`, {
            command,
            args,
            exitCode: code,
            signal,
            stdout,
            stderr,
          }),
        );
        return;
      }
      if (!allowedExitCodes.includes(code)) {
        const head = stderr.trim().split("\n").slice(0, 20).join("\n");
        reject(
          new ProcessError(
            `${command} ${args.join(" ")} exited ${code}${head ? `:\n${head}` : ""}`,
            { command, args, exitCode: code, signal, stdout, stderr },
          ),
        );
        return;
      }
      resolve({ exitCode: code, signal, stdout, stderr });
    });
  });
}

export async function commandExists(command) {
  // Use posix `command -v` via /usr/bin/env so the probe never passes
  // its argument through a shell parser (avoids any injection surface).
  try {
    const result = await runCommand("/usr/bin/env", ["which", command], {
      allowedExitCodes: [0, 1],
    });
    return result.exitCode === 0;
  } catch {
    return false;
  }
}
