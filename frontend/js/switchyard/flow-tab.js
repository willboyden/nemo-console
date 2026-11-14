// The "Decisions and models" tab of the Switchyard page (addendum 9.14): the decision flow (route -> decision source -> target, widths =
// the share of decisions over the window), a row per model (p50 / p99 latency, error rate, cache hit, largest context), a heatmap of the
// call rate per model over time, fallbacks and routing overhead, and what clients got. One series request (split=1) feeds every card; the
// window, a route filter and a model filter live in the URL hash. A figure the counters cannot give is an en dash, never 0. The charts and
// tables are built once and updated in place, so an open data table survives the 15-second refresh.
import { h } from '../dom.js';
import { t } from '../i18n.js';
import { describeError } from '../errors.js';
import { fmtInt, fmtCompact, fmtMs, fmtRatio, fmtPct } from '../format.js';
import { badge, btn, card, emptyBox, errorBox, field, select, skeleton, stat } from '../components/ui.js';
import { createHashState } from '../charts/hash-state.js';
import { flowChart } from '../charts/svg-charts.js';
import { matrixHeatmap } from '../charts/heatmap.js';
import { linesCard } from '../charts/lines-card.js';
import { liveTable, labelledBar, chartEmpty, repaint, swap, windowPhrase } from '../charts/parts.js';
import { normalizeAnswer, rateSegments, valueSegments, pickRateScale, scaleSegments } from '../series-model.js';
import {
  FLOW_KEYS,
  FLOW_WINDOWS,
  seriesOfKey,
  decisionFlow,
  nodeName,
  nodeLayer,
  routeCallsFromSnapshot,
  filterOptions,
  modelRows,
  rateMatrix,
  fallbackModel,
  overheadModel,
  outcomeRows,
} from '../switchyard-model.js';
import { shareCard } from './share-card.js';

const REFRESH_MS = 15000;

export function flowTab(scope, ctx) {
  const hash = createHashState({ path: 'switchyard', tab: 'flow', initial: ctx?.initial !== false });
  let win = FLOW_WINDOWS.includes(hash.get('window')) ? hash.get('window') : '1h';
  let route = hash.get('route'),
    model = hash.get('model');
  let answer = null,
    snap = null,
    seq = 0,
    optionSig = '';
  const winFn = () => win;

  // ------------------------------------------------------------------------------------------------------------ controls
  const winSel = select(
    FLOW_WINDOWS.map((w) => ({ value: w, label: t(`window.${w}`) })),
    win,
    (v) => {
      win = v;
      hash.set({ window: v === '1h' ? null : v });
      load();
    },
    { 'aria-label': t('routing.window') },
  );
  const routeSel = h('select', { 'aria-label': t('sy.flow.route_filter'), onChange: (e) => setFilter({ route: e.target.value }) });
  const modelSel = h('select', { 'aria-label': t('sy.flow.model_filter'), onChange: (e) => setFilter({ model: e.target.value }) });
  const controls = h(
    'div',
    { class: 'row gap wrap bottom w2-controls' },
    field(t('routing.window'), winSel),
    field(t('sy.flow.route'), routeSel),
    field(t('sy.flow.model'), modelSel),
    btn(t('common.refresh'), { size: 'sm', icon: 'restart', onClick: () => load() }),
  );
  function setFilter(patch) {
    if ('route' in patch) route = patch.route;
    if ('model' in patch) model = patch.model;
    hash.set({ route: route || null, model: model || null });
    render();
  }
  function fillOptions(opts) {
    const sig = JSON.stringify([opts, route, model]);
    if (sig === optionSig) return;
    optionSig = sig;
    const fill = (sel, names, current, allLabel) => {
      const list = current && !names.includes(current) ? [current, ...names] : names;
      sel.replaceChildren(h('option', { value: '', selected: !current }, allLabel), ...list.map((n) => h('option', { value: n, selected: n === current }, n)));
    };
    fill(routeSel, opts.routes, route, t('sy.flow.all_routes'));
    fill(modelSel, opts.models, model, t('sy.flow.all_models'));
  }

  // ------------------------------------------------------------------------------------------------------------ decision flow
  let flowState = null; // {flows, heads, alt}: what the chart draws
  const flowChartHost = h('div', { class: 'chart-wrap-lg sy-flow-chart' });
  const flowTable = liveTable(t('chart.data_table'), [t('sy.flow.edge'), t('sy.flow.col_layer'), t('sy.flow.col_count'), t('routing.share')]);
  const flowHead = h('div', { class: 'row gap wrap sy-flow-head' });
  const flowNotes = h('div', { class: 'stack-v' });
  const flowEmpty = h('div');
  const flowShares = h('div', { class: 'sy-route-shares' });
  const flowBody = h('div', { class: 'stack-v' }, h('p', { class: 'hint' }, t('sy.flow.lead')), flowHead, flowNotes, flowEmpty, flowChartHost, flowTable.el);
  const flowCardEl = card(t('sy.flow.title'), flowBody);

  function drawFlow() {
    if (!flowState) return;
    const w = Math.max(280, Math.round(flowChartHost.clientWidth || 640));
    const chart = flowState.flows.length ? flowChart(flowState.flows, { width: w, height: w < 520 ? 300 : 260, ariaLabel: flowState.alt, heads: flowState.heads, labelOf: nodeName }) : null;
    flowChartHost.replaceChildren(chart ?? chartEmpty(t('sy.flow.filtered_out')));
  }
  let flowW = 0;
  if (typeof ResizeObserver !== 'undefined') {
    const ro = new ResizeObserver(() => {
      const w = Math.round(flowChartHost.clientWidth);
      if (w < 80 || Math.abs(w - flowW) < 6) return;
      flowW = w;
      drawFlow();
    });
    ro.observe(flowChartHost);
    scope.own(() => ro.disconnect());
  }

  function updateFlow(series, routeCalls) {
    const decisions = seriesOfKey(series, 'sy.decisions_total');
    const f = decisionFlow({ decisions, routeCalls }, { route, model });
    swap(flowNotes);
    if (f.mode === 'none') {
      flowState = null;
      flowChartHost.hidden = true;
      flowTable.el.hidden = true;
      swap(flowHead);
      swap(flowEmpty, emptyBox(t('sy.flow.none'), !decisions.length ? t('sy.flow.none_hint') : t('sy.flow.idle_hint', { window: windowPhrase(win) })));
    } else {
      const heads = f.mode === 'three' ? [t('sy.flow.col_route'), t('sy.flow.col_source'), t('sy.flow.col_target')] : [t('sy.flow.col_source'), t('sy.flow.col_target')];
      flowState = { flows: f.flows, heads, alt: t('sy.flow.alt', { total: fmtInt(f.total), window: windowPhrase(win), cols: heads.length }) };
      swap(flowEmpty);
      flowChartHost.hidden = false;
      flowTable.el.hidden = false;
      swap(
        flowHead,
        h('span', { class: 'muted small' }, t('sy.flow.total', { count: fmtInt(f.total), window: windowPhrase(win) })),
        f.resets ? badge(t('sy.flow.reset', { count: f.resets }), 'warn') : null,
      );
      swap(
        flowNotes,
        f.mode === 'two' ? h('p', { class: 'note info', role: 'status' }, t('sy.flow.two_col_note')) : null,
        route && !f.routeApplied ? h('p', { class: 'note info', role: 'status' }, t('sy.flow.route_not_applied')) : null,
      );
      flowTable.set(
        f.flows.map((x) => [
          `${nodeName(x.source)} → ${nodeName(x.target)}`,
          nodeLayer(x.source) === 'route' ? t('sy.flow.col_route') : t('sy.flow.col_source'),
          fmtInt(x.value),
          f.total > 0 ? fmtPct((x.value / f.total) * 100, 1) : null,
        ]),
      );
      drawFlow();
    }
    // `sy.route.calls` is a count among the newest routing records, not a counter: its own small list, never a part of the flow.
    repaint(flowShares, () => {
      if (!f.routeShares.length) return null;
      const data = Object.fromEntries(f.routeShares.map((r) => [r.route, r.value]));
      const w = f.routeShares[0].window.replace(/^last_(\d+)_records$/, (_, n) => t('sy.flow.last_records', { count: n }));
      return shareCard(t('sy.flow.route_shares'), data, t('unit.requests'), { note: w || null });
    });
  }

  // ------------------------------------------------------------------------------------------------------------ per-model rows
  const modelsHost = h('div');
  function updateModels(series) {
    const rows = modelRows(
      {
        latency: seriesOfKey(series, 'sy.model.latency_ms'),
        errors: seriesOfKey(series, 'sy.model.errors_total'),
        calls: seriesOfKey(series, 'sy.model.calls'),
        cache: seriesOfKey(series, 'sy.model.cache_hit_ratio'),
        context: seriesOfKey(series, 'sy.model.max_context_tokens'),
      },
      { model },
    );
    repaint(modelsHost, () => {
      if (!rows.length) return card(t('sy.models.title'), emptyBox(t('sy.models.none'), t('sy.models.none_hint')));
      const maxMs = Math.max(0, ...rows.flatMap((r) => [r.p50, r.p99].filter((v) => v != null)));
      const maxCtx = Math.max(0, ...rows.map((r) => r.maxContext ?? 0));
      const maxErr = Math.max(0, ...rows.map((r) => r.errorRate ?? 0));
      const cell = (value, max, text, label, cls = '') =>
        h('td', { class: 'sy-bar-cell' }, h('div', { class: 'sy-cell' }, labelledBar(value, max, `${label}: ${text}`, cls), h('span', { class: 'num' }, text)));
      const body = rows.map((r) =>
        h(
          'tr',
          h('th', { scope: 'row', class: 'sy-model' }, r.model),
          cell(r.p50, maxMs, fmtMs(r.p50), `${r.model} ${t('sy.models.p50')}`, 'bf1'),
          cell(r.p99, maxMs, fmtMs(r.p99), `${r.model} ${t('sy.models.p99')}`, 'bf3'),
          cell(r.errorRate, Math.max(maxErr, 0.01), r.errorRate == null ? '–' : fmtPct(r.errorRate * 100, r.errorRate < 0.1 ? 2 : 1), `${r.model} ${t('sy.models.error_rate')}`, 'bf-bad'),
          cell(r.cache, 1, r.cache == null ? '–' : fmtRatio(r.cache, 0), `${r.model} ${t('sy.models.cache')}`, 'bf2'),
          cell(r.maxContext, maxCtx, r.maxContext == null ? '–' : fmtCompact(r.maxContext), `${r.model} ${t('sy.models.context')}`, 'bf4'),
          h('td', { class: 'num al-right' }, r.calls == null ? '–' : fmtInt(r.calls)),
        ),
      );
      return card(
        t('sy.models.title'),
        h(
          'div',
          { class: 'stack-v' },
          h('p', { class: 'hint' }, t('sy.models.lead', { window: windowPhrase(win) })),
          h(
            'div',
            { class: 'tablewrap' },
            h(
              'table',
              { class: 'compact sy-models', 'aria-label': t('sy.models.title') },
              h(
                'thead',
                h(
                  'tr',
                  h('th', { scope: 'col' }, t('sy.models.model')),
                  h('th', { scope: 'col' }, t('sy.models.p50')),
                  h('th', { scope: 'col' }, t('sy.models.p99')),
                  h('th', { scope: 'col' }, t('sy.models.error_rate')),
                  h('th', { scope: 'col' }, t('sy.models.cache')),
                  h('th', { scope: 'col' }, t('sy.models.context')),
                  h('th', { scope: 'col', class: 'al-right' }, t('sy.models.calls')),
                ),
              ),
              h('tbody', body),
            ),
          ),
          h('p', { class: 'muted small' }, t('sy.models.note')),
        ),
      );
    });
  }

  // ------------------------------------------------------------------------------------------------------------ call-rate heatmap
  let heatModel = null;
  const heat = matrixHeatmap(scope, {
    cellH: 20,
    build: () =>
      heatModel && heatModel.rows.length
        ? { rows: heatModel.rows.map((r) => ({ id: r.model, label: r.model, group: 'm', values: r.values, stats: null })), ts: heatModel.ts, bucketS: heatModel.bucketS }
        : null,
    domain: () => ({ min: 0, max: heatModel?.max || 1, curve: 'sqrt' }),
    fmt: () => (v) => `${+v.toFixed(v < 1 ? 3 : 2)}/${heatModel?.unit === 'min' ? t('sy.heat.min') : t('sy.heat.s')}`,
    ariaLabel: () => t('sy.heat.alt', { models: heatModel.rows.length, window: windowPhrase(win), unit: heatModel.unit === 'min' ? t('sy.heat.per_min') : t('sy.heat.per_s') }),
  });
  const heatEmpty = h('div');
  const heatCard = card(t('sy.heat.title'), h('div', { class: 'stack-v' }, h('p', { class: 'hint' }, t('sy.heat.lead')), heatEmpty, heat.el));
  function updateHeat(series, meta) {
    heatModel = rateMatrix(seriesOfKey(series, 'sy.model.calls'), meta, { maxCols: Math.max(48, Math.min(160, Math.floor((heat.el.clientWidth || 700) / 5))), model });
    const empty = !heatModel.rows.length || heatModel.rows.every((r) => r.values.every((v) => v == null));
    swap(heatEmpty, empty ? emptyBox(t('sy.heat.none'), t('sy.heat.none_hint')) : '');
    heat.el.hidden = empty;
    if (!empty) heat.redraw();
  }

  // ------------------------------------------------------------------------------------------------------------ fallbacks + overhead
  const fbChart = linesCard(scope, {
    title: t('sy.fb.rate_title'),
    panels: [{ id: 'fb', yFmt: (v) => `${+v.toPrecision(2)}`, valueFmt: (v) => (v == null ? '–' : `${+v.toPrecision(3)}`) }],
    win: winFn,
  });
  const ohChart = linesCard(scope, { title: t('sy.oh.title'), panels: [{ id: 'oh', yFmt: (v) => `${+v.toFixed(1)}`, valueFmt: fmtMs }], win: winFn });
  const tilesHost = h('div');
  const fbCard = card(t('sy.fb.title'), h('div', { class: 'stack-v' }, h('p', { class: 'hint' }, t('sy.fb.lead')), tilesHost, h('div', { class: 'grid two' }, fbChart.el, ohChart.el)));
  const reasonName = (r) => {
    const k = `sy.fb.r_${r}`;
    const s = t(k);
    return s === k ? r || t('sy.fb.unknown') : s;
  };
  function updateFallbacks(series, meta) {
    const fb = fallbackModel(seriesOfKey(series, 'sy.fallbacks_total'));
    const oh = overheadModel(seriesOfKey(series, 'sy.routing_overhead_ms'));
    tilesHost.replaceChildren(
      h(
        'div',
        { class: 'stats' },
        stat(t('sy.fb.total'), fb.total == null ? '–' : fmtInt(fb.total), t('sy.fb.in_window', { window: windowPhrase(win) })),
        ...fb.rows.slice(0, 4).map((r) => stat(reasonName(r.reason), fmtInt(r.value), t('sy.fb.reason'))),
        stat(t('sy.oh.p50'), oh.p50?.latest == null ? '–' : fmtMs(oh.p50.latest), oh.p50?.mean == null ? null : t('sy.oh.mean', { value: fmtMs(oh.p50.mean) })),
        stat(t('sy.oh.p99'), oh.p99?.latest == null ? '–' : fmtMs(oh.p99.latest), oh.p99?.mean == null ? null : t('sy.oh.mean', { value: fmtMs(oh.p99.mean) })),
      ),
    );
    // a fallback is rare: its rate is shown per minute when the busiest reason is below 0.1 a second (the card says which)
    const raw = fb.rows.map((r) => rateSegments(r.series.points, meta.stepS));
    const scale = pickRateScale(raw);
    const fbLines = fb.rows.map((r, i) => ({ name: reasonName(r.reason), idx: i, segments: scaleSegments(raw[i], scale.factor) })).filter((l) => l.segments.some((s) => s.length));
    const ohLines = ['p50', 'p99'].filter((q) => oh[q]).map((q, i) => ({ name: q, idx: i, segments: valueSegments(oh[q].series.points, meta.stepS) }));
    fbChart.update({
      from: meta.from,
      to: meta.to,
      panels: [{ lines: fbLines, valueFmt: (v) => (v == null ? '–' : `${+v.toPrecision(3)}${scale.per === 'min' ? '/min' : '/s'}`) }],
      caption: scale.per === 'min' ? t('series.per_min') : t('series.per_s'),
      note: fb.rows.some((r) => r.resets > 0) ? t('sy.fb.reset_note') : null,
    });
    ohChart.update({ from: meta.from, to: meta.to, panels: [{ lines: ohLines }] });
  }

  // ------------------------------------------------------------------------------------------------------------ client outcomes
  const outHost = h('div');
  function updateOutcomes(series) {
    const rows = outcomeRows(seriesOfKey(series, 'sy.client_responses_total'));
    const data = Object.fromEntries(rows.map((r) => [r.outcome || t('sy.out.unknown'), r.value]));
    repaint(outHost, () =>
      rows.length
        ? shareCard(t('sy.out.title'), data, t('unit.requests'), { note: t('sy.out.lead', { window: windowPhrase(win) }) })
        : card(t('sy.out.title'), emptyBox(t('sy.out.none'), t('sy.out.none_hint'))),
    );
  }

  // ------------------------------------------------------------------------------------------------------------ load + render
  const noteHost = h('div', { class: 'stack-v' });
  const stateHost = h('div');
  const body = h('div', { class: 'stack-v', hidden: true }, flowCardEl, modelsHost, heatCard, fbCard, h('div', { class: 'grid two' }, flowShares, outHost));

  function render() {
    if (!answer) return;
    const series = normalizeAnswer(answer);
    const meta = { from: answer.from, to: answer.to, stepS: answer.step_s };
    const routeCalls = routeCallsFromSnapshot(snap?.available ? snap.points : []);
    fillOptions(filterOptions({ routeCalls, decisions: seriesOfKey(series, 'sy.decisions_total'), modelSeries: seriesOfKey(series, 'sy.model.calls') }));
    swap(noteHost, answer.truncated ? h('p', { class: 'note info', role: 'status' }, t('series.truncated')) : null);
    swap(stateHost);
    body.hidden = false;
    updateFlow(series, routeCalls);
    updateModels(series);
    updateHeat(series, meta);
    updateFallbacks(series, meta);
    updateOutcomes(series);
  }

  async function load() {
    const my = ++seq;
    if (!answer) swap(stateHost, skeleton(4));
    try {
      const [a, m] = await Promise.all([scope.api.series('switchyard', { keys: FLOW_KEYS, window: win, split: true }), scope.api.metrics('switchyard').catch(() => null)]);
      if (!scope.alive || my !== seq) return;
      answer = a;
      snap = m;
      render();
    } catch (e) {
      if (!scope.alive || my !== seq) return;
      if (!answer) swap(stateHost, errorBox(e, load));
      else swap(noteHost, h('p', { class: 'note warn', role: 'status' }, describeError(e)));
    }
  }

  load();
  scope.every(REFRESH_MS, load);
  return h('div', { class: 'stack-v' }, h('h2', t('sy.flow.page')), controls, noteHost, stateHost, body);
}
