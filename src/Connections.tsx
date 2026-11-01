import { useEffect, useState } from 'react';
import { ArrowRight, ChevronRight, ExternalLink, KeyRound, Laptop, Plus, Radio, Save, ShieldCheck, Sparkles, Trash2, Unplug, Zap } from 'lucide-react';
import { api, del, enc, errorText, openUrl, post } from './api';
import { Badge, Button, Diagnostics, ErrorNotice, IconButton, Select, Status } from './components';
import type { AppState, Connection, Model, Probe } from './types';

type Props = { state: AppState; refresh: () => Promise<AppState>; notify: (s: string) => void };
type Draft = Pick<Connection, 'label' | 'preset' | 'protocol' | 'baseUrl' | 'modelId' | 'contextWindow' | 'tools' | 'authKind'> & { apiKey: string; removeSecret: boolean };
const presets: Record<string, { label: string; protocol: Connection['protocol']; baseUrl: string; authKind: Connection['authKind'] }> = {
  lmstudio: { label: 'LM Studio', protocol: 'openai_chat', baseUrl: 'http://127.0.0.1:1234/v1', authKind: 'none' },
  openai: { label: 'OpenAI', protocol: 'openai_responses', baseUrl: 'https://api.openai.com/v1', authKind: 'api_key' },
  anthropic: { label: 'Anthropic', protocol: 'anthropic', baseUrl: 'https://api.anthropic.com/v1', authKind: 'api_key' },
  ollama: { label: 'Ollama', protocol: 'openai_chat', baseUrl: 'http://127.0.0.1:11434/v1', authKind: 'none' },
  gemini: { label: 'Google Gemini', protocol: 'gemini', baseUrl: 'https://generativelanguage.googleapis.com/v1beta', authKind: 'api_key' },
  custom: { label: 'Custom endpoint', protocol: 'openai_chat', baseUrl: '', authKind: 'api_key' },
};
const blank = (): Draft => ({ ...presets.lmstudio, preset: 'lmstudio', modelId: '', contextWindow: 8192, tools: false, apiKey: '', removeSecret: false });

export default function Connections({ state, refresh, notify }: Props) {
  const [selected, setSelected] = useState<string | null>(state.connections[0]?.id || null);
  const [draft, setDraft] = useState<Draft>(blank);
  const [busy, setBusy] = useState('');
  const [error, setError] = useState('');
  const [models, setModels] = useState<Model[]>([]);
  const [diagnostics, setDiagnostics] = useState<string[]>([]);
  const [probe, setProbe] = useState<Probe | null>(null);
  const [advanced, setAdvanced] = useState(false);
  const [oauth, setOauth] = useState<{ loginId: string; url: string; status: string } | null>(null);
  const current = state.connections.find((c) => c.id === selected);

  useEffect(() => {
    const conn = state.connections.find((c) => c.id === selected);
    setDraft(conn ? { label: conn.label, preset: conn.preset, protocol: conn.protocol, baseUrl: conn.baseUrl, modelId: conn.modelId, contextWindow: conn.contextWindow, tools: conn.tools, authKind: conn.authKind, apiKey: '', removeSecret: false } : blank());
    setModels([]); setProbe(null); setDiagnostics([]); setError('');
  }, [selected]); // Keep unsaved edits when global state refreshes.+
  useEffect(() => {
    if (!oauth || oauth.status !== 'pending') return;
    let mounted = true;
    const timer = setInterval(async () => {
      try {
        const result = await api<{ status: string; error?: string }>(`/api/oauth/${enc(oauth.loginId)}`);
        if (!mounted) return;
        if (result.error) { setError(result.error); setOauth({ ...oauth, status: 'failed' }); return; }
        if (['completed', 'success', 'complete'].includes(result.status)) { setOauth({ ...oauth, status: 'completed' }); const fresh = await refresh(); const latest = fresh.connections.find((c) => c.authKind === 'openai_oauth'); if (latest) setSelected(latest.id); notify('ChatGPT connection added.'); }
        else if (['failed', 'error', 'canceled', 'expired'].includes(result.status)) setOauth({ ...oauth, status: result.status });
      } catch (e) { if (mounted) { setError(errorText(e)); setOauth({ ...oauth, status: 'failed' }); } }
    }, 1800);
    return () => { mounted = false; clearInterval(timer); };
  }, [oauth, refresh, notify]);

  const change = <K extends keyof Draft>(key: K, value: Draft[K]) => setDraft((v) => ({ ...v, [key]: value }));
  async function save(): Promise<Connection> {
    const connection = await post<Connection>('/api/connections', { ...draft, id: selected || undefined, label: draft.label.trim(), baseUrl: draft.baseUrl.trim(), modelId: draft.modelId.trim() });
    await refresh();
    if (!selected) setSelected(connection.id);
    setDraft((v) => ({ ...v, apiKey: '', removeSecret: false }));
    return connection;
  }
  async function act(action: 'save' | 'discover' | 'probe') {
    setBusy(action); setError('');
    try {
      const conn = await save();
      if (action === 'discover') { const result = await post<{ models: Model[]; diagnostics: string[] }>(`/api/connections/${enc(conn.id)}/models`); setModels(result.models); setDiagnostics(result.diagnostics); if (!result.models.length) notify('No models listed. You can still enter an exact model ID.'); }
      else if (action === 'probe') { const result = await post<Probe>(`/api/connections/${enc(conn.id)}/probe`, { modelId: draft.modelId }); setProbe(result); setDraft((value) => ({ ...value, tools: result.tools })); await refresh(); }
      else notify('Connection saved.');
    } catch (e) { setError(errorText(e)); } finally { setBusy(''); }
  }
  async function remove() {
    if (!selected || !confirm('Remove this model connection? Existing tasks will keep their history.')) return;
    setBusy('delete'); try { await del(`/api/connections/${enc(selected)}`); const fresh = await refresh(); setSelected(fresh.connections[0]?.id || null); notify('Connection removed.'); } catch (e) { setError(errorText(e)); } finally { setBusy(''); }
  }
  async function startOAuth() {
    setBusy('oauth'); setError('');
    try { const result = await post<{ url: string; loginId: string }>('/api/oauth/openai/start'); setOauth({ ...result, status: 'pending' }); await openUrl(result.url); }
    catch (e) { setError(errorText(e)); } finally { setBusy(''); }
  }

  return <section className="page connections-page">
    <div className="page-heading"><div><span className="eyebrow">YOUR MODELS, YOUR CHOICE</span><h1>Connections<span className="heading-dot">.</span></h1><p>Bring an API, a subscription connection, or a model running down the hall.</p></div><Button onClick={() => { setSelected(null); setDraft(blank()); setError(''); setModels([]); setProbe(null); }}><Plus size={16} />Add connection</Button></div>
    <div className="connections-layout"><aside className="connection-list"><div className="section-label">SAVED CONNECTIONS<span>{state.connections.length}</span></div>{state.connections.length === 0 ? <div className="list-empty"><Unplug size={24} /><p>No connections yet.<br />Set up your first model here.</p></div> : state.connections.map((c) => <button key={c.id} className={`connection-card ${selected === c.id ? 'active' : ''}`} onClick={() => setSelected(c.id)}><span className="provider-icon">{['lmstudio', 'ollama'].includes(c.preset) ? <Laptop size={19} /> : <Sparkles size={19} />}</span><span><strong>{c.label}</strong><small>{c.modelId || 'Choose a model'}</small></span><ChevronRight size={14} /></button>)}
      <div className="oauth-card"><span className="oauth-symbol"><Sparkles size={19} /></span><h3>Continue with ChatGPT</h3><p>Connect an eligible ChatGPT plan using the official sign-in flow.</p><Button onClick={startOAuth} loading={busy === 'oauth'} disabled={oauth?.status === 'pending'}>Sign in with ChatGPT<ExternalLink size={13} /></Button>{oauth && <div className="oauth-status"><Status ok={oauth.status === 'completed'}>{oauth.status === 'pending' ? 'Waiting for browser sign-in…' : `Sign-in ${oauth.status}`}</Status>{oauth.status === 'pending' && <Button tone="ghost" onClick={() => void openUrl(oauth.url).catch((e) => setError(errorText(e)))}>Reopen sign-in<ExternalLink size={12} /></Button>}</div>}<small>Availability depends on your account and the provider’s current OAuth rules.</small></div>
    </aside>
    <div className="connection-editor panel"><div className="panel-heading"><div><span className="eyebrow">{current ? 'CONNECTION SETTINGS' : 'NEW CONNECTION'}</span><h2>{current?.label || 'Give your next model a home.'}</h2></div>{current && <IconButton label="Remove connection" onClick={remove} disabled={!!busy}><Trash2 size={16} /></IconButton>}</div>
      {error && <ErrorNotice onDismiss={() => setError('')}>{error}</ErrorNotice>}
      <form onSubmit={(e) => { e.preventDefault(); void act('save'); }}><div className="form-grid"><label className="field"><span>Provider preset</span><Select value={draft.preset} onChange={(e) => { const preset = e.target.value; setDraft((v) => ({ ...v, ...presets[preset], preset, modelId: '', apiKey: '', removeSecret: false })); setProbe(null); setModels([]); }}>{Object.entries(presets).map(([value, p]) => <option key={value} value={value}>{p.label}</option>)}</Select></label><label className="field"><span>Connection name</span><input required value={draft.label} onChange={(e) => change('label', e.target.value)} placeholder="My local coder" /></label><label className="field full"><span>API base URL</span><input required type="url" value={draft.baseUrl} onChange={(e) => change('baseUrl', e.target.value)} placeholder="http://127.0.0.1:1234/v1" autoComplete="off" spellCheck={false} /><small className="field-hint">The exact service your requests will go to. Credentials are scoped to this connection.</small></label><label className="field"><span>API protocol</span><Select value={draft.protocol} onChange={(e) => change('protocol', e.target.value as Connection['protocol'])}><option value="openai_chat">OpenAI Chat Completions</option><option value="openai_responses">OpenAI Responses</option><option value="anthropic">Anthropic Messages</option><option value="gemini">Gemini</option></Select></label><label className="field"><span>Authentication</span><Select value={draft.authKind} onChange={(e) => change('authKind', e.target.value as Connection['authKind'])}><option value="none">No authentication</option><option value="api_key">API key / bearer token</option>{draft.authKind === 'openai_oauth' && <option value="openai_oauth">ChatGPT OAuth (saved session)</option>}</Select></label>{draft.authKind === 'api_key' && <label className="field full"><span>API key or token{current?.secretPresent && <Badge kind="good">Stored securely</Badge>}</span><input type="password" value={draft.apiKey} onChange={(e) => change('apiKey', e.target.value)} placeholder={current?.secretPresent ? 'Leave blank to keep the stored credential' : 'Enter your API key or LM Studio token'} autoComplete="new-password" /><small className="field-hint"><KeyRound size={12} />Secret values are handled by the Rust runtime and omitted from connection exports.</small></label>}
      <div className="field full model-section"><div className="field-title"><span>Model</span><Button type="button" tone="ghost" onClick={() => void act('discover')} loading={busy === 'discover'} disabled={!!busy || !draft.baseUrl}>Discover models<Radio size={13} /></Button></div>{models.length > 0 && <Select aria-label="Discovered model" value={models.some((m) => m.id === draft.modelId) ? draft.modelId : ''} onChange={(e) => { const model = models.find((m) => m.id === e.target.value); change('modelId', e.target.value); if (model?.contextWindow) change('contextWindow', model.contextWindow); }}><option value="">Choose a discovered model</option>{models.map((m) => <option key={m.id} value={m.id}>{m.name || m.id}{m.loaded === true ? ' · loaded' : m.loaded === false ? ' · available, not loaded' : ''}</option>)}</Select>}<input aria-label="Exact model ID" value={draft.modelId} onChange={(e) => change('modelId', e.target.value)} placeholder="Enter an exact model ID, or discover one above" autoComplete="off" spellCheck={false} /><small className="field-hint">Manual IDs work even when the server does not offer a model catalog. Discovery saves the connection first.</small></div></div>
      <button type="button" className="disclosure" onClick={() => setAdvanced(!advanced)} aria-expanded={advanced}><ChevronRight size={14} className={advanced ? 'rotated' : ''} />Advanced configuration</button>{advanced && <div className="advanced-panel"><label className="field"><span>Context window (tokens)</span><input type="number" min={512} max={10000000} value={draft.contextWindow} onChange={(e) => change('contextWindow', Number(e.target.value))} /><small className="field-hint">Use the effective loaded context for local models, which may be smaller than the architecture maximum.</small></label><label className="check-row"><input type="checkbox" checked={draft.tools} onChange={(e) => change('tools', e.target.checked)} /><span>Enable structured tool calling<small>This is a configured capability. Verify it with a tool probe before relying on it.</small></span></label>{current?.secretPresent && <label className="check-row"><input type="checkbox" checked={draft.removeSecret} onChange={(e) => change('removeSecret', e.target.checked)} /><span>Remove the stored credential when saving</span></label>}</div>}
      <Diagnostics items={diagnostics} /><div className="form-actions"><Button type="button" onClick={() => void act('probe')} loading={busy === 'probe'} disabled={!!busy || !draft.modelId.trim() || !draft.baseUrl}><Zap size={15} />Test connection</Button><Button type="submit" tone="primary" loading={busy === 'save'} disabled={!!busy || !draft.label.trim() || !draft.baseUrl.trim()}><Save size={15} />Save connection</Button></div></form>
      {probe && <div className="probe-result"><div className="section-label"><span>REAL CONNECTION TEST</span><Badge kind={probe.text && probe.streaming ? 'good' : 'warning'}>{probe.text && probe.streaming ? 'Responded' : 'Needs attention'}</Badge></div><div className="probe-checks"><Status ok={probe.text}>Text response</Status><Status ok={probe.streaming}>Streaming</Status><Status ok={probe.tools}>Tool round trip</Status></div>{probe.reply && <blockquote>{probe.reply}</blockquote>}<Diagnostics items={probe.diagnostics} /></div>}
    </div></div>
    <div className="connection-footnote"><ShieldCheck size={17} /><p><strong>One workspace, different ways to think.</strong> Tool support depends on the model and API route. Switching a connection never transfers its credentials to another provider.</p><ArrowRight size={17} /></div>
  </section>;
}
