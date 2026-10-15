// Bootstrap: i18n, theme, shell (nav/topbar), role from /health, hash router -> lazily loaded view custom elements.
import { loadMessages, t } from './i18n.js';
import { normalizeRole } from './status-util.js';
import { store, api, setApiKey, applyTheme, isAdmin } from './app-context.js';
import { createRouter } from './router.js';
import { h, icon, clear, $ } from './dom.js';
import { toast } from './components/toast.js';
import { openModal } from './components/dialog.js';
import './components/toast.js';

export const ROUTES = [
  { path: '/', tag: 'nc-overview', mod: 'overview', nav: 'overview', icon: 'overview', hidden: true },
  { path: '/overview', tag: 'nc-overview', mod: 'overview', nav: 'overview', icon: 'overview' },
  { path: '/switchyard', tag: 'nc-switchyard', mod: 'switchyard', nav: 'switchyard', icon: 'switchyard' },
  { path: '/relay', tag: 'nc-relay', mod: 'relay', nav: 'relay', icon: 'relay' },
  { path: '/dynamo', tag: 'nc-dynamo', mod: 'dynamo', nav: 'dynamo', icon: 'dynamo' },
  { path: '/skillevaluator', tag: 'nc-skillevaluator', mod: 'skillevaluator', nav: 'skillevaluator', icon: 'skills' },
  { path: '/audit', tag: 'nc-audit', mod: 'audit', nav: 'audit', icon: 'audit', admin: true },
  { path: '/settings', tag: 'nc-settings', mod: 'settings', nav: 'settings', icon: 'settings' },
];
const navRoutes = () => ROUTES.filter((r) => !r.hidden && (!r.admin || isAdmin()));

function buildShell(root) {
  const nav = h('nav', { class: 'nav', 'aria-label': t('nav.primary') }, navRoutes().map((r) => h('a', { href: `#${r.path}`, 'data-route': r.path }, icon(r.icon), h('span', t(`nav.${r.nav}`)))));
  const themeBtn = h('button', { type: 'button', class: 'btn ghost', id: 'theme-btn', 'aria-label': t('topbar.theme'), onClick: toggleTheme });
  const keyBtn = h('button', { type: 'button', class: 'btn ghost', 'aria-label': t('topbar.bearer'), title: t('topbar.bearer'), onClick: () => promptKey() }, icon('key'));
  const s = store.get();
  root.append(
    h('a', { class: 'skip', href: '#main' }, t('common.skip')),
    h('aside', { class: 'sidebar' }, h('div', { class: 'brand' }, h('span', { class: 'logo', 'aria-hidden': 'true' }, '◆'), h('span', t('app.name'))), nav),
    h('div', { class: 'content' },
      h('header', { class: 'topbar' }, h('div', { id: 'crumb', class: 'crumb' }),
        h('div', { class: 'row gap' }, h('span', { id: 'role-badge', class: `badge ${s.role === 'admin' ? 'info' : 'warn'}`, title: t('topbar.role_tip') }, t(`role.${s.role}`)), themeBtn, keyBtn)),
      h('main', { id: 'main', tabindex: '-1' })),
    document.createElement('nc-toasts'));
  syncThemeBtn();
}
function syncThemeBtn() {
  const b = $('#theme-btn'); if (!b) return;
  const light = document.documentElement.dataset.theme === 'light';
  clear(b).append(icon(light ? 'moon' : 'sun'));
  b.title = light ? t('topbar.to_dark') : t('topbar.to_light');
}
function toggleTheme() { applyTheme(document.documentElement.dataset.theme === 'light' ? 'dark' : 'light'); syncThemeBtn(); }

let authDialog = null;
function promptKey() {
  if (authDialog) return;
  const input = h('input', { type: 'password', autocomplete: 'off', spellcheck: 'false', id: 'key-in', 'aria-label': t('auth.key_label'), placeholder: store.get().apiKey ? t('settings.bearer_set') : '' });
  const form = h('form', { class: 'stack-v', onSubmit: (e) => { e.preventDefault(); if (input.value.trim()) setApiKey(input.value.trim()); authDialog?.close(); toast(t('auth.saved'), { kind: 'ok' }); loadHealth().then(() => router.refresh()); } },
    h('p', t('auth.explain')), h('div', { class: 'field' }, h('label', { for: 'key-in' }, t('auth.key_label')), input),
    h('div', { class: 'row gap end' }, h('button', { type: 'button', class: 'btn', onClick: () => { setApiKey(''); authDialog?.close(); } }, t('auth.clear')), h('button', { type: 'submit', class: 'btn primary' }, t('common.save'))));
  authDialog = openModal(t('auth.title'), form, { onClose: () => { authDialog = null; } });
  input.focus();
}

// /health carries trust mode; the role is read from it when the backend provides one (the server enforces it either way).
async function loadHealth() {
  try {
    const hl = await api.health();
    store.set({ role: normalizeRole(hl.role), trust: hl.trust || null, version: hl.version || null });
    return hl;
  } catch { return null; }
}

let current = null, seq = 0;
async function navigate({ path, route, params, query }) {
  const main = $('#main');
  const my = ++seq;
  if (!route) { clear(main).append(h('div', { class: 'state empty' }, h('strong', t('common.not_found')), h('a', { href: '#/overview' }, t('nav.overview')))); return; }
  document.querySelectorAll('.nav a').forEach((a) => { if (a.dataset.route === path || (route.path === '/' && a.dataset.route === '/overview')) a.setAttribute('aria-current', 'page'); else a.removeAttribute('aria-current'); });
  $('#crumb').textContent = t(`nav.${route.nav}`);
  document.title = `${t(`nav.${route.nav}`)} · ${t('app.name')}`;
  try { await import(`./views/${route.mod}.js`); }
  catch (e) { clear(main).append(h('div', { class: 'state error', role: 'alert' }, `${t('common.error')}: ${e.message}`)); return; }
  if (my !== seq) return;
  current?.remove();
  current = document.createElement(route.tag);
  current.params = params || {}; current.query = query || {}; current.path = path;
  clear(main).append(current);
  main.focus({ preventScroll: true });
}

let router;
async function boot() {
  try { await loadMessages(); } catch (e) { console.error(e); }
  if (/[?&]theme=(light|dark)/.test(location.search)) store.set({ theme: document.documentElement.dataset.theme }); else applyTheme(store.get().theme);
  await loadHealth();
  buildShell($('#app'));
  store.subscribe((a) => { if (a) promptKey(); }, (s) => s.authRequired);
  if (store.get().authRequired || store.get().role === 'public') promptKey();
  router = createRouter({ routes: ROUTES, onNavigate: navigate });
  router.start();
}
boot();
