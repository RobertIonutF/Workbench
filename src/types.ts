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
  profileId?: string; contextIds?: string[]; plan?: PlanStep[]; budget?: Budget; forkedFrom?: {chatId:string;messageId?:string};
}
export interface Skill { id: string; name: string; description: string; path: string }
export interface Server { name: string; type: string; command?: string; args?: string[]; url?: string }
export interface Plugin {
  id: string; name: string; version: string; description: string; path: string; enabled: boolean; source: string;
  skills: Skill[]; servers: Server[]; diagnostics: string[]; createdAt: string;
}
export interface Run { id: string; chatId: string; status: 'running' | 'paused' | 'completed' | 'failed' | 'canceled' | 'interrupted'; error?: string; createdAt: string }
export interface Approval { id: string; runId: string; chatId: string; title: string; details: unknown; status: 'pending' | 'allowed' | 'denied'; createdAt: string }
export interface Settings { theme: 'dark' | 'light' | 'system'; maxIterations: number; commandTimeoutSecs: number; autoActivateSkills: boolean; backgroundMode?:boolean }
export interface AppState { connections: Connection[]; projects: Project[]; chats: Chat[]; plugins: Plugin[]; runs: Run[]; approvals: Approval[]; settings: Settings; version: string; terminals?:Array<{id:string;projectId:string;program:string;status:string}>; profiles?:ModelProfile[]; usage?:Usage[]; memories?:Memory[]; hooks?:WorkflowHook[]; schedules?:Schedule[]; notifications?:Notice[]; comparisons?:Comparison[]; worktrees?:Worktree[]; teams?:Team[]; attachments?:Attachment[]; queue?:{turns:QueuedMessage[];steering:QueuedMessage[]} }
export interface Message { id: string; chatId: string; role: 'user' | 'assistant' | 'tool'; content: string; createdAt: string; toolName?: string; toolCalls?: unknown[]; images?:InlineImage[];canonical?:{images?:InlineImage[]} }
export interface InlineImage {mime:string;dataBase64:string}
export interface RunEvent { id: number; runId: string; chatId: string; kind: string; data: unknown; createdAt: string }
export interface FileEntry { name: string; path: string; isDir: boolean; size: number }
export interface Model { id: string; name: string; loaded?: boolean; contextWindow?: number }
export interface Probe { text: boolean; streaming: boolean; tools: boolean; reply: string; diagnostics: string[] }
export interface Check { name: string; ok: boolean; detail: string }

export type Page = 'workspace' | 'connections' | 'plugins' | 'settings' | 'control' | 'studio';
export interface Budget {maxRequests?:number;maxTokens?:number;maxCostUsd?:number|null}
export interface PlanStep {title:string;status:'pending'|'running'|'done'|'blocked'}
export interface ModelProfile {id:string;label:string;connectionId:string;modelId:string;contextWindow:number;maxOutputTokens:number;temperature?:number|null;reasoningEffort?:string;tools:boolean;vision:boolean;inputPrice?:number|null;outputPrice?:number|null;evidence?:unknown}
export interface Usage {id:string;runId:string;chatId:string;modelId:string;inputTokens:number;outputTokens:number;providerReported:boolean;durationMs:number;firstTokenMs?:number;estimatedCostUsd?:number|null;createdAt:string}
export interface Memory {id:string;projectId:string;title:string;content:string;source?:string;enabled:boolean}
export interface WorkflowHook {id:string;projectId:string;label:string;event:string;program:string;args:string[];enabled:boolean;timeoutSecs:number}
export interface Schedule {id:string;name:string;chatId:string;prompt:string;intervalMinutes:number;enabled:boolean;nextRunAt:string;lastStatus?:string;notify:boolean}
export interface Notice {id:string;title:string;chatId?:string;read:boolean;createdAt:string}
export interface Comparison {id:string;prompt:string;status:string;results:Array<{profileId:string;reply?:string;error?:string;passed?:boolean|null;toolProbe?:unknown;usage?:Usage}>}
export interface Worktree {id:string;projectId:string;sourceProjectId:string;path:string;branch:string;baseCommit:string}
export interface Team {id:string;projectId:string;prompt:string;mode:string;status:string;error?:string;results:Array<{role:string;profileId:string;chatId:string;projectId:string;status?:string;error?:string}>}
export interface Attachment {id:string;projectId?:string;chatId?:string;kind:string;name:string;path?:string;mime?:string;estimatedTokens?:number}

export interface QueuedMessage {id:string;chatId:string;text:string;status:string}
