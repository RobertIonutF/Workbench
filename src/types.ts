export interface Connection {
  id: string; label: string; preset: string; protocol: 'openai_chat' | 'openai_responses' | 'anthropic' | 'gemini';
  baseUrl: string; modelId: string; contextWindow: number; tools: boolean; secretPresent: boolean;
  authKind: 'none' | 'api_key' | 'openai_oauth'; createdAt: string;
}
export interface Project { id: string; name: string; path: string; createdAt: string }
export type ExecutionMode = 'ask' | 'read_only' | 'trusted_native' | 'docker';
export interface Chat {
  id: string; projectId?: string; title: string; connectionId: string; modelId: string;
  pluginIds: string[]; skillIds: string[]; executionMode: ExecutionMode; createdAt: string; updatedAt: string;
}
export interface Skill { id: string; name: string; description: string; path: string }
export interface Server { name: string; type: string; command?: string; args?: string[]; url?: string }
export interface Plugin {
  id: string; name: string; version: string; description: string; path: string; enabled: boolean; source: string;
  skills: Skill[]; servers: Server[]; diagnostics: string[]; createdAt: string;
}
export interface Run { id: string; chatId: string; status: 'running' | 'completed' | 'failed' | 'canceled' | 'interrupted'; error?: string; createdAt: string }
export interface Approval { id: string; runId: string; chatId: string; title: string; details: unknown; status: 'pending' | 'allowed' | 'denied'; createdAt: string }
export interface Settings { theme: 'dark' | 'light' | 'system'; maxIterations: number; commandTimeoutSecs: number; autoActivateSkills: boolean }
export interface AppState { connections: Connection[]; projects: Project[]; chats: Chat[]; plugins: Plugin[]; runs: Run[]; approvals: Approval[]; settings: Settings; version: string }
export interface Message { id: string; chatId: string; role: 'user' | 'assistant' | 'tool'; content: string; createdAt: string; toolName?: string; toolCalls?: unknown[] }
export interface RunEvent { id: number; runId: string; chatId: string; kind: string; data: unknown; createdAt: string }
export interface FileEntry { name: string; path: string; isDir: boolean; size: number }
export interface Model { id: string; name: string; loaded?: boolean; contextWindow?: number }
export interface Probe { text: boolean; streaming: boolean; tools: boolean; reply: string; diagnostics: string[] }
export interface Check { name: string; ok: boolean; detail: string }

export type Page = 'workspace' | 'connections' | 'plugins' | 'settings';
