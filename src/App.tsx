import { lazy, Suspense, useCallback, useEffect, useState } from 'react';
import { ArrowRight, Check, CircleHelp, Code2, Folder, FolderPlus, Layers3, LoaderCircle, Menu, MessageSquare, Plus, Plug, RefreshCw, Settings2, TerminalSquare, X } from 'lucide-react';
import { api, del, enc, errorText, post } from './api';
import { Badge, Button, ErrorNotice, IconButton, Modal, PathInput } from './components';
import type { AppState, Chat, Page, Project } from './types';
import Connections from './Connections';
import Plugins from './Plugins';
import Settings from './Settings';
import Workspace from './Workspace';
const ControlPanel = lazy(() => import('./ControlPanel'));
const Studio = lazy(() => import('./Studio'));

const pageNames: Record<Page, string> = { workspace: 'Workspace', connections: 'Connections', plugins: 'Plugins', settings: 'Settings', control:'Control center',studio:'Plugin Studio' };

export default function App() {
  const [state, setState] = useState<AppState | null>(null);
  const [error, setError] = useState('');
  const [networkError, setNetworkError] = useState('');
  const [page, setPage] = useState<Page>('workspace');
  const [chatId, setChatId] = useState<string | null>(null);
  const [projectId, setProjectId] = useState('');
  const [connectionId, setConnectionId] = useState('');
  const [projectModal, setProjectModal] = useState(false);
  const [projectPath, setProjectPath] = useState('');
  const [projectName, setProjectName] = useState('');
  const [busy, setBusy] = useState('');
  const [toast, setToast] = useState('');
  const [sidebarOpen, setSidebarOpen] = useState(false);
  const [narrow, setNarrow] = useState(() => window.innerWidth <= 760);

  useEffect(() => {
    const viewport = window.matchMedia('(max-width: 760px)');
    const update = () => setNarrow(viewport.matches);
    viewport.addEventListener('change', update);
    return () => viewport.removeEventListener('change', update);
  }, []);
  useEffect(() => {
    if (!sidebarOpen || !narrow) return;
    document.querySelector<HTMLElement>('.sidebar .brand')?.focus();
    const close = (event: KeyboardEvent) => { if (event.key === 'Escape') { setSidebarOpen(false); document.querySelector<HTMLButtonElement>('.mobile-menu')?.focus(); } };
    window.addEventListener('keydown', close);
    return () => window.removeEventListener('keydown', close);
  }, [sidebarOpen, narrow]);

  const refresh = useCallback(async () => {
    const fresh = await api<AppState>('/api/state');
    setState(fresh); setNetworkError('');
    return fresh;
  }, []);

  useEffect(() => {
    let live = true;
    const load = async () => { try { const fresh = await api<AppState>('/api/state'); if (live) { setState(fresh); setNetworkError(''); } } catch (e) { if (live) setNetworkError(errorText(e)); } };
    void load();
    const timer = window.setInterval(load, 2500);
    return () => { live = false; clearInterval(timer); };
  }, []);

  useEffect(() => {
    if (!state) return;
    if (!connectionId && state.connections.length) setConnectionId(state.connections[0].id);
    if (connectionId && !state.connections.some((c) => c.id === connectionId)) setConnectionId(state.connections[0]?.id || '');
    if (projectId && !state.projects.some((p) => p.id === projectId)) setProjectId('');
    if (chatId && !state.chats.some((c) => c.id === chatId)) setChatId(null);
    const media = window.matchMedia('(prefers-color-scheme: light)');
    const setTheme = () => { document.documentElement.dataset.theme = state.settings.theme === 'system' ? (media.matches ? 'light' : 'dark') : state.settings.theme; };
    setTheme(); media.addEventListener('change', setTheme);
    return () => media.removeEventListener('change', setTheme);
  }, [state, connectionId, projectId, chatId]);
  useEffect(() => { if (!toast) return; const timer = setTimeout(() => setToast(''), 5000); return () => clearTimeout(timer); }, [toast]);

  const notify = useCallback((message: string) => setToast(message), []);
  const activeChat = state?.chats.find((c) => c.id === chatId);
  const activeProjectId = activeChat?.projectId || projectId;
  const activeProject = state?.projects.find((p) => p.id === activeProjectId);
  const filteredChats = state?.chats.filter((c) => !projectId || c.projectId === projectId).sort((a, b) => b.updatedAt.localeCompare(a.updatedAt)) || [];
  const pending = state?.approvals.filter((a) => a.status === 'pending').length || 0;

  async function newChat() {
    const connection = state?.connections.find((c) => c.id === connectionId);
    if (!connection) { setPage('connections'); notify('Connect a model to start your first task.'); return; }
    setBusy('new-chat');
    try {
      const preferredProfile = state?.profiles?.find(p=>p.connectionId===connection.id);
      const chat = await post<Chat>('/api/chats', { profileId:preferredProfile?.id, title: 'New chat', projectId: projectId || undefined, connectionId: connection.id, modelId: connection.modelId, pluginIds: [], skillIds: [], executionMode: 'ask' });
      await refresh(); setChatId(chat.id); setPage('workspace'); setSidebarOpen(false);
    } catch (e) { setError(errorText(e)); } finally { setBusy(''); }
  }
  async function addProject(event: React.FormEvent) {
    event.preventDefault(); setBusy('project');
    try { const project = await post<Project>('/api/projects', { path: projectPath.trim(), name: projectName.trim() || undefined }); await refresh(); setProjectId(project.id); setChatId(null); setProjectModal(false); setSidebarOpen(false); setProjectPath(''); setProjectName(''); setPage('workspace'); notify('Project opened.'); }
    catch (e) { setError(errorText(e)); } finally { setBusy(''); }
  }
  async function removeChat(chat: Chat) {
    if (!window.confirm(`Delete “${chat.title}” and its conversation history? Your project files will remain.`)) return;
    try { await del(`/api/chats/${enc(chat.id)}`); await refresh(); if (chatId === chat.id) setChatId(null); } catch (e) { setError(errorText(e)); }
  }

  return <div className={`app-shell ${sidebarOpen ? 'sidebar-open' : ''}`}>
    {sidebarOpen && <button className="sidebar-backdrop" aria-label="Close navigation" onClick={() => setSidebarOpen(false)} />}
    <aside className="sidebar" inert={narrow && !sidebarOpen} aria-hidden={narrow && !sidebarOpen}>
      <a className="brand" href="#" onClick={(e) => { e.preventDefault(); setPage('workspace'); }} aria-label="Workbench home"><span className="brand-mark"><Code2 size={22} strokeWidth={1.7} /></span><span>workbench<span className="brand-dot">.</span></span><Badge>LOCAL</Badge></a>
      <Button className="new-task" onClick={newChat} loading={busy === 'new-chat'}><Plus size={16} />New task<span className="key-hint">↗</span></Button>
      <nav aria-label="Main navigation" className="main-nav">
        <button className={page === 'workspace' ? 'selected' : ''} onClick={() => { setPage('workspace'); setSidebarOpen(false); }}><Layers3 size={17} />Workspace{pending > 0 && <span className="count amber">{pending}</span>}</button>
        <button className={page === 'connections' ? 'selected' : ''} onClick={() => { setPage('connections'); setSidebarOpen(false); }}><Plug size={17} />Connections<span className="count">{state?.connections.length || 0}</span></button>
        <button className={page === 'plugins' ? 'selected' : ''} onClick={() => { setPage('plugins'); setSidebarOpen(false); }}><Layers3 size={17} />Plugins<span className="count">{state?.plugins.length || 0}</span></button>
        <button className={page === 'studio' ? 'selected' : ''} onClick={() => {setPage('studio');setSidebarOpen(false);}}><Code2 size={17}/>Plugin Studio</button>
        <button className={page === 'control' ? 'selected' : ''} onClick={() => {setPage('control');setSidebarOpen(false);}}><TerminalSquare size={17}/>Control center{(state?.notifications?.filter(n=>!n.read).length||0)>0&&<span className="count amber">{state?.notifications?.filter(n=>!n.read).length}</span>}</button>
      </nav>
      <div className="sidebar-section-label"><span>PROJECTS</span><IconButton label="Open project" onClick={() => setProjectModal(true)}><Plus size={14} /></IconButton></div>
      <div className="project-list"><button className={`project-button ${!projectId ? 'active' : ''}`} onClick={() => { setProjectId(''); setChatId(null); setPage('workspace'); setSidebarOpen(false); }}><Folder size={15} /><span>All projects</span></button>{state?.projects.map((p) => <button className={`project-button ${projectId === p.id ? 'active' : ''}`} key={p.id} title={p.path} onClick={() => { setProjectId(p.id); setChatId(null); setPage('workspace'); setSidebarOpen(false); }}><Folder size={15} /><span>{p.name}</span></button>)}{state?.projects.length === 0 && <button className="sidebar-empty-link" onClick={() => setProjectModal(true)}><FolderPlus size={14} />Open a local folder</button>}</div>
      <div className="sidebar-section-label"><span>RECENT TASKS</span><span className="muted-text">{filteredChats.length}</span></div>
      <div className="chat-list">{filteredChats.length === 0 ? <p className="sidebar-hint">Your tasks will live here.<br />Start with a question or a project.</p> : filteredChats.map((chat) => <div className={`chat-item ${chat.id === chatId && page === 'workspace' ? 'active' : ''}`} key={chat.id}><button onClick={() => { setChatId(chat.id); setConnectionId(chat.connectionId); setPage('workspace'); setSidebarOpen(false); }}><MessageSquare size={14} /><span>{chat.title}</span>{state?.runs.some((r) => r.chatId === chat.id && ['running','paused'].includes(r.status)) && <span className="running-dot" />}</button><IconButton label={`Delete ${chat.title}`} onClick={() => void removeChat(chat)}><X size={12} /></IconButton></div>)}</div>
      <div className="sidebar-footer"><button className={page === 'settings' ? 'selected' : ''} onClick={() => { setPage('settings'); setSidebarOpen(false); }}><Settings2 size={16} />Settings & diagnostics</button><div className="local-status"><span className={`status-dot ${state && !networkError ? 'online' : ''}`} />{state && !networkError ? 'Local runtime connected' : 'Connecting to local runtime'}<span>{state ? `v${state.version}` : ''}</span></div></div>
    </aside>
    <main className="main-shell" inert={narrow && sidebarOpen}>
      <header className="topbar"><div className="breadcrumbs"><IconButton label="Toggle navigation" className="mobile-menu" onClick={() => setSidebarOpen(!sidebarOpen)}><Menu size={18} /></IconButton><span>{pageNames[page]}</span>{page === 'workspace' && <><span className="breadcrumb-slash">/</span><strong>{activeProject?.name || 'Personal workspace'}</strong></>}</div><div className="topbar-actions"><span className="local-only"><span className="status-dot online" />Local first</span><IconButton label="Refresh workspace state" onClick={() => void refresh().catch((e) => setError(errorText(e)))}><RefreshCw size={15} /></IconButton></div></header>
      {(error || networkError) && <div className="global-error"><ErrorNotice onDismiss={() => setError('')}>{error || networkError}</ErrorNotice></div>}
      {!state ? <div className="bootstrap"><span className="brand-mark large"><Code2 size={32} /></span><h1>Make room for good work.</h1><p>{(error || networkError) ? 'The interface is ready. Connect the local Rust runtime to continue.' : 'Connecting to your local workspace…'}</p>{!error && !networkError && <LoaderCircle className="spin" size={20} />}{(error || networkError) && <Button onClick={() => void refresh().catch((e) => setError(errorText(e)))}><RefreshCw size={15} />Try again</Button>}<span className="bootstrap-note"><TerminalSquare size={14} />Start the Workbench server or launch the desktop application.</span></div> : <>
        {page === 'workspace' && <Workspace state={state} chat={activeChat} project={activeProject} selectedConnection={connectionId} onConnectionChange={setConnectionId} onCreateChat={newChat} creating={busy === 'new-chat'} onOpenProject={() => setProjectModal(true)} onConnections={() => setPage('connections')} refresh={refresh} notify={notify} onSelectChat={setChatId} />}
        {page === 'connections' && <Connections state={state} refresh={refresh} notify={notify} />}
        {page === 'plugins' && <Plugins state={state} refresh={refresh} notify={notify} />}
        {page === 'settings' && <Settings state={state} refresh={refresh} notify={notify} />}
        <Suspense fallback={<div className="bootstrap"><LoaderCircle className="spin"/>Loading workspace tools…</div>}>{page === 'control' && <ControlPanel state={state} refresh={refresh} notify={notify} onOpenChat={id=>{setChatId(id);setPage('workspace');}}/>}
        {page === 'studio' && <Studio state={state} refresh={refresh} notify={notify}/>}</Suspense>
      </>}
      <footer className="statusbar"><span><Code2 size={12} />WORKBENCH<span className="statusbar-divider">/</span>{page === 'workspace' ? activeChat?.modelId || 'Choose your model. Own your workflow.' : pageNames[page]}</span><span><CircleHelp size={12} />Tools run through your local permission policy</span></footer>
    </main>
    {toast && <div className="toast" role="status"><Check size={16} /><span>{toast}</span><IconButton label="Dismiss notification" onClick={() => setToast('')}><X size={14} /></IconButton></div>}
    {projectModal && <Modal title="Open a project" subtitle="Your files stay on this computer. Choose the folder this task can work in." onClose={() => setProjectModal(false)}><form className="form-body" onSubmit={addProject}>{error && <ErrorNotice onDismiss={() => setError('')}>{error}</ErrorNotice>}<label className="field"><span>Project folder</span><PathInput value={projectPath} onChange={setProjectPath} placeholder="D:\Projects\my-app" /></label><label className="field"><span>Display name <small>optional</small></span><input value={projectName} onChange={(e) => setProjectName(e.target.value)} placeholder="Use the folder name" /></label><div className="modal-actions"><Button type="button" onClick={() => setProjectModal(false)}>Cancel</Button><Button tone="primary" type="submit" disabled={!projectPath.trim()} loading={busy === 'project'}>Open project<ArrowRight size={15} /></Button></div></form></Modal>}
  </div>;
}
