import { NcView } from '../components/base.js';
import { h, clear } from '../dom.js';
import { t } from '../i18n.js';
import { fmtDate } from '../format.js';
import { auditLabel } from '../status-util.js';
import { isAdmin } from '../app-context.js';
import { badge, card, select, btn, emptyBox, errorBox, skeleton } from '../components/ui.js';

class NcAudit extends NcView {
  setup() {
    this.sys = ''; this.host = h('div', {}, skeleton(4));
    const sel = select([{ value: '', label: t('audit.all') }, ...['switchyard', 'relay', 'dynamo'].map((s) => ({ value: s, label: t(`sys.${s}`) }))], '', (v) => { this.sys = v; this.load(); }, { 'aria-label': t('audit.system') });
    this.append(h('h1', t('nav.audit')), h('p', { class: 'muted' }, t('audit.explain')),
      card(t('audit.title'), this.host, h('div', { class: 'row gap' }, sel, btn(t('common.refresh'), { size: 'sm', icon: 'restart', onClick: () => this.load() }))));
    if (!isAdmin()) { clear(this.host).append(emptyBox(t('audit.admin_only'), t('audit.admin_only_hint'))); return; }
    this.load();
  }
  async load() {
    clear(this.host).append(skeleton(4));
    try {
      const rows = await this.scope.api.audit({ limit: 200, system: this.sys });
      if (!this.scope.alive) return;
      clear(this.host).append(rows.length ? h('div', { class: 'tablewrap tall' }, h('table', { class: 'compact' },
        h('thead', h('tr', ['audit.when', 'audit.actor', 'audit.system_col', 'audit.action', 'audit.params', 'audit.status'].map((k) => h('th', { scope: 'col' }, t(k))))),
        h('tbody', rows.map((r) => h('tr', h('td', { class: 'num' }, fmtDate(r.ts)), h('td', r.actor || '–'), h('td', r.system || '–'), h('td', h('code', auditLabel(r)), r.role ? h('span', { class: 'muted small' }, ` (${r.role})`) : null),
          h('td', { class: 'wrap-anywhere small' }, r.params && Object.keys(r.params).length ? JSON.stringify(r.params) : '–'), h('td', badge(String(r.status ?? '–'), (r.status ?? 200) >= 400 ? 'bad' : 'ok'))))))) : emptyBox(t('audit.empty')));
    } catch (e) { if (this.scope.alive) clear(this.host).append(errorBox(e, () => this.load())); }
  }
}
customElements.define('nc-audit', NcAudit);
