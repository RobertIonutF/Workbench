# Frontend API contract

Rust backend, camelCase JSON, all errors `{error: string}`. UI uses real endpoints. All `/api` requests need `X-Workbench-Token`. Desktop `invoke('server_info')` returns `{baseUrl, token}`. Browser mode serves UI at same origin and takes `#token=...` once, saves in sessionStorage and removes the fragment. Development can use `VITE_WORKBENCH_URL` (default same origin), token from fragment. Do not hardcode a token.

GET /api/state -> `{connections:Connection[],projects:Project[],chats:Chat[],plugins:Plugin[],runs:Run[],approvals:Approval[],settings:Settings,version:string}`.
Connection `{id,label,preset,protocol,baseUrl,modelId,contextWindow:number,tools:boolean,secretPresent:boolean,authKind:'none'|'api_key'|'openai_oauth',createdAt}`. Protocol `openai_chat|openai_responses|anthropic|gemini`. Presets `openai|anthropic|lmstudio|ollama|custom|gemini`.
Project `{id,name,path,createdAt}`.
Chat `{id,projectId?:string,title,connectionId,modelId,pluginIds:string[],skillIds:string[],executionMode:'ask'|'read_only'|'trusted_native'|'docker',createdAt,updatedAt}`.
Plugin `{id,name,version,description,path,enabled,source,skills:Skill[],servers:Server[],diagnostics:string[],createdAt}`. Skill `{id,name,description,path}`. Server `{name,type,command?:string,url?:string}`. skills IDs qualified `${pluginId}:${skillName}`.
Run `{id,chatId,status:'running'|'completed'|'failed'|'canceled'|'interrupted',error?:string,createdAt}`.
Approval `{id,runId,chatId,title,details:unknown,status:'pending'|'allowed'|'denied',createdAt}`.
Settings `{theme:'dark'|'light'|'system',maxIterations:number,commandTimeoutSecs:number,autoActivateSkills:boolean}`.

POST /api/connections body Connection fields (id optional) plus `apiKey?:string,removeSecret?:boolean` -> Connection. Blank apiKey preserves current. DELETE /api/connections/:id -> `{ok:true}`.
POST /api/connections/:id/models -> `{models:[{id,name,loaded?:boolean,contextWindow?:number}],diagnostics:string[]}`. POST /api/connections/:id/probe body `{modelId?:string}` -> `{text:boolean,streaming:boolean,tools:boolean,reply:string,diagnostics:string[]}`.
POST /api/oauth/openai/start -> `{url,loginId}`; open URL externally then refresh state (callback creates connection). OAuth errors reported by GET /api/oauth/:loginId -> `{status,error?}`.
POST /api/projects `{path,name?}` -> Project. DELETE /api/projects/:id -> `{ok:true}`.
POST /api/chats Chat input (id optional) -> Chat. PATCH /api/chats/:id with same editable fields -> Chat. DELETE /api/chats/:id -> `{ok:true}`.
GET /api/chats/:id/messages -> Message[]. Message `{id,chatId,role:'user'|'assistant'|'tool',content,createdAt,toolName?:string,toolCalls?:unknown[]}`.
POST /api/chats/:id/turns `{text}` -> Run. POST /api/runs/:id/cancel -> `{ok:true}`.
GET /api/events?after=0&chatId=... -> `{events:Event[],cursor:number}`. Event `{id:number,runId,chatId,kind,data:any,createdAt}`. kinds `text_delta` (data `{text}`), `tool_start`, `tool_result`, `approval`, `run_complete`, `run_error`, `skill_activated`, `usage`. Poll while run active; messages include final assistant/tool history. Avoid duplicate delta rendering when final messages refresh.
POST /api/approvals/:id `{decision:'allow_once'|'deny'}` -> `{ok:true}`.
GET /api/workspace/files?projectId=...&path=... -> `{entries:[{name,path,isDir,size}],path}`. GET /api/workspace/file?projectId=...&path=... -> `{path,content}`. PUT same path `{content}` -> `{ok:true}`. GET /api/workspace/diff?projectId=... -> `{diff:string}`. POST /api/workspace/undo `{chatId,path}` -> `{ok:true}`.
POST /api/plugins/install `{path}` -> Plugin (folder or ZIP). POST /api/plugins/create `{path,name,description,template:'skill'|'mcp'|'combined',language:'rust'|'typescript'|'python',skillBody?:string}` -> `{path,diagnostics:string[]}`. This creates source, not auto install.
Existing-server creation also accepts `serverUrl`, or `serverCommand` with `serverArgs:string[]`. POST /api/plugins/build `{path}` builds a generated Rust companion, or validates an interpreted package. Build and test actions execute declared source with native authority.
POST /api/plugins/:id/enable `{enabled:boolean}` -> Plugin. DELETE /api/plugins/:id -> `{ok:true}`.
POST /api/plugins/:id/check -> `{valid:boolean,diagnostics:string[]}`. POST /api/plugins/:id/test -> `{ok:boolean,tools:any[],diagnostics:string[]}`. Test launches declared MCPs with explicit UI action; display real native authority before action.
POST /api/plugins/:id/pack `{outputPath}` -> `{path}`. POST /api/plugins/:id/update `{path}` -> Plugin. POST /api/plugins/:id/rollback -> Plugin.
GET /api/plugins/:id/skills/:name -> `{content}`. PUT same `{content}` -> `{ok:true}` (author source only, runtime immutable version requires update).
POST /api/plugins/:id/auth `{serverName,apiKey}` stores a destination-bound server token; empty `apiKey` disconnects. POST /api/plugins/:id/oauth/start `{serverName}` starts public-client MCP OAuth. GET /api/oauth/:loginId reports pending/completed/failed status. These credentials are separate from model-provider credentials.
GET /api/settings -> Settings. POST /api/settings Settings -> Settings.
POST /api/system/open `{path?:string,url?:string}` opens selected local artifact or http(s) link using OS. POST /api/system/pick-directory -> `{path?:string}` native directory picker (desktop only, browser can enter path).
GET /api/doctor -> `{checks:[{name,ok,detail}]}`. POST /api/export -> sanitized state/config export `{path}`.

No fake provider responses in production. Empty states should guide real setup. Show genuine errors. Chat selecting plugins must filter enabled ones and persist chat selections. Native commands require approval in ask mode; read_only denies writes/commands; trusted_native grants selected-project commands. Tool execution output feeds the timeline and persists. Buttons must call these actions or be omitted.
