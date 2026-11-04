import { NcView } from '../components/base.js';
import { h, clear } from '../dom.js';
import { t } from '../i18n.js';
import { fmtDate } from '../format.js';
import { auditLabel } from '../status-util.js';
import { SYSTEM_IDS } from '../systems.js';
import { isAdmin } from '../app-context.js';
import { badge, card, select, btn, emptyBox, errorBox, skeleton } from '../components/ui.js';
import { consolePage } from '../components/system-page.js';

class NcAudit extends NcView {
  setup() {
    consolePage(this, { page: 'audit', active: this.query?.tab, tabDefs: [{ id: 'log', label: t('tab.log'), render: (scope) => this.logTab(scope) }] });
  }
  /** The newest 200 audited calls, optionally for one system. Admin only. */
  logTab(scope) {
    let sys = '';
    const host = h('div', {}, skeleton(4));
    const load = async () => {
      clear(host).append(skeleton(4));
      try {
        const rows = await scope.api.audit({ limit: 200, system: sys });
        if (!scope.alive) return;
        clear(host).append(rows.length ? h('div', { class: 'tablewrap tall' }, h('table', { class: 'compact' },
          h('thead', h('tr', ['audit.when', 'audit.actor', 'audit.system_col', 'audit.action', 'audit.params', 'audit.status'].map((k) => h('th', { scope: 'col' }, t(k))))),
          h('tbody', rows.map((r) => h('tr', h('td', { class: 'num' }, fmtDate(r.ts)), h('td', r.actor || '–'), h('td', r.system || '–'), h('td', h('code', auditLabel(r)), r.role ? h('span', { class: 'muted small' }, ` (${r.role})`) : null),
            h('td', { class: 'wrap-anywhere small' }, r.params && Object.keys(r.params).length ? JSON.stringify(r.params) : '–'), h('td', badge(String(r.status ?? '–'), (r.status ?? 200) >= 400 ? 'bad' : 'ok'))))))) : emptyBox(t('audit.empty')));
      } catch (e) { if (scope.alive) clear(host).append(errorBox(e, load)); }
    };
    const sel = select([{ value: '', label: t('audit.all') }, ...SYSTEM_IDS.map((x) => ({ value: x, label: t(`sys.${x}`) }))], '', (v) => { sys = v; load(); }, { 'aria-label': t('audit.system') });
    if (isAdmin()) load(); else clear(host).append(emptyBox(t('audit.admin_only'), t('audit.admin_only_hint')));
    return h('div', { class: 'stack-v' }, h('p', { class: 'muted' }, t('audit.explain')),
      card(t('audit.title'), host, h('div', { class: 'row gap' }, sel, btn(t('common.refresh'), { size: 'sm', icon: 'restart', onClick: () => { if (isAdmin()) load(); } }))));
  }
}
customElements.define('nc-audit', NcAudit);
