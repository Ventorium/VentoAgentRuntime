export interface RuntimeClientOptions { baseUrl: string; token: string }
export interface CommandRequest { command: string[]; cwd?: string; env?: Record<string, string>; timeoutMs?: number; stdin?: Uint8Array }
export class RuntimeClient { constructor(options: RuntimeClientOptions); create(options?: Record<string, unknown>): Promise<Sandbox>; connect(id: string): Promise<Sandbox> }
export class Sandbox { readonly sandboxId: string; readonly info: Record<string, unknown>; readonly commands: {run(request: string[] | CommandRequest): Promise<unknown>}; readonly files: {write(path: string, data: Uint8Array): Promise<void>; read(path: string): Promise<Uint8Array>; list(path: string): Promise<unknown>}; pause(): Promise<unknown>; resume(): Promise<unknown>; destroy(): Promise<void> }
export function startLocalRuntime(binaryPath: string, listen: string, token: string, configPath?: string): number
