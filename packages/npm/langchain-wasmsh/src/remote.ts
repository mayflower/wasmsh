import {
  BaseSandbox,
  type ExecuteResponse,
  type FileDownloadResponse,
  type FileUploadResponse,
  type GrepMatch,
  type GrepResult,
} from "deepagents";

import {
  errorMessage,
  getDiagnosticError,
  mapDownloadError,
  shellQuote,
} from "./internal.js";

/**
 * Options accepted by {@link WasmshRemoteSandbox.create}.
 *
 * `dispatcherUrl` is the only required field; everything else mirrors the
 * in-process `WasmshSandbox` ergonomically.
 */
export interface WasmshRemoteSandboxOptions {
  /** Base URL of the wasmsh dispatcher, e.g. `http://localhost:8080`. */
  dispatcherUrl: string;
  /** Reuse an existing dispatcher session instead of creating a new one. */
  sessionId?: string;
  /** Hostnames allowed for network access (empty = deny all). */
  allowedHosts?: string[];
  /** VM step budget per command. 0 means unlimited. */
  stepBudget?: number;
  /** Files to seed at session creation. Keys are absolute paths. */
  initialFiles?: Record<string, string | Uint8Array>;
  /** Working directory prepended to every `execute()`. Defaults to `/workspace`. */
  workingDirectory?: string;
  /** Extra HTTP headers forwarded with every request (future auth hook). */
  headers?: Record<string, string>;
  /** Inject a custom fetch (useful for tests). */
  fetch?: typeof globalThis.fetch;
}

const DEFAULT_WORKING_DIRECTORY = "/workspace";

function encodeBase64(bytes: Uint8Array): string {
  // Chunk to avoid call-stack blowups on large payloads.  btoa is
  // synchronous and available in every modern runtime (Node ≥18 + all
  // browsers), so we don't take a Buffer dependency.
  let binary = "";
  const chunk = 0x8000;
  for (let i = 0; i < bytes.length; i += chunk) {
    binary += String.fromCharCode(...bytes.subarray(i, i + chunk));
  }
  return btoa(binary);
}

function decodeBase64(base64: string): Uint8Array {
  const binary = atob(base64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}

function toInitialFilePayload(
  files: Record<string, string | Uint8Array> | undefined,
): Array<{ path: string; contentBase64: string }> {
  if (!files) return [];
  return Object.entries(files).map(([path, content]) => ({
    path,
    contentBase64: encodeBase64(
      typeof content === "string" ? new TextEncoder().encode(content) : content,
    ),
  }));
}

interface DispatcherEnvelope<T = unknown> {
  ok?: boolean;
  error?: string;
  result?: T;
  session?: {
    sessionId?: string;
    [key: string]: unknown;
  };
}

interface RunResult {
  output: string;
  exitCode: number | null;
  events?: unknown[];
}

interface FileResult {
  contentBase64?: string;
  events?: unknown[];
}

/**
 * Wasmsh sandbox backed by a remote dispatcher + runner pool.
 *
 * Use this when you want Kubernetes-scale concurrency or sessions that
 * outlive the client process.  For single-process use prefer the
 * in-process {@link WasmshSandbox}.
 *
 * The dispatcher contract is documented in
 * `docs/reference/dispatcher-api.md`; the Helm chart in
 * `deploy/helm/wasmsh/` provisions the control plane.
 */
export class WasmshRemoteSandbox extends BaseSandbox {
  #baseUrl: string;

  #sessionId: string;

  #workingDirectory: string;

  #headers: Record<string, string>;

  #fetch: typeof globalThis.fetch;

  #closed = false;

  private constructor(options: {
    baseUrl: string;
    sessionId: string;
    workingDirectory: string;
    headers: Record<string, string>;
    fetch: typeof globalThis.fetch;
  }) {
    super();
    this.#baseUrl = options.baseUrl;
    this.#sessionId = options.sessionId;
    this.#workingDirectory = options.workingDirectory;
    this.#headers = options.headers;
    this.#fetch = options.fetch;
  }

  get id(): string {
    return this.#sessionId;
  }

  get isRunning(): boolean {
    return !this.#closed;
  }

  /** Create a remote sandbox bound to a dispatcher session. */
  static async create(
    options: WasmshRemoteSandboxOptions,
  ): Promise<WasmshRemoteSandbox> {
    const baseUrl = options.dispatcherUrl.replace(/\/+$/, "");
    const clientSessionId =
      options.sessionId ?? `wasmsh-ts-${crypto.randomUUID()}`;
    const workingDirectory =
      options.workingDirectory ?? DEFAULT_WORKING_DIRECTORY;
    const headers = { ...(options.headers ?? {}) };
    const doFetch = options.fetch ?? globalThis.fetch.bind(globalThis);

    const response = await doFetch(`${baseUrl}/sessions`, {
      method: "POST",
      headers: {
        "content-type": "application/json",
        ...headers,
      },
      body: JSON.stringify({
        session_id: clientSessionId,
        allowed_hosts: options.allowedHosts ?? [],
        step_budget: options.stepBudget ?? 0,
        initial_files: toInitialFilePayload(options.initialFiles),
      }),
    });
    const envelope = await readEnvelope(response, "/sessions");

    const serverSessionId = envelope.session?.sessionId ?? clientSessionId;
    return new WasmshRemoteSandbox({
      baseUrl,
      sessionId:
        typeof serverSessionId === "string" && serverSessionId
          ? serverSessionId
          : clientSessionId,
      workingDirectory,
      headers,
      fetch: doFetch,
    });
  }

  async execute(command: string): Promise<ExecuteResponse> {
    const envelope = await this.#post<RunResult>(
      `/sessions/${this.#sessionId}/run`,
      {
        command: `cd ${shellQuote(this.#workingDirectory)} && ${command}`,
      },
    );
    const result = envelope.result ?? { output: "", exitCode: null };
    return {
      output: result.output ?? "",
      exitCode: result.exitCode ?? null,
      truncated: false,
    };
  }

  async uploadFiles(
    files: Array<[string, Uint8Array]>,
  ): Promise<FileUploadResponse[]> {
    return Promise.all(
      files.map(async ([path, content]): Promise<FileUploadResponse> => {
        if (!path.startsWith("/")) {
          return { path, error: "invalid_path" };
        }
        try {
          const envelope = await this.#post<FileResult>(
            `/sessions/${this.#sessionId}/write-file`,
            { path, contentBase64: encodeBase64(content) },
          );
          const diagnostic = getDiagnosticError(envelope.result?.events);
          return {
            path,
            error: diagnostic ? mapDownloadError(diagnostic) : null,
          };
        } catch (error: unknown) {
          return { path, error: mapDownloadError(errorMessage(error)) };
        }
      }),
    );
  }

  async downloadFiles(paths: string[]): Promise<FileDownloadResponse[]> {
    return Promise.all(
      paths.map(async (path): Promise<FileDownloadResponse> => {
        if (!path.startsWith("/")) {
          return { path, content: null, error: "invalid_path" };
        }
        // The dispatcher's VFS reads directories as empty bytes; do the
        // same pre-check the in-process backend does.
        try {
          const check = await this.execute(
            `test -d ${shellQuote(path)} && echo DIR || true`,
          );
          if (check.output.trim() === "DIR") {
            return { path, content: null, error: "is_directory" };
          }
        } catch {
          // Fall through to the read attempt; the error surface below
          // still classifies the failure correctly.
        }
        try {
          const envelope = await this.#post<FileResult>(
            `/sessions/${this.#sessionId}/read-file`,
            { path },
          );
          const diagnostic = getDiagnosticError(envelope.result?.events);
          if (diagnostic) {
            return { path, content: null, error: mapDownloadError(diagnostic) };
          }
          const contentBase64 = envelope.result?.contentBase64;
          if (typeof contentBase64 !== "string") {
            return { path, content: null, error: "invalid_path" };
          }
          return { path, content: decodeBase64(contentBase64), error: null };
        } catch (error: unknown) {
          return {
            path,
            content: null,
            error: mapDownloadError(errorMessage(error)),
          };
        }
      }),
    );
  }

  async grep(
    pattern: string,
    path: string = "/",
    glob: string | null = null,
  ): Promise<GrepResult> {
    if (!glob) {
      return super.grep(pattern, path, glob);
    }
    // wasmsh's `find` lacks `-exec`, same constraint as the in-process
    // variant — use `grep --include=GLOB` instead.
    const command = `grep -rHnF --include=${shellQuote(glob)} -e ${shellQuote(
      pattern,
    )} ${shellQuote(path)} 2>/dev/null || true`;
    const result = await this.execute(command);
    const output = result.output.trim();
    if (!output) {
      return { matches: [] };
    }
    const matches: GrepMatch[] = [];
    for (const line of output.split("\n")) {
      const parts = line.split(":");
      if (parts.length < 3) continue;
      const lineNum = parseInt(parts[1], 10);
      if (isNaN(lineNum)) continue;
      matches.push({
        path: parts[0],
        line: lineNum,
        text: parts.slice(2).join(":"),
      });
    }
    return { matches };
  }

  async stop(): Promise<void> {
    if (this.#closed) return;
    this.#closed = true;
    // Best effort: neither call must throw, because agent teardown often
    // runs when the dispatcher is already unreachable.
    for (const step of [
      () => this.#post(`/sessions/${this.#sessionId}/close`, {}),
      () => this.#delete(`/sessions/${this.#sessionId}`),
    ]) {
      try {
        await step();
      } catch {
        // swallow; teardown is advisory
      }
    }
  }

  async close(): Promise<void> {
    await this.stop();
  }

  async #post<T = unknown>(
    path: string,
    body: unknown,
  ): Promise<DispatcherEnvelope<T>> {
    const response = await this.#fetch(`${this.#baseUrl}${path}`, {
      method: "POST",
      headers: {
        "content-type": "application/json",
        ...this.#headers,
      },
      body: JSON.stringify(body),
    });
    return readEnvelope<T>(response, path);
  }

  async #delete(path: string): Promise<DispatcherEnvelope> {
    const response = await this.#fetch(`${this.#baseUrl}${path}`, {
      method: "DELETE",
      headers: { ...this.#headers },
    });
    return readEnvelope(response, path);
  }
}

async function readEnvelope<T = unknown>(
  response: Response,
  path: string,
): Promise<DispatcherEnvelope<T>> {
  let text: string;
  try {
    text = await response.text();
  } catch (error: unknown) {
    throw new Error(
      `dispatcher ${path}: failed to read response body: ${errorMessage(error)}`,
    );
  }
  let envelope: DispatcherEnvelope<T>;
  try {
    envelope = text ? (JSON.parse(text) as DispatcherEnvelope<T>) : {};
  } catch {
    throw new Error(
      `dispatcher ${path} returned non-JSON body (status ${response.status}): ${text.slice(0, 200)}`,
    );
  }
  if (!response.ok || envelope.ok === false) {
    const reason =
      envelope.error ?? `dispatcher error (status ${response.status})`;
    throw new Error(`dispatcher ${path}: ${reason}`);
  }
  return envelope;
}
