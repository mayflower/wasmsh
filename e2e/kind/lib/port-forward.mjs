import { spawn } from "node:child_process";
import net from "node:net";

// Opens a kubectl port-forward that outlives the calling test module.
// The caller is responsible for calling stop() to release the port.
export async function openPortForward({
  kubeconfig,
  context,
  namespace,
  target,
  targetPort,
  localPort = 0,
  readyTimeoutMs = 20_000,
}) {
  const args = [];
  if (kubeconfig) args.push("--kubeconfig", kubeconfig);
  if (context) args.push("--context", context);
  if (namespace) args.push("--namespace", namespace);
  args.push("port-forward", target, `${localPort}:${targetPort}`, "--address", "127.0.0.1");

  const child = spawn("kubectl", args, { stdio: ["ignore", "pipe", "pipe"] });
  let resolvedPort = localPort;
  let readyResolve;
  let readyReject;
  const ready = new Promise((resolve, reject) => {
    readyResolve = resolve;
    readyReject = reject;
  });
  const timer = setTimeout(() => {
    readyReject(new Error(`port-forward to ${target}:${targetPort} did not become ready`));
    child.kill("SIGTERM");
  }, readyTimeoutMs);

  let stderr = "";
  child.stdout.setEncoding("utf8");
  child.stderr.setEncoding("utf8");
  child.stdout.on("data", (chunk) => {
    const match = chunk.match(/Forwarding from 127\.0\.0\.1:(\d+)/);
    if (match) {
      resolvedPort = Number(match[1]);
      clearTimeout(timer);
      readyResolve({ port: resolvedPort });
    }
  });
  child.stderr.on("data", (chunk) => {
    stderr += chunk;
  });
  child.on("exit", (code, signal) => {
    clearTimeout(timer);
    if (code !== 0 && code !== null) {
      readyReject(new Error(`port-forward exited ${code}: ${stderr.trim()}`));
    }
  });

  const { port } = await ready;

  // kubectl prints "Forwarding from ..." before the listener is fully
  // accepting.  Confirm with an actual TCP connect before handing the
  // port over to the caller.
  await waitForTcpOpen("127.0.0.1", port, 10_000);

  return {
    port,
    url: `http://127.0.0.1:${port}`,
    child,
    async stop() {
      if (child.exitCode !== null || child.signalCode) return;
      child.kill("SIGTERM");
      await new Promise((resolve) => {
        const bail = setTimeout(() => {
          child.kill("SIGKILL");
          resolve();
        }, 5_000);
        child.once("exit", () => {
          clearTimeout(bail);
          resolve();
        });
      });
    },
  };
}

function waitForTcpOpen(host, port, timeoutMs) {
  return new Promise((resolve, reject) => {
    const deadline = Date.now() + timeoutMs;
    function attempt() {
      const socket = net.createConnection({ host, port });
      socket.once("connect", () => {
        socket.end();
        resolve();
      });
      socket.once("error", () => {
        socket.destroy();
        if (Date.now() > deadline) {
          reject(new Error(`tcp ${host}:${port} not reachable within ${timeoutMs}ms`));
          return;
        }
        setTimeout(attempt, 200);
      });
    }
    attempt();
  });
}
