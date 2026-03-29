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
}

export interface BrowserSessionOptions {
  assetBaseUrl: string;
  worker?: Worker;
  stepBudget?: number;
  initialFiles?: InitialFileInput[];
}

export interface WasmshSession {
  run(command: string): Promise<RunResult>;
  writeFile(path: string, content: Uint8Array): Promise<{ events: unknown[] }>;
  readFile(path: string): Promise<ReadFileResult>;
  listDir(path: string): Promise<ListDirResult>;
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
