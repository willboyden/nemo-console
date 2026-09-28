import { NcView } from '../components/base.js';
import { h, clear, setTrustedHtml } from '../dom.js';
import { t } from '../i18n.js';
import { fmtInt, fmtCompact, fmtPct, fmtTick } from '../format.js';
import { roundedShares, errorRate, topN } from '../share.js';
import { counterRate, keyWith } from '../metrics.js';
import { shareBars } from '../charts.js';
import { card, select, stat, btn, emptyBox, errorBox, skeleton } from '../components/ui.js';
import { lineChartCard } from '../components/chart-card.js';
import { systemPage } from '../components/system-page.js';
import { statusDetails } from '../components/status-panel.js';
import { eventsPanel } from '../components/events-panel.js';
import { configEditor } from '../components/config-editor.js';
import { playgroundPanel } from '../components/playground.js';
import { actionsPanel } from '../components/actions-panel.js';

const WINDOWS = ['15m', '1h', '24h'];

function shareCard(title, data, unit, key) {
  const rows = topN(roundedShares(data).map((r) => ({ ...r, pct: r.pct })), 8);
  const wrap = h('div');
  setTrustedHtml(wrap, shareBars(rows, { unit, ariaLabel: t('routing.share_aria', { title }), pctFmt: (p) => fmtPct(p, 1), fmt: fmtInt }));
  return card(title, rows.length ? h('div', {}, wrap, h('details', { class: 'chart-data' }, h('summary', t('chart.data_table')),
    h('div', { class: 'tablewrap' }, h('table', { class: 'compact' }, h('thead', h('tr', h('th', { scope: 'col' }, t('routing.name')), h('th', { scope: 'col', class: 'al-right' }, t('routing.count')), h('th', { scope: 'col', class: 'al-right' }, t('routing.share')))),
      h('tbody', rows.map((r) => h('tr', h('td', r.name), h('td', { class: 'num al-right' }, fmtInt(r.value)), h('td', { class: 'num al-right' }, fmtPct(r.pct, 1))))))))) : emptyBox(t('routing.no_data')));
}

function routingTab(scope) {
  let win = '1h';
  const host = h('div', { class: 'stack-v' }), summaryHost = h('div', { class: 'stack-v' }), rateHost = h('div');
  const sel = select(WINDOWS.map((w) => ({ value: w, label: t(`window.${w}`) })), win, (v) => { win = v; load(); }, { 'aria-label': t('routing.window') });
  async function load() {
    try {
      const [sum, hist] = await Promise.all([scope.api.routingSummary(win), scope.api.history('switchyard', 'sy.requests_total', win).catch(() => null)]);
      if (!scope.alive) return;
      const tok = sum.tokens || {};
      clear(summaryHost).append(
        h('div', { class: 'stats' }, stat(t('routing.requests'), fmtInt(sum.requests)), stat(t('routing.errors'), fmtInt(sum.errors), t('routing.error_rate', { pct: fmtPct(errorRate(sum.errors, sum.requests), 2) })),
          ...['prompt', 'completion', 'reasoning', 'cached', 'total'].filter((k) => tok[k] != null).map((k) => stat(t(`tokens.${k}`), fmtCompact(tok[k]), tok[k] != null ? fmtInt(tok[k]) : null))),
        h('div', { class: 'grid three' }, shareCard(t('routing.by_tier'), sum.by_tier, t('unit.requests')), shareCard(t('routing.by_route'), sum.by_route, t('unit.requests')), shareCard(t('routing.by_model'), sum.by_model, t('unit.requests'))));
      const rate = counterRate(hist?.points || []).map(([ts, v]) => [ts, v * 60]);
      clear(rateHost).append(lineChartCard({ title: t('routing.rate_title'), series: [{ name: t('routing.requests'), points: rate }], yFormat: fmtTick, ariaLabel: t('routing.rate_aria') }));
    } catch (e) { if (scope.alive) clear(summaryHost).append(errorBox(e, load)); }
  }
  host.append(h('div', { class: 'row gap wrap between' }, h('h2', t('routing.title')), h('div', { class: 'row gap' }, sel, btn(t('common.refresh'), { size: 'sm', icon: 'restart', onClick: load }))), summaryHost, rateHost,
    card(t('routing.live'), eventsPanel(scope, 'switchyard', { mode: 'routing', kinds: ['routing.decision'] })),
    h('details', { class: 'card details-card' }, h('summary', t('status.components_endpoints')), h('div', { class: 'card-body', id: 'sy-status' })));
  summaryHost.append(skeleton(3));
  load(); scope.every(10000, load);
  return host;
}

class NcSwitchyard extends NcView {
  setup() {
    systemPage(this, { system: 'switchyard', active: this.query?.tab, tabDefs: [
      { id: 'routing', label: t('tab.routing'), render: (scope, ctx) => { const el = routingTab(scope); const box = el.querySelector('#sy-status'); ctx.onStatus((st) => clear(box).append(statusDetails(st))); return el; } },
      { id: 'config', label: t('tab.config'), render: (scope) => configEditor(scope, 'switchyard') },
      { id: 'playground', label: t('tab.playground'), render: (scope) => playgroundPanel(scope, 'switchyard') },
      { id: 'actions', label: t('tab.actions'), render: (scope) => actionsPanel(scope, 'switchyard') },
    ] });
  }
}
customElements.define('nc-switchyard', NcSwitchyard);
