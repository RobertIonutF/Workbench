type ServerInfo = { baseUrl: string; token: string };
let server: ServerInfo | null = null;
let bootstrap: Promise<ServerInfo> | null = null;

export const isDesktop = () => '__TAURI_INTERNALS__' in window;

export async function connect(): Promise<ServerInfo> {
  // A restarted local server can issue a new browser capability. Accept a new
  // explicit fragment even when the existing page is navigating only its hash.
  if (!isDesktop()) {
    const incoming = new URLSearchParams(window.location.hash.slice(1)).get('token');
    if (incoming) {
      sessionStorage.setItem('workbench.token', incoming);
      history.replaceState(null, '', window.location.pathname + window.location.search);
      server = null; bootstrap = null;
    }
  }
  if (server) return server;
  if (!bootstrap) bootstrap = (async () => {
    if (isDesktop()) {
      const { invoke } = await import('@tauri-apps/api/core');
      server = await invoke<ServerInfo>('server_info');
    } else {
      const hash = new URLSearchParams(window.location.hash.slice(1));
      const token = hash.get('token');
      if (token) {
        sessionStorage.setItem('workbench.token', token);
        history.replaceState(null, '', window.location.pathname + window.location.search);
      }
      const saved = token || sessionStorage.getItem('workbench.token');
      if (!saved) throw new Error('Open Workbench using the browser link printed by the Rust server. It includes the local access token.');
      server = { baseUrl: import.meta.env.VITE_WORKBENCH_URL || window.location.origin, token: saved };
    }
    if (!server?.token || !server.baseUrl) throw new Error('The desktop runtime returned incomplete server information.');
    return server;
  })().catch((error: unknown) => { bootstrap = null; throw error; });
  return bootstrap;
}

export async function api<T>(path: string, options: { method?: string; body?: unknown; signal?: AbortSignal } = {}): Promise<T> {
  const info = await connect();
  const response = await fetch(`${info.baseUrl.replace(/\/$/, '')}${path}`, {
    method: options.method || 'GET',
    headers: { 'X-Workbench-Token': info.token, ...(options.body !== undefined ? { 'Content-Type': 'application/json' } : {}) },
    body: options.body !== undefined ? JSON.stringify(options.body) : undefined,
    signal: options.signal,
  });
  const raw = await response.text();
  let data: unknown;
  try { data = raw ? JSON.parse(raw) : null; }
  catch { throw new Error(response.ok ? 'The local server returned an unreadable response.' : `Local server error (${response.status}).`); }
  if (!response.ok) {
    const message = data && typeof data === 'object' && 'error' in data ? String(data.error) : `Request failed (${response.status})`;
    throw new Error(message);
  }
  return data as T;
}

export const post = <T>(path: string, body: unknown = {}) => api<T>(path, { method: 'POST', body });
export const put = <T>(path: string, body: unknown) => api<T>(path, { method: 'PUT', body });
export const del = <T>(path: string) => api<T>(path, { method: 'DELETE' });
export const query = (values: Record<string, string | undefined>) => new URLSearchParams(Object.entries(values).filter((entry): entry is [string, string] => entry[1] !== undefined)).toString();
export const enc = encodeURIComponent;
export const errorText = (error: unknown) => error instanceof Error ? error.message : String(error);
export const openPath = (path: string) => post('/api/system/open', { path });
export const openUrl = (url: string) => {
  const parsed = new URL(url);
  if (!['http:', 'https:'].includes(parsed.protocol)) return Promise.reject(new Error('Only HTTP and HTTPS links can be opened.'));
  return post('/api/system/open', { url });
};
