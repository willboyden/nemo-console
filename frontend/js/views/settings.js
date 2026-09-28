import { NcView } from '../components/base.js';
import { h, clear } from '../dom.js';
import { t } from '../i18n.js';
import { store, applyTheme, setApiKey, isAdmin } from '../app-context.js';
import { badge, btn, adminBtn, card, field, select, errorBox, skeleton, kv } from '../components/ui.js';
import { toast, toastError } from '../components/toast.js';
import { announce } from '../components/live.js';

class NcSettings extends NcView {
  setup() {
    this.host = h('div', {}, skeleton(4));
    const theme = select([{ value: 'dark', label: t('settings.dark') }, { value: 'light', label: t('settings.light') }, { value: 'system', label: t('settings.system') }], store.get().theme, (v) => applyTheme(v), { 'aria-label': t('settings.theme') });
    const bearer = h('input', { type: 'password', autocomplete: 'off', spellcheck: 'false', placeholder: store.get().apiKey ? t('settings.bearer_set') : t('settings.bearer_ph'), 'aria-label': t('settings.bearer') });
    this.append(h('h1', t('nav.settings')),
      card(t('settings.appearance'), field(t('settings.theme'), theme)),
      card(t('settings.session'), h('form', { class: 'stack-v', onSubmit: (e) => { e.preventDefault(); if (bearer.value.trim()) { setApiKey(bearer.value.trim()); bearer.value = ''; toast(t('settings.bearer_saved'), { kind: 'ok' }); this.load(); } } },
        h('p', { class: 'hint' }, t('settings.bearer_hint')), field(t('settings.bearer'), bearer),
        h('div', { class: 'row gap' }, btn(t('common.save'), { type: 'submit', kind: 'primary' }), btn(t('auth.clear'), { onClick: () => { setApiKey(''); toast(t('settings.bearer_cleared'), { kind: 'ok' }); } })),
        h('p', { class: 'small muted' }, t('settings.role', { role: store.get().role, trust: store.get().trust || '–' })))),
      card(t('settings.backend'), this.host));
    this.load();
  }
  async load() {
    clear(this.host).append(skeleton(4));
    try {
      const s = await this.scope.api.settings();
      if (!this.scope.alive) return;
      const urls = {}, envs = {}, ro = !isAdmin();
      const urlFields = Object.entries(s.urls || {}).map(([k, v]) => { const i = h('input', { type: 'text', value: v, disabled: ro, spellcheck: 'false', autocomplete: 'off', inputmode: 'url' }); urls[k] = i; return field(t('settings.url_of', { name: k }), i); });
      const poll = h('input', { type: 'number', min: 0.5, step: 'any', value: String(s.poll_interval_s ?? ''), disabled: ro });
      // Credentials are configured by environment-variable NAME; the value never reaches the browser.
      const envFields = Object.entries(s.playground_key_env || {}).map(([k, v]) => { const i = h('input', { type: 'text', value: v, disabled: ro, spellcheck: 'false', autocomplete: 'off', placeholder: 'ENV_VAR_NAME' }); envs[k] = i; return field(t('settings.env_of', { name: k }), i, s.tokens?.[k] || null); });
      const form = h('form', { class: 'stack-v', onSubmit: async (e) => {
        e.preventDefault();
        const body = { urls: Object.fromEntries(Object.entries(urls).map(([k, i]) => [k, i.value.trim()])), poll_interval_s: Number(poll.value), playground_key_env: Object.fromEntries(Object.entries(envs).map(([k, i]) => [k, i.value.trim()])) };
        try { await this.scope.api.saveSettings(body); toast(t('settings.saved'), { kind: 'ok' }); announce(t('settings.saved')); this.load(); } catch (err) { toastError(err); }
      } },
        s.restart_required ? h('p', { class: 'note warn', role: 'status' }, t('settings.restart_required')) : null,
        h('h3', { class: 'sub' }, t('settings.urls')), h('div', { class: 'pgrid' }, urlFields),
        h('h3', { class: 'sub' }, t('settings.polling')), h('div', { class: 'pgrid' }, field(t('settings.poll_interval'), poll, t('settings.ring', { n: s.ring_seconds ?? '–' }))),
        envFields.length ? [h('h3', { class: 'sub' }, t('settings.credentials')), h('div', { class: 'pgrid' }, envFields)] : null,
        h('dl', { class: 'facts' }, kv(t('settings.version'), s.version || '–'), kv(t('settings.state_dir'), h('code', s.state_dir || '–')), kv(t('settings.trust'), s.trust_loopback ? t('settings.trust_loopback') : t('settings.trust_bearer')),
          ...Object.entries(s.tokens || {}).map(([k, v]) => kv(t('settings.token_of', { name: k }), badge(String(v), /^\[set/.test(String(v)) ? 'ok' : 'warn')))),
        h('p', { class: 'hint' }, t('settings.tokens_hint')), h('div', { class: 'row' }, adminBtn(t('common.save'), { kind: 'primary', type: 'submit' })));
      clear(this.host).append(form);
    } catch (e) { if (this.scope.alive) clear(this.host).append(errorBox(e, () => this.load())); }
  }
}
customElements.define('nc-settings', NcSettings);
