// Analytics tab: one /analytics call drives KPI tiles, time series, a per-model table, histograms, two heatmaps, a
// scatter, the routing panel (with a route -> decision -> served-model flow), tools and errors. The same window, anchor
// and filter controls as the Traces tab; the data refreshes politely and re-renders only when it changed.
import { h } from '../dom.js';
import { t } from '../i18n.js';
import { fmtInt, fmtCompact } from '../format.js';
import { describeError } from '../errors.js';
import { emptyBox, errorBox, skeleton, stat } from '../components/ui.js';
import { fmtDur, fmtTok, fmtPercent, fmtRate, fmtStamp } from '../viz/fmt.js';
import { fmtOffsetMs } from '../viz/scale.js';
import { createNav } from './nav.js';
import { liveLoad } from './live.js';
import { indexStrip, indexGate } from './index-strip.js';
import { rangeControls, facetSelect, resetButton } from './controls.js';
import { legendList, dataTable, chartEmpty, inlineBar, swap, dash, kindChip } from './ui.js';
import { navLink } from './nav.js';
import { fitted } from './fit.js';
import { chartCard as sharedChartCard } from '../charts/card.js';
import { stackedChart, lineChart, histogramChart, activityHeatmap, latencyHeatmap, scatterChart, flowChart } from './charts.js';

const FILTERS = ['client', 'model', 'kind'];
const pms = (v) => (v == null ? '–' : fmtDur(v));

let fitScope = null; // a scope per render, so charts redrawn on resize are released with the data they showed
const chartCard = (o) => sharedChartCard(fitScope, o);
const panel = (title, ...body) => h('section', { class: 'card', 'aria-label': title }, h('header', { class: 'card-head' }, h('h3', title)), h('div', { class: 'card-body' }, ...body));
const table = (cols, rows, { cls = '', label } = {}) =>
  h(
    'div',
    { class: 'tablewrap' },
    h(
      'table',
      { class: `compact ${cls}`.trim(), 'aria-label': label },
      h(
        'thead',
        h(
          'tr',
          cols.map((c, i) => h('th', { scope: 'col', class: c.right ? 'al-right' : '' }, c.label)),
        ),
      ),
      h('tbody', rows),
    ),
  );
const td = (v, right = true) => h('td', { class: right ? 'num al-right' : '' }, v ?? dash());

export function analyticsTab(scope, ctx) {
  const nav = createNav('analytics', ctx);
  const body = h('div', { class: 'stack-v', 'aria-busy': 'true' }, skeleton(6));
  const noteHost = h('div');
  let gate = null,
    controls = null;
  const refreshAll = () => {
    live.forget();
    live.refresh();
  };
  const fEls = {
    client: facetSelect({ name: 'client', label: t('rt.f.client'), nav, onChange: refreshAll }),
    model: facetSelect({ name: 'model', label: t('rt.f.model'), nav, onChange: refreshAll }),
    kind: facetSelect({ name: 'kind', label: t('rt.f.kind'), nav, onChange: refreshAll }),
  };
  const reset = resetButton(() => {
    nav.set({ client: null, model: null, kind: null });
    for (const el of Object.values(fEls)) el.update(el.facet);
    refreshAll();
  });
  const loadFacets = () =>
    scope.api
      .traces({ window: nav.get('window'), anchor: nav.get('anchor'), limit: 1 })
      .then((d) => {
        if (!scope.alive) return;
        for (const [k, el] of Object.entries(fEls)) {
          el.facet = d.facets?.[k];
          el.update(el.facet);
        }
      })
      .catch(() => {});
  const strip = indexStrip(scope, nav, {
    onIndex: (ix, meta) => {
      const g = indexGate(ix, nav);
      const was = gate;
      gate = g;
      if (controls) controls.hidden = !!g;
      if (!!g !== !!was || (g && meta.changed)) {
        // the gate replaces the charts (not loading: clear aria-busy); when it lifts, show the skeleton again so the
        // next answer always renders, changed or not
        if (g) {
          swap(body, g);
          body.removeAttribute('aria-busy');
        } else {
          swap(body, skeleton(6));
          body.setAttribute('aria-busy', 'true');
          live.refresh();
        }
      }
    },
  });
  const query = () => ({ window: nav.get('window'), anchor: nav.get('anchor'), ...Object.fromEntries(FILTERS.filter((k) => nav.get(k)).map((k) => [k, nav.get(k)])) });

  const live = liveLoad(scope, {
    load: (signal) => scope.api.analytics(query(), { signal }),
    ms: 20000,
    enabled: () => !gate,
    onData: (a, { changed }) => {
      if (gate) return;
      swap(noteHost);
      if (changed || body.getAttribute('aria-busy') === 'true') {
        render(a);
        body.removeAttribute('aria-busy');
      }
    },
    onError: (e, { quiet }) => {
      if (quiet && body.getAttribute('aria-busy') !== 'true') swap(noteHost, h('p', { class: 'note warn', role: 'status' }, t('rt.refresh_failed', { why: describeError(e) })));
      else swap(body, errorBox(e, refreshAll));
    },
  });
  live.refresh();
  loadFacets();

  function render(a) {
    fitScope?.dispose();
    fitScope = scope.child();
    const T = a.totals || {};
    const empty = !T.llm_calls && !T.traces;
    if (empty) {
      swap(body, emptyBox(t('rt.an.empty'), t('rt.an.empty_hint')));
      return;
    }
    const s = a.series || {};
    const modelCls = (() => {
      const names = (a.by_model || []).map((m) => m.model);
      return (m) => `bf${(Math.max(0, names.indexOf(m)) % 6) + 1}`;
    })();
    const scCls = (m) => `sc${modelCls(m).slice(2)}`;
    const common = { ts: s.ts, bucketS: a.bucket_s };
    const okCalls = (s.llm_calls || []).map((v, i) => Math.max(0, v - (s.errors?.[i] || 0)));
    const unc = (s.input_tokens || []).map((v, i) => Math.max(0, v - (s.cached_tokens?.[i] || 0)));
    const rangeText = a.range ? t('rt.an.range', { from: fmtStamp(a.range.from), to: fmtStamp(a.range.to), bucket: fmtDur(a.bucket_s * 1000) }) : '';

    const kpis = h(
      'div',
      { class: 'stats kpis' },
      stat(t('rt.an.k_traces'), fmtInt(T.traces), t('rt.an.k_sessions', { n: fmtInt(T.sessions) })),
      stat(t('rt.an.k_llm'), fmtInt(T.llm_calls), t('rt.an.k_tools', { n: fmtInt(T.tool_calls) })),
      stat(t('rt.an.k_errors'), fmtInt(T.errors), T.error_rate != null ? t('rt.an.k_rate', { pct: fmtPercent(T.error_rate, 1) }) : null),
      stat(t('rt.an.k_tokens_in'), fmtTok(T.input_tokens), t('rt.an.k_cached', { n: fmtTok(T.cached_tokens) })),
      stat(t('rt.an.k_cache'), fmtPercent(T.cache_hit_ratio, 1), t('rt.an.k_cache_sub')),
      stat(t('rt.an.k_tokens_out'), fmtTok(T.output_tokens), t('rt.an.k_reasoning', { n: fmtTok(T.reasoning_tokens) })),
      stat(t('rt.an.k_p50'), pms(T.p50_ms), t('rt.an.k_p95_sub')),
      stat(t('rt.an.k_p95_title'), pms(T.p95_ms), t('rt.an.k_p99', { p99: pms(T.p99_ms) })),
      stat(t('rt.an.k_ttfc'), pms(T.ttfc_p50_ms), T.ttfc_p95_ms != null ? t('rt.an.k_ttfc95', { p95: pms(T.ttfc_p95_ms) }) : null),
      stat(t('rt.an.k_tps'), T.tokens_per_s_p50 != null ? fmtRate(T.tokens_per_s_p50) : '–', t('rt.an.k_tps_sub')),
    );

    const tsRows = (cols) => () => (s.ts || []).map((tsv, i) => [fmtStamp(tsv), ...cols.map((c) => (c[i] == null ? null : Math.round(c[i] * 10) / 10))]);
    const series1 = chartCard({
      title: t('rt.an.c_requests'),
      hint: rangeText,
      build: (w) =>
        stackedChart({
          width: w,
          ...common,
          series: [
            { key: 'ok', label: t('rt.an.ok'), cls: 'bf1', values: okCalls },
            { key: 'err', label: t('rt.status.error'), cls: 'bf-bad', values: s.errors || [] },
          ],
          ariaLabel: t('rt.an.c_requests_alt', { total: fmtInt(T.llm_calls), errors: fmtInt(T.errors) }),
        }),
      legend: [
        { cls: 'bf1', label: t('rt.an.ok') },
        { cls: 'bf-bad', label: t('rt.status.error') },
      ],
      table: dataTable(t('chart.data_table'), [t('chart.time'), t('rt.an.ok'), t('rt.status.error')], tsRows([okCalls, s.errors || []])),
      empty: t('rt.an.no_series'),
    });
    const series2 = chartCard({
      title: t('rt.an.c_tokens'),
      hint: t('rt.an.c_tokens_hint'),
      build: (w) =>
        stackedChart({
          width: w,
          ...common,
          series: [
            { key: 'unc', label: t('rt.ctx.lg_in'), cls: 'bf1', values: unc },
            { key: 'cac', label: t('rt.ctx.lg_cached'), cls: 'bf2', values: s.cached_tokens || [] },
            { key: 'out', label: t('rt.ctx.lg_out'), cls: 'bf4', values: s.output_tokens || [] },
          ],
          ariaLabel: t('rt.an.c_tokens_alt', { input: fmtTok(T.input_tokens), output: fmtTok(T.output_tokens), cached: fmtTok(T.cached_tokens) }),
        }),
      legend: [
        { cls: 'bf1', label: t('rt.ctx.lg_in') },
        { cls: 'bf2', label: t('rt.ctx.lg_cached') },
        { cls: 'bf4', label: t('rt.ctx.lg_out') },
      ],
      table: dataTable(t('chart.data_table'), [t('chart.time'), t('rt.ctx.lg_in'), t('rt.ctx.lg_cached'), t('rt.ctx.lg_out')], tsRows([unc, s.cached_tokens || [], s.output_tokens || []])),
      empty: t('rt.an.no_series'),
    });
    const series3 = chartCard({
      title: t('rt.an.c_latency'),
      hint: t('rt.an.c_latency_hint'),
      build: (w) =>
        lineChart({
          width: w,
          ...common,
          series: [
            { key: 'p50', label: 'p50', cls: 'ls1', values: s.p50_ms || [] },
            { key: 'p95', label: 'p95', cls: 'ls3', dash: 'dash', values: s.p95_ms || [] },
            { key: 'ttfc', label: t('rt.an.ttfc_p50'), cls: 'ls2', dash: 'dot', values: s.ttfc_p50_ms || [] },
          ],
          ariaLabel: t('rt.an.c_latency_alt', { p50: pms(T.p50_ms), p95: pms(T.p95_ms), ttfc: pms(T.ttfc_p50_ms) }),
        }),
      legend: [
        { cls: 'ls1', label: 'p50', line: 'solid' },
        { cls: 'ls3', label: 'p95', line: 'dash' },
        { cls: 'ls2', label: t('rt.an.ttfc_p50'), line: 'dot' },
      ],
      table: dataTable(t('chart.data_table'), [t('chart.time'), 'p50 (ms)', 'p95 (ms)', `${t('rt.an.ttfc_p50')} (ms)`], tsRows([s.p50_ms || [], s.p95_ms || [], s.ttfc_p50_ms || []])),
      empty: t('rt.an.no_series'),
    });
    const bm = s.by_model || [];
    const series4 = chartCard({
      title: t('rt.an.c_models'),
      build: (w) =>
        stackedChart({
          width: w,
          ...common,
          series: bm.map((m) => ({ key: m.model, label: m.model, cls: modelCls(m.model), values: m.llm_calls })),
          ariaLabel: t('rt.an.c_models_alt', { n: bm.length }),
        }),
      legend: bm.map((m) => ({ cls: modelCls(m.model), label: m.model })),
      table: dataTable(t('chart.data_table'), [t('chart.time'), ...bm.map((m) => m.model)], tsRows(bm.map((m) => m.llm_calls))),
      empty: t('rt.an.no_series'),
    });

    // per-model table
    const models = a.by_model || [];
    const mx = (k) => Math.max(1, ...models.map((m) => m[k] ?? 0));
    const modelTable = panel(
      t('rt.an.models'),
      models.length
        ? table(
            [
              { label: t('rt.f.model') },
              { label: t('rt.an.m_calls'), right: true },
              { label: t('rt.col.errors'), right: true },
              { label: t('rt.sd.tok_in'), right: true },
              { label: t('rt.sd.tok_out'), right: true },
              { label: t('rt.sd.tok_cached'), right: true },
              { label: 'p50', right: true },
              { label: 'p95', right: true },
              { label: t('rt.an.m_ttfc'), right: true },
              { label: t('rt.an.m_tps'), right: true },
            ],
            models.map((m) =>
              h(
                'tr',
                h('td', h('i', { class: `sw ${modelCls(m.model)}`, 'aria-hidden': 'true' }), ' ', h('code', m.model)),
                h('td', { class: 'num al-right' }, h('div', { class: 'cell-bar' }, inlineBar(m.llm_calls, mx('llm_calls'), 'bar-calls'), h('span', fmtInt(m.llm_calls)))),
                td(m.errors ? h('span', { class: 'bad-text' }, `✕ ${fmtInt(m.errors)}`) : '0'),
                td(fmtTok(m.input_tokens)),
                td(fmtTok(m.output_tokens)),
                td(fmtTok(m.cached_tokens)),
                td(pms(m.p50_ms)),
                h('td', { class: 'num al-right' }, h('div', { class: 'cell-bar' }, inlineBar(m.p95_ms, mx('p95_ms'), 'bar-lat'), h('span', pms(m.p95_ms)))),
                td(pms(m.ttfc_p50_ms)),
                td(m.tokens_per_s_p50 != null ? fmtRate(m.tokens_per_s_p50) : null),
              ),
            ),
            { label: t('rt.an.models') },
          )
        : chartEmpty(),
    );
    const breakdown = panel(
      t('rt.an.breakdown'),
      h(
        'div',
        { class: 'grid two' },
        table(
          [{ label: t('rt.f.client') }, { label: t('rt.col.traces'), right: true }, { label: t('rt.an.m_calls'), right: true }, { label: t('rt.col.errors'), right: true }],
          (a.by_client || []).map((c) => h('tr', h('td', c.client ?? t('rt.unknown_client')), td(fmtInt(c.traces)), td(fmtInt(c.llm_calls)), td(fmtInt(c.errors)))),
          { label: t('rt.f.client') },
        ),
        table(
          [{ label: t('rt.f.kind') }, { label: t('rt.col.traces'), right: true }, { label: t('rt.an.m_calls'), right: true }, { label: t('rt.col.errors'), right: true }],
          (a.by_kind || []).map((c) => h('tr', h('td', kindChip(c.kind)), td(fmtInt(c.traces)), td(fmtInt(c.llm_calls)), td(fmtInt(c.errors)))),
          { label: t('rt.f.kind') },
        ),
      ),
    );

    // histograms
    const H = a.histograms || {};
    const hist = (key, title, { xFmt, markers, cls = 'bf1' }) => {
      const hh = H[key];
      return chartCard({
        title,
        build: (w) => histogramChart({ width: w, hist: hh, markers, xFmt, barCls: cls, ariaLabel: t('rt.hist.alt', { title, total: fmtInt((hh?.counts || []).reduce((x, y) => x + y, 0)) }) }),
        table: dataTable(t('chart.data_table'), [t('rt.hist.range'), t('rt.hist.count')], () => (hh?.counts || []).map((c, i) => [`${xFmt(hh.edges[i])} – ${xFmt(hh.edges[i + 1])}`, c])),
        empty: t('rt.an.no_hist'),
      });
    };
    const ms = (v) => fmtOffsetMs(v);
    const histograms = h(
      'div',
      { class: 'grid three hist-grid' },
      hist('duration_ms', t('rt.an.h_duration'), { xFmt: ms, markers: { p50: T.p50_ms, p95: T.p95_ms } }),
      hist('ttfc_ms', t('rt.an.h_ttfc'), { xFmt: ms, markers: { p50: T.ttfc_p50_ms, p95: T.ttfc_p95_ms }, cls: 'bf2' }),
      hist('input_tokens', t('rt.an.h_input'), { xFmt: (v) => fmtCompact(v), cls: 'bf1' }),
      hist('output_tokens', t('rt.an.h_output'), { xFmt: (v) => fmtCompact(v), cls: 'bf4' }),
      hist('tokens_per_s', t('rt.an.h_tps'), { xFmt: (v) => String(Math.round(v)), markers: { p50: T.tokens_per_s_p50 }, cls: 'bf3' }),
      hist('chunk_gap_ms', t('rt.an.h_gap'), { xFmt: ms, cls: 'bf6' }),
    );

    // heatmaps
    const heatKey = h('div', { class: 'heat-key small muted' }, t('rt.heat.fewer'), ...[0, 1, 2, 3, 4].map((n) => h('i', { class: `heat-sw heat${n}`, 'aria-hidden': 'true' })), t('rt.heat.more'));
    const act = chartCard({
      title: t('rt.an.c_activity'),
      hint: t('rt.an.c_activity_hint'),
      build: (w) => activityHeatmap(a.heatmap?.activity, { width: w, ariaLabel: t('rt.an.c_activity_alt', { days: a.heatmap?.activity?.days?.length ?? 0 }) }),
      extra: heatKey.cloneNode(true),
      table: dataTable(t('chart.data_table'), [t('rt.heat.day'), ...Array.from({ length: 24 }, (_, i) => String(i).padStart(2, '0'))], () =>
        (a.heatmap?.activity?.days || []).map((d, i) => [d, ...a.heatmap.activity.counts[i]]),
      ),
      empty: t('rt.an.no_hist'),
    });
    const lat = chartCard({
      title: t('rt.an.c_latency_heat'),
      hint: t('rt.an.c_latency_heat_hint'),
      build: (w) => latencyHeatmap(a.heatmap?.latency, { width: w, ariaLabel: t('rt.an.c_latency_heat_alt') }),
      extra: heatKey,
      table: null,
      empty: t('rt.an.no_hist'),
    });

    const sc = chartCard({
      title: t('rt.an.c_scatter'),
      hint: t('rt.an.c_scatter_hint', { n: fmtInt((a.scatter || []).length) }),
      build: (w) =>
        scatterChart(a.scatter || [], {
          width: w,
          height: w < 520 ? 260 : 300,
          modelClass: scCls,
          onOpen: (d) => nav.go({ trace: d.trace_id, span: d.span_id, view: null }, 'traces'),
          ariaLabel: t('rt.an.c_scatter_alt', { n: fmtInt((a.scatter || []).length) }),
        }),
      legend: [...[...new Set((a.scatter || []).map((p) => p.model))].map((m) => ({ cls: `sc-sw ${scCls(m)}`, label: m ?? '–' })), { cls: 'sc-sw sc-errsw', label: t('rt.status.error') }],
      table: dataTable(t('chart.data_table'), [t('rt.f.model'), t('rt.col.duration'), t('rt.sd.tok_in'), t('rt.sd.tok_out'), t('rt.col.status')], () =>
        (a.scatter || []).slice(0, 150).map((p) => [p.model, fmtDur(p.duration_ms), fmtInt(p.input_tokens), fmtInt(p.output_tokens), p.status]),
      ),
      wide: true,
      empty: t('rt.an.no_hist'),
    });

    // routing
    const R = a.routing || {};
    const routing = panel(
      t('rt.an.routing'),
      R.requests
        ? h(
            'div',
            { class: 'stack-v' },
            h(
              'div',
              { class: 'stats' },
              stat(t('rt.an.r_requests'), fmtInt(R.requests), (R.by_algorithm || []).map((x) => `${x.algorithm} ${fmtInt(x.count)}`).join(' · ')),
              stat(t('rt.an.r_overhead'), pms(R.overhead_p50_ms), t('rt.an.k_ttfc95', { p95: pms(R.overhead_p95_ms) })),
              stat(t('rt.an.r_judge'), fmtInt(R.judge?.calls), t('rt.an.r_judge_sub', { errors: fmtInt(R.judge?.errors), p50: pms(R.judge?.p50_ms), p95: pms(R.judge?.p95_ms) })),
              stat(t('rt.an.r_failures'), fmtInt((R.failures || []).reduce((x, f) => x + f.count, 0))),
            ),
            h(
              'div',
              { class: 'grid two' },
              fitted(fitScope, (w) => flowChart(R.flows || [], { width: w, height: w < 520 ? 290 : 240, ariaLabel: t('rt.an.flow_alt', { n: (R.flows || []).length }) }), chartEmpty()),
              h(
                'div',
                { class: 'stack-v' },
                table(
                  [{ label: t('rt.routing.route') }, { label: t('rt.routing.decision') }, { label: t('rt.routing.served') }, { label: t('rt.an.m_calls'), right: true }],
                  (R.decisions || []).map((d) => h('tr', h('td', h('code', d.route ?? '–')), h('td', d.decision ?? '–'), h('td', h('code', d.served_model ?? '–')), td(fmtInt(d.count)))),
                  { label: t('rt.an.decisions') },
                ),
                (R.failures || []).length
                  ? table(
                      [{ label: t('rt.an.f_kind') }, { label: t('rt.an.f_phase') }, { label: t('rt.an.m_calls'), right: true }],
                      R.failures.map((f) => h('tr', h('td', h('code', f.kind)), h('td', f.phase), td(fmtInt(f.count)))),
                      { label: t('rt.an.r_failures') },
                    )
                  : h('p', { class: 'hint' }, t('rt.an.no_failures')),
              ),
            ),
            dataTable(t('chart.data_table'), [t('rt.flow.from'), t('rt.flow.to'), t('rt.hist.count')], () => (R.flows || []).map((f) => [f.source, f.target, f.value])),
          )
        : chartEmpty(t('rt.an.no_routing')),
    );

    // tools
    const tools = a.tools || [];
    const mxT = Math.max(1, ...tools.map((x) => x.calls)),
      mxR = Math.max(1, ...(a.tool_requests || []).map((x) => x.count));
    const toolsPanel = panel(
      t('rt.an.tools'),
      h(
        'div',
        { class: 'grid two' },
        h(
          'div',
          h('h4', { class: 'sub' }, t('rt.an.t_executed')),
          tools.length
            ? table(
                [{ label: t('rt.sd.tool_name') }, { label: t('rt.an.m_calls'), right: true }, { label: t('rt.col.errors'), right: true }, { label: 'p50', right: true }, { label: 'p95', right: true }],
                tools.map((x) =>
                  h(
                    'tr',
                    h('td', h('code', x.name)),
                    h('td', { class: 'num al-right' }, h('div', { class: 'cell-bar' }, inlineBar(x.calls, mxT, 'bar-calls'), h('span', fmtInt(x.calls)))),
                    td(x.errors ? h('span', { class: 'bad-text' }, `✕ ${fmtInt(x.errors)}`) : '0'),
                    td(pms(x.p50_ms)),
                    td(pms(x.p95_ms)),
                  ),
                ),
                { label: t('rt.an.t_executed') },
              )
            : chartEmpty(t('rt.an.no_tools')),
        ),
        h(
          'div',
          h('h4', { class: 'sub' }, t('rt.an.t_requested')),
          (a.tool_requests || []).length
            ? table(
                [{ label: t('rt.sd.tool_name') }, { label: t('rt.an.t_times'), right: true }],
                a.tool_requests.map((x) =>
                  h('tr', h('td', h('code', x.name)), h('td', { class: 'num al-right' }, h('div', { class: 'cell-bar' }, inlineBar(x.count, mxR, 'bar-req'), h('span', fmtInt(x.count))))),
                ),
                { label: t('rt.an.t_requested') },
              )
            : chartEmpty(t('rt.an.no_tools')),
        ),
      ),
    );

    // errors
    const E = a.errors || {};
    const rank = (rows, key, cls) => {
      const m = Math.max(1, ...rows.map((r) => r.count));
      return rows.length
        ? table(
            [{ label: t(`rt.an.e_${key}`) }, { label: t('rt.an.t_times'), right: true }],
            rows.map((r) => h('tr', h('td', h('code', String(r[key]))), h('td', { class: 'num al-right' }, h('div', { class: 'cell-bar' }, inlineBar(r.count, m, cls), h('span', fmtInt(r.count)))))),
            { label: t(`rt.an.e_${key}`) },
          )
        : chartEmpty(t('rt.an.no_errors'));
    };
    const errorsPanel = panel(
      t('rt.an.errors'),
      h(
        'div',
        { class: 'grid three' },
        h('div', h('h4', { class: 'sub' }, t('rt.an.e_by_type')), rank(E.by_type || [], 'type', 'bar-err')),
        h('div', h('h4', { class: 'sub' }, t('rt.an.e_by_status')), rank(E.by_status || [], 'status', 'bar-err')),
        h('div', h('h4', { class: 'sub' }, t('rt.an.e_finish')), rank(E.finish_reasons || [], 'reason', 'bar-calls')),
      ),
      h('h4', { class: 'sub' }, t('rt.an.e_recent')),
      (E.recent || []).length
        ? table(
            [{ label: t('rt.col.start') }, { label: t('rt.f.model') }, { label: t('rt.an.e_type') }, { label: t('rt.an.e_trace') }],
            E.recent.map((r) =>
              h(
                'tr',
                h('td', { class: 'num nowrap' }, fmtStamp(r.ts)),
                h('td', r.model ?? dash()),
                h('td', h('code', r.type)),
                h('td', navLink(nav, { trace: r.trace_id, span: r.span_id, view: null }, { class: 'row-link' }, h('code', r.trace_id.slice(0, 8)), 'traces')),
              ),
            ),
            { label: t('rt.an.e_recent') },
          )
        : chartEmpty(t('rt.an.no_errors')),
    );

    swap(
      body,
      kpis,
      h('div', { class: 'grid two' }, series1, series2, series3, series4),
      modelTable,
      breakdown,
      histograms,
      h('div', { class: 'grid two' }, act, lat),
      sc,
      routing,
      toolsPanel,
      errorsPanel,
    );
  }

  controls = h(
    'div',
    { class: 'stack-v' },
    h(
      'div',
      { class: 'rt-toolbar' },
      rangeControls(nav, () => (loadFacets(), refreshAll())),
    ),
    h('div', { class: 'rt-filters' }, fEls.client, fEls.model, fEls.kind, h('div', { class: 'field filter end' }, reset)),
  );
  return h('div', { class: 'stack-v rt-tab' }, strip.el, controls, noteHost, body);
}
