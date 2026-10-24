// Descriptor-driven page for the systems added in ARCHITECTURE.md 9.8 (openshell, dcgm, aiperf, guardrails, evaluator).
// A descriptor says which metric keys deserve charts / tables; everything else (status, components, problems, events,
// config editor, actions, playground) is driven by what the API returns. Tabs that have nothing to show are not built:
// Config only when GET /config lists files, Playground only when GET /playground returns a spec.
import { h, clear, append } from '../dom.js';
import { t } from '../i18n.js';
import { fmtUnit, fmtTick, fmtBytes, fmtCompact } from '../format.js';
import { metricLabel, labelText, groupPoints, pickChartKeys } from '../metric-groups.js';
import { keyWith } from '../metrics.js';
import { card, select, stat, btn, emptyBox, errorBox, skeleton } from './ui.js';
import { lineChartCard } from './chart-card.js';
import { systemPage } from './system-page.js';
import { componentsTable, endpointsList } from './status-panel.js';
import { eventsPanel } from './events-panel.js';
import { configEditor } from './config-editor.js';
import { playgroundPanel } from './playground.js';
import { actionsPanel } from './actions-panel.js';

const WINDOWS = ['15m', '1h', '24h'];
const MAX_SERIES = 4, MAX_STATS = 24;


/** Short y-axis tick format by unit, so large byte counts and ratios stay inside the plot margin. */
function axisFmt(unit) {
  if (unit === 'ratio') return (v) => `${Math.round(v * 100)}%`;
  if (unit === 'B') return (v) => fmtBytes(v, 0);
  return (v) => (Math.abs(v) >= 10000 ? fmtCompact(v) : fmtTick(v));
}

function statsAndCharts(scope, desc) {
  let win = '1h';
  const statsHost = h('div', { class: 'stats' }), allHost = h('div'), chartsHost = h('div', { class: 'grid two' }), tablesHost = h('div', { class: 'stack-v' }), note = h('div');
  const sel = select(WINDOWS.map((w) => ({ value: w, label: t(`window.${w}`) })), win, (v) => { win = v; load(); }, { 'aria-label': t('routing.window') });
  async function load() {
    try {
      const m = await scope.api.metrics(desc.system);
      if (!scope.alive) return;
      if (!m.available) { clear(note).append(h('p', { class: 'note warn' }, m.error || t('gen.metrics_unavailable'))); clear(statsHost); clear(allHost); clear(chartsHost); clear(tablesHost); return; }
      clear(note);
      const grouped = groupPoints(m);
      const single = [...grouped.entries()].filter(([k]) => !(desc.tables || []).includes(k) && (!desc.stats || desc.stats.includes(k))).flatMap(([, ps]) => ps).slice(0, MAX_STATS);
      clear(statsHost).append(...single.map((p) => stat(metricLabel(p.key), fmtUnit(p.value, p.unit), labelText(p.labels) || null)));
      if (!single.length) statsHost.append(emptyBox(t('gen.no_points')));
      append(clear(allHost), [m.points.length ? allValues(m.points) : null]);
      append(clear(tablesHost), (desc.tables || []).filter((k) => grouped.has(k)).map((k) => labelledTable(k, grouped.get(k))));
      const keys = pickChartKeys(desc, grouped);
      const cards = await Promise.all(keys.map(async (key) => {
        const pts = grouped.get(key).slice(0, MAX_SERIES), unit = pts[0].unit;
        const series = await Promise.all(pts.map(async (p) => {
          const hist = await scope.api.history(desc.system, keyWith(key, p.labels), win).catch(() => null);
          return { name: labelText(p.labels) || metricLabel(key), points: hist?.points || [] };
        }));
        return lineChartCard({ title: metricLabel(key), series, yFormat: axisFmt(unit), valueFmt: (v) => fmtUnit(v, unit) });
      }));
      if (!scope.alive) return;
      trendsHead.hidden = !cards.length;
      clear(chartsHost).append(...cards);
    } catch (e) { if (scope.alive) clear(note).append(errorBox(e, load)); }
  }
  const trendsHead = h('div', { class: 'row gap wrap between' }, h('h2', t('gen.trends')), h('div', { class: 'row gap' }, sel, btn(t('common.refresh'), { size: 'sm', icon: 'restart', onClick: load })));
  trendsHead.hidden = true;
  statsHost.append(skeleton(2));
  load(); scope.every(10000, load);
  return h('div', { class: 'stack-v' },
    card(t('gen.numbers'), h('div', { class: 'stack-v' }, note, statsHost, allHost)),
    trendsHead,
    chartsHost, tablesHost);
}

/** Every metric point in one collapsed table: the complete, text form of the tiles above. */
function allValues(points) {
  return h('details', { class: 'chart-data' }, h('summary', t('gen.all_values', { count: points.length })),
    h('div', { class: 'tablewrap tall' }, h('table', { class: 'compact', 'aria-label': t('gen.all_values', { count: points.length }) },
      h('thead', h('tr', h('th', { scope: 'col' }, t('gen.col_metric')), h('th', { scope: 'col' }, t('gen.col_labels')), h('th', { scope: 'col', class: 'al-right' }, t('chart.value')))),
      h('tbody', points.map((p) => h('tr', h('td', metricLabel(p.key)), h('td', labelText(p.labels) || '–'), h('td', { class: 'num al-right' }, fmtUnit(p.value, p.unit))))))));
}

/** A table of every label set + value for one key (scores, per-model numbers): the text form of data too wide for a chart. */
function labelledTable(key, points) {
  const cols = [...new Set(points.flatMap((p) => Object.keys(p.labels || {})))];
  return card(metricLabel(key), h('div', { class: 'tablewrap' }, h('table', { class: 'compact', 'aria-label': metricLabel(key) },
    h('thead', h('tr', cols.map((c) => h('th', { scope: 'col' }, c)), h('th', { scope: 'col', class: 'al-right' }, t('chart.value')))),
    h('tbody', points.map((p) => h('tr', cols.map((c) => h('td', p.labels?.[c] ?? '–')), h('td', { class: 'num al-right' }, fmtUnit(p.value, p.unit))))))));
}

function statusTab(scope, ctx, desc) {
  const compHost = h('div');
  ctx.onStatus((st) => append(clear(compHost), [componentsTable(st.components), st.endpoints?.length ? h('h3', { class: 'sub' }, t('status.endpoints')) : null, endpointsList(st.endpoints)]));
  return h('div', { class: 'stack-v' },
    h('p', { class: 'hint' }, t(`intro.${desc.system}`)),
    card(t('gen.state'), compHost),
    statsAndCharts(scope, desc),
    card(t('gen.events'), eventsPanel(scope, desc.system, { mode: 'generic' })));
}

/** Mount the page into a view. Config / playground are probed first so absent tabs are never shown. */
export async function genericSystemView(view, desc) {
  const scope = view.scope;
  const loading = skeleton(3); view.append(loading);
  const [cfg, pg] = await Promise.all([
    scope.api.configList(desc.system).catch(() => []),
    desc.playground ? scope.api.playground(desc.system).catch(() => null) : Promise.resolve(null),
  ]);
  if (!scope.alive) return;
  loading.remove();
  const tabDefs = [{ id: 'status', label: t('tab.status'), render: (s, ctx) => statusTab(s, ctx, desc) }];
  if (Array.isArray(cfg) && cfg.length) tabDefs.push({ id: 'config', label: t('tab.config'), render: (s) => configEditor(s, desc.system) });
  if (pg) tabDefs.push({ id: 'playground', label: t('tab.playground'), render: (s) => playgroundPanel(s, desc.system) });
  tabDefs.push({ id: 'actions', label: t('tab.actions'), render: (s) => actionsPanel(s, desc.system) });
  systemPage(view, { system: desc.system, active: view.query?.tab, tabDefs });
}
