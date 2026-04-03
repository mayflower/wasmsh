export interface InitialFileInput {
  path: string;
  content: Uint8Array;
}

export interface RunResult {
  events: unknown[];
  output: string;
  stdout: string;
  stderr: string;
  exitCode: number | null;
}

export interface ReadFileResult {
  events: unknown[];
  content: Uint8Array;
}

export interface ListDirResult {
  events: unknown[];
  output: string;
}

export interface NodeSessionOptions {
  assetDir?: string;
  nodeExecutable?: string;
  stepBudget?: number;
  initialFiles?: InitialFileInput[];
  /** Hostnames allowed for network access (empty = deny all). */
  allowedHosts?: string[];
  /** Request timeout in milliseconds (default: 300000 = 5 minutes). 0 disables. */
  timeoutMs?: number;
}

export interface BrowserSessionOptions {
  assetBaseUrl: string;
  worker?: Worker;
  stepBudget?: number;
  initialFiles?: InitialFileInput[];
  /** Hostnames allowed for network access (empty = deny all). */
  allowedHosts?: string[];
  /** Request timeout in milliseconds (default: 300000 = 5 minutes). 0 disables. */
  timeoutMs?: number;
}

export interface InstallPythonPackagesOptions {
  /** Install dependencies (default: true). */
  deps?: boolean;
  /** Continue installing after failures (default: false). */
  keepGoing?: boolean;
  /** Allow pre-release versions (default: false). */
  pre?: boolean;
  /** Reinstall packages even if already installed (default: false). */
  reinstall?: boolean;
}

export interface InstallResult {
  /** Successfully installed requirements. */
  installed: Array<{
    requirement: string;
    /** Resolved package name (present for package-name installs). */
    name?: string;
    /** Resolved version (present for package-name installs). */
    version?: string;
  }>;
  /** Original requirements as passed. */
  requirements: string[];
}

export interface WasmshSession {
  run(command: string): Promise<RunResult>;
  writeFile(path: string, content: Uint8Array): Promise<{ events: unknown[] }>;
  readFile(path: string): Promise<ReadFileResult>;
  listDir(path: string): Promise<ListDirResult>;
  /**
   * Install Python packages into the sandbox.
   *
   * Supported requirement formats:
   * - `emfs:/path/to/wheel.whl` — install from the in-sandbox Emscripten filesystem
   * - `https://host/pkg-1.0-py3-none-any.whl` — download and install (requires allowedHosts)
   * - `"six"` — resolve from PyPI and install (requires allowedHosts incl. pypi.org)
   *
   * Only pure-Python wheels (py3-none-any) are supported.
   *
   * Security: `file:` URIs are rejected. Network installs will require `allowedHosts`.
   */
  installPythonPackages(
    requirements: string | string[],
    options?: InstallPythonPackagesOptions,
  ): Promise<InstallResult>;
  close(): Promise<void>;
}

export declare const DEFAULT_WORKSPACE_DIR = "/workspace";

export declare function resolveAssetPath(...segments: string[]): string;
export declare function resolveNodeHostPath(): string;
export declare function resolveBrowserWorkerPath(): URL;

export declare function createNodeSession(
  options?: NodeSessionOptions,
): Promise<WasmshSession>;

export declare function createBrowserWorkerSession(
  options: BrowserSessionOptions,
): Promise<WasmshSession>;
