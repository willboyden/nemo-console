// Descriptor-driven page for the systems added in ARCHITECTURE.md 9.8 (openshell, dcgm, aiperf, guardrails, evaluator).
// A descriptor says which metric keys deserve charts / tables; everything else (status, components, problems, events,
// config editor, actions, playground) is driven by what the API returns. Tabs that have nothing to show are not built:
// Config only when GET /config lists files, Playground only when GET /playground returns a spec.
import { h, clear, append } from '../dom.js';
import { t } from '../i18n.js';
import { fmtUnit, fmtTick, fmtBytes, fmtCompact } from '../format.js';
import { metricLabel, labelText, groupPoints, pickChartKeys, chartEntries } from '../metric-groups.js';
import { SERIES_WINDOWS, wantsRate } from '../series-model.js';
import { describeError } from '../errors.js';
import { createHashState } from '../charts/hash-state.js';
import { seriesCard } from '../charts/series-card.js';
import { card, select, stat, btn, emptyBox, errorBox, skeleton } from './ui.js';
import { systemPage } from './system-page.js';
import { componentsTable, endpointsList } from './status-panel.js';
import { eventsPanel } from './events-panel.js';
import { configEditor } from './config-editor.js';
import { playgroundPanel } from './playground.js';
import { actionsPanel } from './actions-panel.js';
import { runHistoryPanel } from './run-history.js';

const MAX_STATS = 24;

/** Short y-axis tick format by unit, so large byte counts and ratios stay inside the plot margin. */
function axisFmt(unit) {
  if (unit === 'ratio') return (v) => `${Math.round(v * 100)}%`;
  if (unit === 'B') return (v) => fmtBytes(v, 0);
  return (v) => (Math.abs(v) >= 10000 ? fmtCompact(v) : fmtTick(v));
}

/**
 * The numbers tiles and the trend charts of a generic page. Each chart is ONE request to the series route (addendum 9.13):
 * a counter (a key ending in `_total`, or a descriptor entry with `rate: true`) is drawn as a rate, a key with several label
 * variants gets one line per label (`split`, or the descriptor entry's own say), and the window, the "show max" toggle and
 * each chart's line picker live in the URL hash.
 */
function statsAndCharts(scope, ctx, desc) {
  const hash = createHashState({ path: desc.system, tab: 'status', initial: ctx?.initial !== false });
  let win = SERIES_WINDOWS.includes(hash.get('window')) ? hash.get('window') : '1h';
  let showMax = hash.get('max') === '1';
  const statsHost = h('div', { class: 'stats' }),
    allHost = h('div'),
    chartsHost = h('div', { class: 'grid two' }),
    tablesHost = h('div', { class: 'stack-v' }),
    note = h('div');
  const cards = new Map(); // key -> {card, split, rate}: a card is kept across refreshes so its picker and table stay as the user left them
  let drawnKeys = '';
  // A page may add cards of its own under the numbers (desc.extras, addendum 9.14): they follow this window picker.
  const windowListeners = new Set();
  const shared = {
    hash,
    win: () => win,
    onWindow: (f) => {
      windowListeners.add(f);
      scope.own(() => windowListeners.delete(f));
    },
  };
  const sel = select(
    SERIES_WINDOWS.map((w) => ({ value: w, label: t(`window.${w}`) })),
    win,
    (v) => {
      win = v;
      hash.set({ window: v === '1h' ? null : v });
      syncMaxToggle();
      load();
      for (const f of [...windowListeners]) f(v);
    },
    { 'aria-label': t('routing.window') },
  );
  const maxBox = h('input', { type: 'checkbox', checked: showMax });
  maxBox.addEventListener('change', () => {
    showMax = maxBox.checked;
    hash.set({ max: showMax ? '1' : null });
    for (const c of cards.values()) c.card.redraw();
  });
  const maxLabel = h('label', { class: 'check' }, maxBox, h('span', t('gen.show_max')));
  /** The max is only worth drawing where a bucket holds several samples (not the raw 15-minute points) and a gauge has some spread. */
  function syncMaxToggle() {
    const applies = win !== '15m';
    maxBox.disabled = !applies;
    maxLabel.title = applies ? t('gen.show_max_tip') : t('gen.show_max_off');
  }
  syncMaxToggle();

  async function load() {
    try {
      const m = await scope.api.metrics(desc.system);
      if (!scope.alive) return;
      if (!m.available) {
        clear(note).append(h('p', { class: 'note warn' }, m.error || t('gen.metrics_unavailable')));
        clear(statsHost);
        clear(allHost);
        clear(chartsHost);
        clear(tablesHost);
        cards.clear();
        drawnKeys = '';
        trendsHead.hidden = true;
        if (extrasEl) extrasEl.hidden = true;
        return;
      }
      clear(note);
      if (extrasEl) extrasEl.hidden = false;
      const grouped = groupPoints(m);
      const single = [...grouped.entries()]
        .filter(([k]) => !(desc.tables || []).includes(k) && (!desc.stats || desc.stats.includes(k)))
        .flatMap(([, ps]) => ps)
        .slice(0, MAX_STATS);
      clear(statsHost).append(...single.map((p) => stat(metricLabel(p.key), fmtUnit(p.value, p.unit), labelText(p.labels) || null)));
      if (!single.length) statsHost.append(emptyBox(t('gen.no_points')));
      append(clear(allHost), [m.points.length ? allValues(m.points) : null]);
      append(
        clear(tablesHost),
        (desc.tables || []).filter((k) => grouped.has(k)).map((k) => labelledTable(k, grouped.get(k))),
      );
      const keys = pickChartKeys(desc, grouped);
      const entries = new Map(chartEntries(desc).map((e) => [e.key, e]));
      for (const key of keys) {
        const entry = entries.get(key) || {};
        const split = typeof entry.split === 'boolean' ? entry.split : grouped.get(key).length > 1;
        const rate = wantsRate(key, entry);
        const have = cards.get(key);
        if (have && have.split === split && have.rate === rate) continue;
        const unit = grouped.get(key)[0].unit;
        cards.set(key, {
          split,
          rate,
          card: seriesCard(scope, { title: metricLabel(key), key, rate, yFmt: axisFmt(unit), valueFmt: (v) => fmtUnit(v, unit), hash, win: () => win, showMax: () => showMax }),
        });
      }
      for (const k of [...cards.keys()]) if (!keys.includes(k)) cards.delete(k);
      const order = keys.join('|');
      if (order !== drawnKeys) {
        drawnKeys = order;
        clear(chartsHost).append(...keys.map((k) => cards.get(k).card.el));
      }
      trendsHead.hidden = !keys.length && !extrasEl;
      await Promise.all(
        keys.map(async (key) => {
          const c = cards.get(key);
          try {
            const a = await scope.api.series(desc.system, { keys: [key], window: win, split: c.split });
            if (scope.alive && cards.get(key) === c) c.card.update(a);
          } catch (e) {
            if (scope.alive && cards.get(key) === c) c.card.fail(describeError(e));
          }
        }),
      );
    } catch (e) {
      if (scope.alive) clear(note).append(errorBox(e, load));
    }
  }
  const trendsHead = h(
    'div',
    { class: 'row gap wrap between' },
    h('h2', t('gen.trends')),
    h('div', { class: 'row gap wrap' }, sel, maxLabel, btn(t('common.refresh'), { size: 'sm', icon: 'restart', onClick: load })),
  );
  trendsHead.hidden = true;
  const extrasEl = desc.extras ? desc.extras(scope, shared) : null;
  if (extrasEl) extrasEl.hidden = true; // shown once the system reports metrics
  statsHost.append(skeleton(2));
  load();
  scope.every(10000, load);
  return h('div', { class: 'stack-v' }, card(t('gen.numbers'), h('div', { class: 'stack-v' }, note, statsHost, allHost)), trendsHead, extrasEl, chartsHost, tablesHost);
}

/** Every metric point in one collapsed table: the complete, text form of the tiles above. */
function allValues(points) {
  return h(
    'details',
    { class: 'chart-data' },
    h('summary', t('gen.all_values', { count: points.length })),
    h(
      'div',
      { class: 'tablewrap tall' },
      h(
        'table',
        { class: 'compact', 'aria-label': t('gen.all_values', { count: points.length }) },
        h('thead', h('tr', h('th', { scope: 'col' }, t('gen.col_metric')), h('th', { scope: 'col' }, t('gen.col_labels')), h('th', { scope: 'col', class: 'al-right' }, t('chart.value')))),
        h(
          'tbody',
          points.map((p) => h('tr', h('td', metricLabel(p.key)), h('td', labelText(p.labels) || '–'), h('td', { class: 'num al-right' }, fmtUnit(p.value, p.unit)))),
        ),
      ),
    ),
  );
}

/** A table of every label set + value for one key (scores, per-model numbers): the text form of data too wide for a chart. */
function labelledTable(key, points) {
  const cols = [...new Set(points.flatMap((p) => Object.keys(p.labels || {})))];
  return card(
    metricLabel(key),
    h(
      'div',
      { class: 'tablewrap' },
      h(
        'table',
        { class: 'compact', 'aria-label': metricLabel(key) },
        h(
          'thead',
          h(
            'tr',
            cols.map((c) => h('th', { scope: 'col' }, c)),
            h('th', { scope: 'col', class: 'al-right' }, t('chart.value')),
          ),
        ),
        h(
          'tbody',
          points.map((p) =>
            h(
              'tr',
              cols.map((c) => h('td', p.labels?.[c] ?? '–')),
              h('td', { class: 'num al-right' }, fmtUnit(p.value, p.unit)),
            ),
          ),
        ),
      ),
    ),
  );
}

function statusTab(scope, ctx, desc) {
  const compHost = h('div');
  ctx.onStatus((st) => append(clear(compHost), [componentsTable(st.components), st.endpoints?.length ? h('h3', { class: 'sub' }, t('status.endpoints')) : null, endpointsList(st.endpoints)]));
  return h(
    'div',
    { class: 'stack-v' },
    h('p', { class: 'hint' }, t(`intro.${desc.system}`)),
    card(t('gen.state'), compHost),
    statsAndCharts(scope, ctx, desc),
    card(t('gen.events'), eventsPanel(scope, desc.system, { mode: 'generic' })),
  );
}

/** Mount the page into a view. Config / playground are probed first so absent tabs are never shown. */
export async function genericSystemView(view, desc) {
  const scope = view.scope;
  const loading = skeleton(3);
  view.append(loading);
  const [cfg, pg] = await Promise.all([scope.api.configList(desc.system).catch(() => []), desc.playground ? scope.api.playground(desc.system).catch(() => null) : Promise.resolve(null)]);
  if (!scope.alive) return;
  loading.remove();
  const tabDefs = [{ id: 'status', label: t('tab.status'), render: (s, ctx) => statusTab(s, ctx, desc) }];
  // Tabs a page adds right after Status (the GPU fleet heatmap, addendum 9.14): `desc.extraTabs()` returns [{id, label, render(scope, ctx)}].
  if (desc.extraTabs) tabDefs.push(...desc.extraTabs());
  // The run history (addendum 9.13) is built from the system's own run events; a page with no descriptor has no such tab.
  if (desc.runHistory) tabDefs.push({ id: 'history', label: t('tab.history'), render: (s, ctx) => runHistoryPanel(s, ctx, desc.runHistory) });
  if (Array.isArray(cfg) && cfg.length) tabDefs.push({ id: 'config', label: t('tab.config'), render: (s) => configEditor(s, desc.system) });
  if (pg) tabDefs.push({ id: 'playground', label: t('tab.playground'), render: (s) => playgroundPanel(s, desc.system) });
  tabDefs.push({ id: 'actions', label: t('tab.actions'), render: (s) => actionsPanel(s, desc.system) });
  systemPage(view, { system: desc.system, active: view.query?.tab, tabDefs });
}
