// REST client for /api/v1 (ARCHITECTURE.md section 4). Errors are {error:{code,message}}; non-GET sends X-Console: 1.
export class ApiError extends Error {
  constructor(status, body = {}) {
    const e = body && typeof body === 'object' ? (body.error || {}) : {};
    super(e.message || `HTTP ${status}`);
    this.name = 'ApiError';
    this.status = status;
    this.code = e.code || null;
    this.body = body;
  }
}

export const CSRF_HEADER = 'X-Console';
export const SAFE_METHODS = new Set(['GET', 'HEAD']);

export function buildQuery(query) {
  const p = new URLSearchParams();
  for (const [k, v] of Object.entries(query || {})) {
    if (v == null || v === '') continue;
    p.set(k, String(v));
  }
  const s = p.toString();
  return s ? `?${s}` : '';
}
const enc = encodeURIComponent;

export function createClient({ base = '/api/v1', getKey = () => null, onUnauthorized = () => {}, fetchImpl } = {}) {
  const f = (...a) => (fetchImpl || globalThis.fetch)(...a);
  // A bearer is optional (loopback-admin trust needs none); it lives in sessionStorage only.
  const authHeaders = () => { const k = getKey(); return k ? { Authorization: `Bearer ${k}` } : {}; };
  const url = (path, query) => `${base}${path}${buildQuery(query)}`;
  const mutHeaders = (method) => (SAFE_METHODS.has(method.toUpperCase()) ? {} : { [CSRF_HEADER]: '1' });

  async function request(method, path, { query, body, signal } = {}) {
    const hdr = { Accept: 'application/json', ...authHeaders(), ...mutHeaders(method) };
    let payload;
    if (body !== undefined) { hdr['Content-Type'] = 'application/json'; payload = JSON.stringify(body); }
    let res;
    try { res = await f(url(path, query), { method, headers: hdr, body: payload, signal }); }
    catch (e) { if (e?.name === 'AbortError') throw e; throw new ApiError(0, { error: { code: 'network', message: e?.message || 'unreachable' } }); }
    if (res.status === 401) onUnauthorized();
    if (!res.ok) { let b = {}; try { b = await res.json(); } catch { /* not json */ } throw new ApiError(res.status, b); }
    if (res.status === 204) return null;
    return (res.headers.get?.('content-type') || '').includes('json') ? res.json() : res.text();
  }
  const get = (p, q, o) => request('GET', p, { ...o, query: q });
  const post = (p, body, o) => request('POST', p, { ...o, body: body ?? {} });
  const put = (p, body, o) => request('PUT', p, { ...o, body });
  const sys = (id) => `/systems/${enc(id)}`;

  return {
    url, authHeaders, mutHeaders, request, get, post, put,
    health: () => get('/health'),
    overview: () => get('/overview'),
    topology: () => get('/topology'),
    status: (id) => get(`${sys(id)}/status`),
    metrics: (id) => get(`${sys(id)}/metrics`),
    history: (id, key, window) => get(`${sys(id)}/metrics/history`, { key, window }),
    events: (id, limit = 50) => get(`${sys(id)}/events`, { limit }),
    configList: (id) => get(`${sys(id)}/config`),
    config: (id, file) => get(`${sys(id)}/config/${enc(file)}`),
    validate: (id, file, content) => post(`${sys(id)}/config/${enc(file)}/validate`, { content }),
    apply: (id, file, body) => post(`${sys(id)}/config/${enc(file)}/apply`, { ...body, confirm: true }),
    configHistory: (id, file) => get(`${sys(id)}/config/${enc(file)}/history`),
    rollback: (id, file, revision_id) => post(`${sys(id)}/config/${enc(file)}/rollback`, { revision_id, confirm: true }),
    params: (id) => get(`${sys(id)}/params`),
    previewParams: (id, values) => post(`${sys(id)}/params/preview`, { values }),
    applyParams: (id, values) => put(`${sys(id)}/params`, { values, confirm: true }),
    actions: (id) => get(`${sys(id)}/actions`),
    runAction: (id, action, params, confirm) => post(`${sys(id)}/actions/${enc(action)}`, { params: params || {}, confirm: !!confirm }),
    playground: (id) => get(`${sys(id)}/playground`),
    routingSummary: (window) => get('/routing/summary', { window }),
    audit: (q) => get('/audit', q),
    settings: () => get('/settings'),
    saveSettings: (body) => put('/settings', body),
  };
}
