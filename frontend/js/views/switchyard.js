import { NcView } from '../components/base.js';
import { h, clear } from '../dom.js';
import { t } from '../i18n.js';
import { fmtInt, fmtCompact, fmtPct, fmtTick } from '../format.js';
import { errorRate } from '../share.js';
import { counterRate } from '../metrics.js';
import { card, select, stat, btn, errorBox, field, skeleton } from '../components/ui.js';
import { lineChartCard } from '../components/chart-card.js';
import { systemPage } from '../components/system-page.js';
import { statusDetails } from '../components/status-panel.js';
import { eventsPanel } from '../components/events-panel.js';
import { configEditor } from '../components/config-editor.js';
import { playgroundPanel } from '../components/playground.js';
import { actionsPanel } from '../components/actions-panel.js';
import { createHashState } from '../charts/hash-state.js';
import { filterCounts } from '../switchyard-model.js';
import { shareCard } from '../switchyard/share-card.js';
import { flowTab } from '../switchyard/flow-tab.js';

const WINDOWS = ['15m', '1h', '24h'];

function routingTab(scope, ctx) {
  let win = '1h';
  // The route and model filters (addendum 9.14) are frontend filters over what the server sent: the by-route and by-model cards keep only the
  // chosen name (the tier card has no route or model, so it is shown whole). They live in the URL hash.
  const hash = createHashState({ path: 'switchyard', tab: 'routing', initial: ctx?.initial !== false });
  let route = hash.get('route'),
    model = hash.get('model');
  let lastSum = null;
  const host = h('div', { class: 'stack-v' }),
    summaryHost = h('div', { class: 'stack-v' }),
    rateHost = h('div');
  const sel = select(
    WINDOWS.map((w) => ({ value: w, label: t(`window.${w}`) })),
    win,
    (v) => {
      win = v;
      load();
    },
    { 'aria-label': t('routing.window') },
  );
  const routeSel = h('select', { 'aria-label': t('sy.flow.route_filter'), onChange: (e) => setFilter({ route: e.target.value }) });
  const modelSel = h('select', { 'aria-label': t('sy.flow.model_filter'), onChange: (e) => setFilter({ model: e.target.value }) });
  function setFilter(patch) {
    if ('route' in patch) route = patch.route;
    if ('model' in patch) model = patch.model;
    hash.set({ route: route || null, model: model || null });
    renderSummary();
  }
  function fillFilters(sum) {
    const fill = (selEl, names, current, allLabel) => {
      const list = [...new Set([...(current ? [current] : []), ...names])];
      selEl.replaceChildren(h('option', { value: '', selected: !current }, allLabel), ...list.map((n) => h('option', { value: n, selected: n === current }, n)));
    };
    fill(routeSel, Object.keys(sum.by_route || {}).sort(), route, t('sy.flow.all_routes'));
    fill(modelSel, Object.keys(sum.by_model || {}).sort(), model, t('sy.flow.all_models'));
  }
  function renderSummary() {
    const sum = lastSum;
    if (!sum) return;
    fillFilters(sum);
    const tok = sum.tokens || {};
    clear(summaryHost).append(
      h(
        'div',
        { class: 'stats' },
        stat(t('routing.requests'), fmtInt(sum.requests)),
        stat(t('routing.errors'), fmtInt(sum.errors), t('routing.error_rate', { pct: fmtPct(errorRate(sum.errors, sum.requests), 2) })),
        ...['prompt', 'completion', 'reasoning', 'cached', 'total'].filter((k) => tok[k] != null).map((k) => stat(t(`tokens.${k}`), fmtCompact(tok[k]), tok[k] != null ? fmtInt(tok[k]) : null)),
      ),
      route || model ? h('p', { class: 'note info', role: 'status' }, t('routing.filter_note')) : null,
      h(
        'div',
        { class: 'grid three' },
        shareCard(t('routing.by_tier'), sum.by_tier, t('unit.requests')),
        shareCard(t('routing.by_route'), filterCounts(sum.by_route, route), t('unit.requests')),
        shareCard(t('routing.by_model'), filterCounts(sum.by_model, model), t('unit.requests')),
      ),
    );
  }
  async function load() {
    try {
      const [sum, hist] = await Promise.all([scope.api.routingSummary(win), scope.api.history('switchyard', 'sy.requests_total', win).catch(() => null)]);
      if (!scope.alive) return;
      lastSum = sum;
      renderSummary();
      const rate = counterRate(hist?.points || []).map(([ts, v]) => [ts, v * 60]);
      clear(rateHost).append(lineChartCard({ title: t('routing.rate_title'), series: [{ name: t('routing.requests'), points: rate }], yFormat: fmtTick, ariaLabel: t('routing.rate_aria') }));
    } catch (e) {
      if (scope.alive) clear(summaryHost).append(errorBox(e, load));
    }
  }
  host.append(
    h('h2', t('routing.title')),
    h(
      'div',
      { class: 'row gap wrap bottom w2-controls' },
      field(t('routing.window'), sel),
      field(t('sy.flow.route'), routeSel),
      field(t('sy.flow.model'), modelSel),
      btn(t('common.refresh'), { size: 'sm', icon: 'restart', onClick: load }),
    ),
    summaryHost,
    rateHost,
    card(t('routing.live'), eventsPanel(scope, 'switchyard', { mode: 'routing', kinds: ['routing.decision'] })),
    h('details', { class: 'card details-card' }, h('summary', t('status.components_endpoints')), h('div', { class: 'card-body', id: 'sy-status' })),
  );
  summaryHost.append(skeleton(3));
  load();
  scope.every(10000, load);
  return host;
}

class NcSwitchyard extends NcView {
  setup() {
    systemPage(this, {
      system: 'switchyard',
      active: this.query?.tab,
      tabDefs: [
        {
          id: 'routing',
          label: t('tab.routing'),
          render: (scope, ctx) => {
            const el = routingTab(scope, ctx);
            const box = el.querySelector('#sy-status');
            ctx.onStatus((st) => clear(box).append(statusDetails(st)));
            return el;
          },
        },
        { id: 'flow', label: t('tab.flow'), render: (scope, ctx) => flowTab(scope, ctx) },
        { id: 'config', label: t('tab.config'), render: (scope) => configEditor(scope, 'switchyard') },
        { id: 'playground', label: t('tab.playground'), render: (scope) => playgroundPanel(scope, 'switchyard') },
        { id: 'actions', label: t('tab.actions'), render: (scope) => actionsPanel(scope, 'switchyard') },
      ],
    });
  }
}
customElements.define('nc-switchyard', NcSwitchyard);
