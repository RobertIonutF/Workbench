import { useCallback, useEffect, useRef, useState } from 'react';
import Markdown from 'react-markdown';
import { ArrowLeft, ArrowRight, ArrowUp, Check, ChevronRight, Code2, File, FileCode2, Files, Folder, FolderOpen, GitBranch, GitCompareArrows, LoaderCircle, MoreHorizontal, Pencil, Plus, RefreshCw, Save, Shield, ShieldAlert, Square, Terminal, Undo2, X } from 'lucide-react';
import { api, enc, errorText, openPath, openUrl, post, put, query } from './api';
import { Badge, Button, Empty, ErrorNotice, IconButton, Select } from './components';
import type { AppState, Chat, ExecutionMode, FileEntry, Message, Project, Run, RunEvent } from './types';

type Props = { state: AppState; chat?: Chat; project?: Project; selectedConnection: string; onConnectionChange: (id: string) => void; onCreateChat: () => Promise<void>; creating: boolean; onOpenProject: () => void; onConnections: () => void; refresh: () => Promise<AppState>; notify: (s: string) => void };
const modeLabels: Record<ExecutionMode, string> = { ask: 'Ask before execution', read_only: 'Read only', trusted_native: 'Trusted native', docker: 'Docker isolation' };
const displayData = (data: unknown) => typeof data === 'string' ? data : JSON.stringify(data, null, 2);
const textOfDelta = (event: RunEvent) => event.data && typeof event.data === 'object' && 'text' in event.data ? String(event.data.text) : '';

export default function Workspace({ state, chat, project, selectedConnection, onConnectionChange, onCreateChat, creating, onOpenProject, onConnections, refresh, notify }: Props) {
  const [messages, setMessages] = useState<Message[]>([]);
  const [events, setEvents] = useState<RunEvent[]>([]);
  const [text, setText] = useState('');
  const [error, setError] = useState('');
  const [sending, setSending] = useState(false);
  const [pollKey, setPollKey] = useState(0);
  const [options, setOptions] = useState(false);
  const [rename, setRename] = useState(false);
  const [title, setTitle] = useState('');
  const [modelDraft, setModelDraft] = useState(chat?.modelId || '');
  const [reviewOpen, setReviewOpen] = useState(() => window.innerWidth > 1020);
  const bottom = useRef<HTMLDivElement>(null);
  const scroll = useRef<HTMLDivElement>(null);
  const following = useRef(true);
  const input = useRef<HTMLTextAreaElement>(null);
  const activeRun = state.runs.find((r) => r.chatId === chat?.id && r.status === 'running');
  const latestRun = state.runs.filter((r) => r.chatId === chat?.id).sort((a, b) => b.createdAt.localeCompare(a.createdAt))[0];
  const connection = state.connections.find((c) => c.id === (chat?.connectionId || selectedConnection));
  const pendingApprovals = state.approvals.filter((a) => a.chatId === chat?.id && a.status === 'pending');
  const enabledPlugins = state.plugins.filter((p) => p.enabled);

  useEffect(() => { setMessages([]); setEvents([]); setText(''); setError(''); setOptions(false); setTitle(chat?.title || ''); following.current = true; }, [chat?.id]);
  useEffect(() => { setModelDraft(chat?.modelId || ''); }, [chat?.id, chat?.modelId]);
  useEffect(() => {
    if (!chat) return;
    let disposed = false; let cursor = 0; let inFlight = false;
    const controller = new AbortController();
    const load = async () => {
      if (inFlight) return; inFlight = true;
      try {
        const [nextMessages, result] = await Promise.all([
          api<Message[]>(`/api/chats/${enc(chat.id)}/messages`, { signal: controller.signal }),
          api<{ events: RunEvent[]; cursor: number }>(`/api/events?${query({ after: String(cursor), chatId: chat.id })}`, { signal: controller.signal }),
        ]);
        if (disposed) return;
        setMessages(nextMessages);
        if (result.events.length) setEvents((old) => { const ids = new Set(old.map((e) => e.id)); return [...old, ...result.events.filter((e) => !ids.has(e.id))]; });
        cursor = result.cursor;
      } catch (e) { if (!disposed) setError(errorText(e)); }
      finally { inFlight = false; }
    };
    void load(); const timer = setInterval(load, activeRun ? 850 : 3000);
    return () => { disposed = true; clearInterval(timer); controller.abort(); };
  }, [chat?.id, activeRun?.id, pollKey]);
  useEffect(() => { if (following.current) bottom.current?.scrollIntoView({ block: 'end', behavior: 'auto' }); }, [messages, events]);
  useEffect(() => { if (!activeRun) setPollKey((n) => n + 1); }, [activeRun?.id]);

  async function patchChat(changes: Partial<Chat>) {
    if (!chat) return;
    try { await api<Chat>(`/api/chats/${enc(chat.id)}`, { method: 'PATCH', body: changes }); await refresh(); }
    catch (e) { setError(errorText(e)); }
  }
  async function send(event?: React.FormEvent) {
    event?.preventDefault(); if (!chat || !text.trim() || sending || activeRun) return;
    setSending(true); setError('');
    try { await post<Run>(`/api/chats/${enc(chat.id)}/turns`, { text: text.trim() }); setText(''); following.current = true; setPollKey((n) => n + 1); await refresh(); input.current?.focus(); }
    catch (e) { setError(errorText(e)); } finally { setSending(false); }
  }
  async function cancel() { if (!activeRun) return; try { await post(`/api/runs/${enc(activeRun.id)}/cancel`); notify('Stop requested. Waiting for the runtime to confirm.'); await refresh(); } catch (e) { setError(errorText(e)); } }
  async function decide(id: string, decision: 'allow_once' | 'deny') { try { await post(`/api/approvals/${enc(id)}`, { decision }); await refresh(); } catch (e) { setError(errorText(e)); } }

  const allDeltas = activeRun && !events.some((e) => e.runId === activeRun.id && ['run_complete', 'run_error'].includes(e.kind)) ? events.filter((e) => e.runId === activeRun.id && e.kind === 'text_delta').map(textOfDelta).join('') : '';
  const persistedThisRun = activeRun ? messages.filter((m) => m.role === 'assistant' && m.createdAt >= activeRun.createdAt).map((m) => m.content).join('') : '';
  // A run can persist several assistant messages between tools. Render only the
  // remaining stream suffix so a history refresh never repeats those messages.
  const streamed = persistedThisRun && allDeltas.startsWith(persistedThisRun) ? allDeltas.slice(persistedThisRun.length) : allDeltas;
  const timelineEvents = events.filter((e) => !['text_delta', 'run_complete'].includes(e.kind));

  return <div className={`workspace ${reviewOpen ? '' : 'review-collapsed'}`}>
    <section className="conversation-pane">
      <div className="conversation-header"><div className="task-heading">{rename && chat ? <form onSubmit={(e) => { e.preventDefault(); void patchChat({ title: title.trim() || chat.title }); setRename(false); }}><input aria-label="Task title" value={title} onChange={(e) => setTitle(e.target.value)} autoFocus /><IconButton label="Save task title" type="submit"><Check size={15} /></IconButton></form> : <><span className="task-status"><span className={`status-dot ${activeRun ? 'working' : ''}`} /></span><h1>{chat?.title || 'A little space to build.'}</h1>{chat && <IconButton label="Rename task" onClick={() => { setTitle(chat.title); setRename(true); }}><Pencil size={13} /></IconButton>}</>}</div><div className="conversation-actions">{chat && <Badge kind={activeRun ? 'warning' : latestRun?.status === 'failed' ? 'danger' : ''}>{activeRun ? 'Working' : latestRun?.status || 'Ready'}</Badge>}<IconButton label="Task options" onClick={() => setOptions(!options)} aria-expanded={options}><MoreHorizontal size={18} /></IconButton><IconButton label={reviewOpen ? 'Hide file review' : 'Show file review'} onClick={() => setReviewOpen(!reviewOpen)}><Files size={17} /></IconButton></div></div>
      {options && <div className="task-options"><div className="task-options-heading"><span className="section-label">TASK CONFIGURATION</span><IconButton label="Close task options" onClick={() => setOptions(false)}><X size={14} /></IconButton></div><form className="field task-model-form" onSubmit={(e) => { e.preventDefault(); void patchChat({ modelId: modelDraft.trim() }); }}><label htmlFor="task-model-id">Model ID for this task</label><div className="input-action"><input id="task-model-id" value={modelDraft} onChange={(e) => setModelDraft(e.target.value)} placeholder="Exact model ID" disabled={!chat || !!activeRun} spellCheck={false} /><Button type="submit" disabled={!chat || !!activeRun || !modelDraft.trim() || modelDraft === chat.modelId}>Apply</Button></div></form><label className="field"><span>Execution policy</span><Select value={chat?.executionMode || 'ask'} disabled={!chat || !!activeRun} onChange={(e) => { const executionMode = e.target.value as ExecutionMode; if (executionMode === 'trusted_native' && !confirm('Trusted native commands and MCP processes run with your Windows user permissions. They can access user files and credentials. Enable for this task?')) return; void patchChat({ executionMode }); }}>{Object.entries(modeLabels).map(([id, label]) => <option key={id} value={id}>{label}</option>)}</Select></label><div className="field"><span>Plugins for this task</span>{!chat ? <p className="field-hint">Create a task before selecting plugins.</p> : enabledPlugins.length === 0 ? <p className="field-hint">Enable a plugin in the Plugins library to use its skills and tools here.</p> : <div className="plugin-choice-list">{enabledPlugins.map((plugin) => <div key={plugin.id}><label className="check-row"><input type="checkbox" disabled={!!activeRun} checked={chat.pluginIds.includes(plugin.id)} onChange={(e) => void patchChat({ pluginIds: e.target.checked ? [...chat.pluginIds, plugin.id] : chat.pluginIds.filter((id) => id !== plugin.id), skillIds: e.target.checked ? chat.skillIds : chat.skillIds.filter((id) => !id.startsWith(`${plugin.id}:`)) })} /><span>{plugin.name}<small>{plugin.skills.length} skills · {plugin.servers.length} MCP servers</small></span></label>{chat.pluginIds.includes(plugin.id) && plugin.skills.map((skill) => <label className="check-row skill-choice" key={skill.id}><input type="checkbox" disabled={!!activeRun} checked={chat.skillIds.includes(skill.id)} onChange={(e) => void patchChat({ skillIds: e.target.checked ? [...chat.skillIds, skill.id] : chat.skillIds.filter((id) => id !== skill.id) })} /><span>{skill.name}</span></label>)}</div>)}</div>}</div></div>}
      {error && <ErrorNotice onDismiss={() => setError('')}>{error}</ErrorNotice>}
      {chat ? <>
      <div className="conversation-scroll" ref={scroll} onScroll={() => { if (scroll.current) following.current = scroll.current.scrollHeight - scroll.current.scrollTop - scroll.current.clientHeight < 140; }}>
        {messages.length === 0 && !activeRun && <div className="task-welcome"><span className="task-welcome-symbol"><Terminal size={25} /></span><h2>What are we working on?</h2><p>{project ? <>Ask about <strong>{project.name}</strong>, fix a bug, or describe the next thing to build.</> : 'Ask a question, or open a project to work with local files.'}</p><div className="context-line"><span><Code2 size={13} />{chat.modelId || 'No model selected'}</span><span><Shield size={13} />{modeLabels[chat.executionMode]}</span></div></div>}
        {messages.map((message) => <MessageView key={message.id} message={message} />)}
        {timelineEvents.length > 0 && <details className="tool-timeline"><summary><Terminal size={14} /><span>Execution timeline</span><Badge>{timelineEvents.length}</Badge><ChevronRight size={13} /></summary><div>{timelineEvents.map((event) => <details className={`timeline-event ${event.kind === 'run_error' ? 'event-error' : ''}`} key={event.id}><summary><span className="event-point" /><strong>{event.kind.replaceAll('_', ' ')}</strong><span>{new Date(event.createdAt).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}</span></summary><pre>{displayData(event.data)}</pre></details>)}</div></details>}
        {streamed && <div className="message assistant"><div className="message-label"><span className="assistant-icon"><Code2 size={13} /></span>Workbench<span className="stream-label"><span className="running-dot" />Thinking with {chat.modelId}</span></div><SafeMarkdown>{streamed}</SafeMarkdown><span className="typing-caret" /></div>}
        {activeRun && !streamed && <div className="thinking" role="status"><span className="thinking-dots"><i /><i /><i /></span>{pendingApprovals.length ? 'Waiting for your decision' : 'Working on your task…'}</div>}
        {latestRun && ['failed', 'interrupted'].includes(latestRun.status) && latestRun.error && <ErrorNotice>{latestRun.error}</ErrorNotice>}
        {pendingApprovals.map((approval) => <div className="approval-card" key={approval.id}><div><ShieldAlert size={21} /><span><strong>{approval.title}</strong><small>This action needs your permission.</small></span></div><pre>{displayData(approval.details)}</pre><div className="approval-actions"><Button onClick={() => void decide(approval.id, 'deny')}>Deny</Button><Button tone="primary" onClick={() => void decide(approval.id, 'allow_once')}>Allow once<ArrowRight size={14} /></Button></div></div>)}
        <div ref={bottom} />
      </div>
      <div className="composer-area"><form className={`composer ${activeRun ? 'is-running' : ''}`} onSubmit={send}><textarea ref={input} aria-label="Message to Workbench" value={text} onChange={(e) => setText(e.target.value)} placeholder={activeRun ? 'A task is running. You can draft your next message…' : 'Describe the task. Be as specific as you like.'} rows={3} onKeyDown={(e) => { if (e.key === 'Enter' && !e.shiftKey && !e.nativeEvent.isComposing) { e.preventDefault(); void send(); } }} /><div className="composer-toolbar"><div className="composer-left"><ModelPicker state={state} connectionId={chat.connectionId} modelId={chat.modelId} disabled={!!activeRun || sending} onChange={(id) => { const c = state.connections.find((v) => v.id === id); if (c) { onConnectionChange(id); void patchChat({ connectionId: id, modelId: c.modelId }); } }} /><button type="button" className="mode-button" onClick={() => setOptions(!options)}><Shield size={13} />{chat.executionMode === 'ask' ? 'Ask' : chat.executionMode === 'read_only' ? 'Read only' : chat.executionMode === 'docker' ? 'Docker' : 'Trusted'}{chat.pluginIds.length > 0 && <span>· {chat.pluginIds.length} plugins</span>}</button></div>{activeRun ? <IconButton label="Stop task" className="send-button stop" onClick={cancel} type="button"><Square size={14} fill="currentColor" /></IconButton> : <IconButton label="Send message" className="send-button" type="submit" disabled={!text.trim() || sending || !chat.modelId}>{sending ? <LoaderCircle className="spin" size={17} /> : <ArrowUp size={18} />}</IconButton>}</div></form><div className="composer-caption"><span>↵ to send<span className="caption-separator">·</span>Shift ↵ for a new line</span><span>{project ? <><Folder size={11} />{project.name}</> : 'No project attached'}</span></div></div>
      </> : <div className="welcome"><div className="welcome-kicker"><span className="small-rule" />YOUR NEXT GOOD IDEA STARTS HERE</div><h2>Less between you<br />and <em>the work.</em></h2><p>A local workspace for your code, your models,<br className="desktop-break" /> and the way you like to build.</p><div className="welcome-actions"><Button tone="primary" onClick={onCreateChat} loading={creating}><Plus size={16} />Start a task<ArrowRight size={16} /></Button><Button onClick={onOpenProject}><FolderOpen size={16} />Open project</Button></div><div className="welcome-setup"><div><span className={`setup-number ${state.connections.length ? 'done' : ''}`}>{state.connections.length ? <Check size={13} /> : '01'}</span><span><strong>Choose your engine</strong><small>{state.connections.length ? `${state.connections.length} model connection${state.connections.length === 1 ? '' : 's'} ready to select` : 'Connect an API or a local model'}</small></span><Button tone="ghost" onClick={onConnections}>{state.connections.length ? 'Manage' : 'Connect'}<ArrowRight size={13} /></Button></div><div><span className={`setup-number ${project ? 'done' : ''}`}>{project ? <Check size={13} /> : '02'}</span><span><strong>Bring a project</strong><small>{project?.path || 'Point Workbench at a folder on your computer'}</small></span><Button tone="ghost" onClick={onOpenProject}>{project ? 'Change' : 'Open'}<ArrowRight size={13} /></Button></div></div><div className="welcome-connection"><ModelPicker state={state} connectionId={selectedConnection} modelId={connection?.modelId || ''} onChange={onConnectionChange} /><span><Shield size={12} />Ask before execution</span></div></div>}
    </section>
    {reviewOpen && <ReviewPane project={project} chat={chat} refreshKey={`${latestRun?.id}-${latestRun?.status}`} onOpenProject={onOpenProject} onClose={() => setReviewOpen(false)} notify={notify} />}
  </div>;
}

function ModelPicker({ state, connectionId, modelId, onChange, disabled }: { state: AppState; connectionId: string; modelId: string; onChange: (id: string) => void; disabled?: boolean }) {
  return <div className="model-picker"><span className="model-dot" /><Select aria-label="Model connection" value={connectionId} disabled={disabled || state.connections.length === 0} onChange={(e) => onChange(e.target.value)}>{state.connections.length === 0 && <option value="">No model connected</option>}{state.connections.map((c) => <option key={c.id} value={c.id}>{c.label} · {c.id === connectionId ? modelId || c.modelId || 'Choose model ID' : c.modelId || 'Choose model ID'}</option>)}</Select></div>;
}
function SafeMarkdown({ children }: { children: string }) {
  const [error, setError] = useState('');
  const link = (href: string | undefined, content: React.ReactNode) => href && /^https?:\/\//i.test(href)
    ? <a href={href} onClick={(event) => { event.preventDefault(); void openUrl(href).catch((e) => setError(errorText(e))); }} title={href}>{content}</a>
    : <span title={href}>{content}</span>;
  return <div className="markdown"><Markdown components={{ a: ({ href, children: content }) => link(href, content), img: ({ src, alt }) => <span className="markdown-image-link">{link(typeof src === 'string' ? src : undefined, alt || 'Open image')}</span> }}>{children}</Markdown>{error && <ErrorNotice onDismiss={() => setError('')}>{error}</ErrorNotice>}</div>;
}
function MessageView({ message }: { message: Message }) {
  if (message.role === 'tool') return <details className="tool-message"><summary><Terminal size={14} /><strong>{message.toolName || 'Tool result'}</strong><ChevronRight size={13} /></summary><pre>{message.content}</pre></details>;
  return <article className={`message ${message.role}`}><div className="message-label">{message.role === 'assistant' ? <><span className="assistant-icon"><Code2 size={13} /></span>Workbench</> : <><span className="user-avatar">Y</span>You</>}<time>{new Date(message.createdAt).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}</time></div><SafeMarkdown>{message.content}</SafeMarkdown>{message.toolCalls && message.toolCalls.length > 0 && <details className="message-tool-calls"><summary>{message.toolCalls.length} tool request{message.toolCalls.length === 1 ? '' : 's'}</summary><pre>{displayData(message.toolCalls)}</pre></details>}</article>;
}

function ReviewPane({ project, chat, refreshKey, onOpenProject, onClose, notify }: { project?: Project; chat?: Chat; refreshKey: string; onOpenProject: () => void; onClose: () => void; notify: (s: string) => void }) {
  const [tab, setTab] = useState<'files' | 'diff'>('files');
  const [path, setPath] = useState('');
  const [entries, setEntries] = useState<FileEntry[]>([]);
  const [selectedFile, setSelectedFile] = useState('');
  const [content, setContent] = useState('');
  const [saved, setSaved] = useState('');
  const [diff, setDiff] = useState('');
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  const [editing, setEditing] = useState(false);

  const load = useCallback(async () => {
    if (!project) return;
    setBusy(true); setError('');
    try { if (tab === 'files') { const result = await api<{ entries: FileEntry[]; path: string }>(`/api/workspace/files?${query({ projectId: project.id, path })}`); setEntries(result.entries.sort((a, b) => Number(b.isDir) - Number(a.isDir) || a.name.localeCompare(b.name))); } else { const result = await api<{ diff: string }>(`/api/workspace/diff?${query({ projectId: project.id })}`); setDiff(result.diff); } } catch (e) { setError(errorText(e)); } finally { setBusy(false); }
  }, [project?.id, path, tab]);
  useEffect(() => { setPath(''); setSelectedFile(''); setContent(''); setSaved(''); setEditing(false); }, [project?.id]);
  useEffect(() => { void load(); }, [load, refreshKey]);
  const discardDraft = () => content === saved || confirm('Discard your unsaved file edits?');
  async function openFile(entry: FileEntry) {
    if (!discardDraft()) return;
    if (entry.isDir) { setPath(entry.path); setSelectedFile(''); setContent(''); setSaved(''); return; }
    setBusy(true); setError('');
    try { const result = await api<{ path: string; content: string }>(`/api/workspace/file?${query({ projectId: project!.id, path: entry.path })}`); setSelectedFile(result.path); setContent(result.content); setSaved(result.content); setEditing(false); } catch (e) { setError(errorText(e)); } finally { setBusy(false); }
  }
  async function saveFile() {
    setBusy(true); setError('');
    try { await put(`/api/workspace/file?${query({ projectId: project!.id, path: selectedFile })}`, { content }); setSaved(content); setEditing(false); notify('File saved.'); } catch (e) { setError(errorText(e)); } finally { setBusy(false); }
  }
  async function undo(filePath: string) {
    if (!chat || !confirm(`Revert the agent’s recorded changes to ${filePath}? Changes that conflict with your edits will be left for review.`)) return;
    setBusy(true); setError('');
    try { await post('/api/workspace/undo', { chatId: chat.id, path: filePath }); await load(); notify('Agent changes reverted.'); } catch (e) { setError(errorText(e)); } finally { setBusy(false); }
  }

  return <aside className="review-pane"><div className="review-header"><span><GitBranch size={15} />Project review</span><div className="review-header-actions"><IconButton label="Refresh project files and changes" onClick={() => void load()} disabled={busy || !project}><RefreshCw size={14} className={busy ? 'spin' : ''} /></IconButton><IconButton label="Close project review" onClick={onClose}><X size={14} /></IconButton></div></div><div className="review-tabs" role="tablist" aria-label="Project review"><button role="tab" aria-selected={tab === 'files'} className={tab === 'files' ? 'active' : ''} onClick={() => setTab('files')}><Files size={14} />Files</button><button role="tab" aria-selected={tab === 'diff'} className={tab === 'diff' ? 'active' : ''} onClick={() => setTab('diff')}><GitCompareArrows size={15} />Changes</button></div>{error && <ErrorNotice onDismiss={() => setError('')}>{error}</ErrorNotice>}{!project ? <Empty icon={<FolderOpen size={25} />} title="A project belongs here." action={<Button onClick={onOpenProject}><Plus size={14} />Open project</Button>}>Open a local folder to browse files and review the work as it happens.</Empty> : <>
      <div className="review-project"><Folder size={13} /><span title={project.path}>{project.name}</span><IconButton label="Open project in file manager" onClick={() => void openPath(project.path).catch((e) => setError(errorText(e)))}><ArrowUp size={13} className="rotate-45" /></IconButton></div>
      {tab === 'files' ? <>{selectedFile ? <div className="file-preview"><div className="file-preview-heading"><IconButton label="Back to file list" onClick={() => { if (discardDraft()) { setSelectedFile(''); setContent(''); setSaved(''); } }}><ArrowLeft size={14} /></IconButton><span title={selectedFile}>{selectedFile.split(/[\\/]/).pop()}</span><IconButton label={editing ? 'Preview file' : 'Edit file'} onClick={() => setEditing(!editing)}><Pencil size={14} /></IconButton></div>{editing ? <textarea className="file-editor" aria-label={`Edit ${selectedFile}`} value={content} onChange={(e) => setContent(e.target.value)} spellCheck={false} /> : <div className="source-view">{content.split('\n').map((line, i) => <div key={i}><span>{i + 1}</span><code>{line || ' '}</code></div>)}</div>}{content !== saved && <div className="file-save"><span>Unsaved changes</span><Button tone="primary" onClick={saveFile} loading={busy}><Save size={13} />Save</Button></div>}</div> : <div className="file-tree">{path && <button className="file-entry parent" onClick={() => setPath(path.replace(/[\\/][^\\/]+[\\/]?$/, '').replace(/^[^\\/]+$/, ''))}><ArrowLeft size={14} /><span>Parent folder</span></button>}{path && <div className="tree-path">{path}</div>}{entries.length === 0 && !busy && <p className="tree-empty">No files in this folder.</p>}{entries.map((entry) => <button className="file-entry" key={entry.path} onClick={() => void openFile(entry)} title={entry.path}>{entry.isDir ? <Folder size={14} className="folder-icon" /> : <FileCode2 size={14} />}<span>{entry.name}</span>{entry.isDir ? <ChevronRight size={12} /> : <small>{entry.size > 1024 ? `${Math.ceil(entry.size / 1024)}k` : `${entry.size}b`}</small>}</button>)}</div>}</> : <div className="diff-scroll">{diff ? <DiffView diff={diff} onUndo={chat ? undo : undefined} /> : <Empty icon={<Check size={24} />} title="No tracked changes">Run a task or edit a file, then refresh to see its diff.</Empty>}</div>}
      <div className="review-note"><Shield size={12} /><span>Review changes before sharing or committing.</span></div>
    </>}</aside>;
}
function DiffView({ diff, onUndo }: { diff: string; onUndo?: (path: string) => void }) {
  return <div className="diff-view">{diff.split('\n').map((line, index) => {
    if (line.startsWith('diff --git ')) return null;
    if (line.startsWith('+++ ')) { const path = line.slice(4).replace(/^b\//, '').split('\t')[0]; return <div className="diff-file" key={index}><File size={13} /><span>{path}</span>{onUndo && path !== '/dev/null' && <IconButton label={`Revert agent edits to ${path}`} onClick={() => onUndo(path)}><Undo2 size={13} /></IconButton>}</div>; }
    if (line.startsWith('--- ') || line.startsWith('index ')) return null;
    return <div key={index} className={`diff-line ${line.startsWith('@@') ? 'hunk' : line.startsWith('+') ? 'addition' : line.startsWith('-') ? 'deletion' : 'context'}`}><code>{line || ' '}</code></div>;
  })}</div>;
}
