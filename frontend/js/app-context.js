// Singletons shared by views: store, API client (optional bearer in sessionStorage only), stream opener.
import { createStore, session, local } from './store.js';
import { createClient, CSRF_HEADER, SAFE_METHODS } from './api.js';
import { openStream } from './sse.js';
import { toastError } from './components/toast.js';

export const store = createStore({
  apiKey: session.get('nc.bearer', ''),   // sessionStorage only, never localStorage
  authRequired: false,
  theme: local.get('nc.theme', 'dark'),
  role: 'admin',      // enforced server-side; this only decides whether admin controls are shown enabled
  trust: null,
  version: null,
});

export const api = createClient({
  getKey: () => store.get().apiKey,
  onUnauthorized: () => store.set({ authRequired: true }),
});

// viewer and public (no key yet) both get admin controls disabled; an absent role is treated as admin until a 403 says otherwise.
export const isAdmin = () => store.get().role === 'admin';

export function setApiKey(key) {
  if (key) session.set('nc.bearer', key); else session.remove('nc.bearer');
  api.clearAuthHold();   // a new sign-in, even with the same key, lets requests through again
  store.set({ apiKey: key || '', authRequired: false });
}

export function stream(path, opts = {}) {
  const { query, ...rest } = opts;
  return openStream(api.url(path, query), {
    ...rest,
    headers: { ...api.authHeaders(), ...(SAFE_METHODS.has((rest.method || 'GET').toUpperCase()) ? {} : { [CSRF_HEADER]: '1' }), ...(rest.headers || {}) },
    onError: (e) => {
      if (e?.status === 401) store.set({ authRequired: true });
      if (e?.status === 429) toastError(e);
      rest.onError?.(e);
    },
  });
}

export function applyTheme(theme) {
  const eff = theme === 'system' ? (matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark') : theme;
  document.documentElement.dataset.theme = eff;
  local.set('nc.theme', theme);
  store.set({ theme });
}
