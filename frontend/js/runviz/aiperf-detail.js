// AIPerf run detail (addendum 9.15), hung on the run history's selection (A and B): the throughput-vs-latency scatter over the runs shown, percentile curves
// (p1..p99) of time to first token, inter-token latency and request latency for run A and, when B is chosen, B overlaid (A solid, B dashed), the HTTP-phase bar
// and, for run A, the per-request view (a latency histogram, the requests as swim lanes by worker, error codes as bars). Data: `/runs/{id}/summary` and
// `/runs/{id}/requests` (viewer routes: numbers and identifiers only), one fetch per run kept for the page's life; the scatter needs only the run events.
// Every chart says its numbers include the router hop. A figure a run lacks is an en dash, never 0. Words: i18n keys `apv.*`, `rv.*`, `viz.*`.
import { h } from '../dom.js';
import { t } from '../i18n.js';
import { fmtMs, fmtInt } from '../format.js';
import { fmtOffsetMs } from '../viz/scale.js';
import { fmtDur } from '../viz/fmt.js';
import { fmtRunValue, fmtAxis } from '../runs.js';
import {
  percentileCurves,
  httpPhases,
  summaryFacts,
  PCT_KEYS,
  HTTP_PHASES,
  cleanRequests,
  requestHistogram,
  swimLanes,
  errorCodes,
  REQUEST_MEASURES,
  throughputPoints,
  SCATTER_X,
} from '../aiperf-model.js';
import { curveChart, pointsChart, markerKey, RUN_STYLE, styleSwatch } from '../charts/curve-chart.js';
import { histogramChart } from '../charts/svg-charts.js';
import { hbarChart } from '../charts/bar-charts.js';
import { swimChart } from '../charts/swim-chart.js';
import { stackCard } from '../charts/stack-card.js';
import { dataTable, chartEmpty } from '../charts/parts.js';
import { vizCard, chartPanel, note, loadingBlock, failBlock, fetchCache, routeId, enc, repaintKeepFocus, pickerBar } from './kit.js';

const SYS = 'aiperf';
const fmtPctVal = (v) => (v == null ? '–' : fmtMs(v));

/** The router-hop sentence every chart of this page carries. */
const routerHop = () => h('p', { class: 'muted small rv-hop' }, t('apv.router_hop'));

export function aiperfDetail(scope, { hash }) {
  const el = h('div', { class: 'stack-v rv-detail' });
  const summaries = fetchCache((id) => scope.api.get(`/systems/${SYS}/runs/${enc(id)}/summary`));
  const requests = fetchCache((id) => scope.api.get(`/systems/${SYS}/runs/${enc(id)}/requests`));
  let ctx = null,
    rs = null;
  let measure = REQUEST_MEASURES.some((m) => m.id === hash.get('rq')) ? hash.get('rq') : 'latency';
  let xField = SCATTER_X.some((x) => x.id === hash.get('sx')) ? hash.get('sx') : 'ttft_p50_ms';
  const again = () => {
    if (scope.alive && ctx) paint();
  };

  // ---- the scatter over the runs shown (event data only)
  function scatterCard() {
    const rows = ctx.view;
    const { points, groups, skipped } = throughputPoints(rows, xField);
    const byId = new Map(rows.map((r) => [r.id, r]));
    const xs = points.map((p) => p.x);
    const logX = xs.length > 1 && Math.max(...xs) / Math.min(...xs) > 10;
    const label = (id) => byId.get(id)?.label ?? id;
    const pts = points.map((p) => ({
      ...p,
      text: t('apv.scatter_tip', { run: label(p.id), conc: p.concurrency == null ? '–' : p.concurrency, x: fmtRunValue(p.x, 'ms'), y: fmtRunValue(p.y, 'tps') }),
    }));
    const xName = t(`rh.n.${xField}`);
    const alt = t('apv.scatter_alt', { count: points.length, x: xName });
    const picker = pickerBar({
      label: t('apv.scatter_x'),
      values: SCATTER_X.map((x) => x.id),
      current: xField,
      labelOf: (v) => t(`apv.sx.${v}`),
      id: 'sx',
      onPick: (v) => {
        xField = v;
        hash.set({ sx: v === 'ttft_p50_ms' ? null : v });
        paint();
      },
    });
    const build = (w) =>
      pointsChart({
        points: pts,
        width: w,
        height: w < 420 ? 240 : 290,
        logX,
        xFmt: (v) => fmtAxis(v, 'ms'),
        yFmt: (v) => fmtAxis(v, 'num'),
        xTitle: xName,
        yTitle: t('apv.scatter_y'),
        a: ctx.A?.id,
        b: ctx.B?.id,
        onPick: (id, o) => ctx.pick(id, o.compare),
        ariaLabel: alt,
      }).svg;
    return vizCard(
      { title: t('apv.scatter_title'), hint: t('apv.scatter_hint'), cls: 'rv-scatter' },
      routerHop(),
      h('div', { class: 'row gap wrap rv-controls' }, picker),
      chartPanel(rs, { build, empty: chartEmpty(t('apv.scatter_empty')) }),
      points.length
        ? h(
            'ul',
            { class: 'legend' },
            groups.map((g) => h('li', markerKey(g.g), h('span', t('apv.conc_key', { conc: g.value == null ? '–' : g.value, count: g.count })))),
          )
        : null,
      skipped ? h('p', { class: 'muted small' }, t('apv.scatter_skipped', { count: skipped })) : null,
      points.length
        ? dataTable(t('chart.data_table'), [t('rh.col_run'), t('apv.col_conc'), xName, t('rh.n.output_tps')], () =>
            points.map((p) => [label(p.id), p.concurrency ?? '–', fmtRunValue(p.x, 'ms'), fmtRunValue(p.y, 'tps')]),
          )
        : null,
    );
  }

  // ---- the percentile curves of A (and B)
  function curvesCard(sa, sb) {
    const curves = percentileCurves(sa.value, sb?.value ?? null);
    const hasB = !!ctx.B;
    const legend = h('ul', { class: 'legend' }, h('li', styleSwatch(RUN_STYLE.a), h('span', `A · ${ctx.A.label}`)), hasB ? h('li', styleSwatch(RUN_STYLE.b), h('span', `B · ${ctx.B.label}`)) : null);
    const panels = curves.map((c) => {
      const lines = [
        c.a ? { key: 'a', name: `A · ${ctx.A.label}`, style: RUN_STYLE.a, values: c.a.values } : null,
        c.b ? { key: 'b', name: `B · ${ctx.B.label}`, style: RUN_STYLE.b, values: c.b.values } : null,
      ].filter(Boolean);
      const title = t(`apv.metric.${c.id}`);
      const alt = t('apv.curve_alt', { metric: title, runs: lines.map((l) => l.name).join(' / ') });
      return chartPanel(rs, {
        title,
        build: (w) => curveChart({ cats: PCT_KEYS, lines, width: w, height: w < 300 ? 190 : 200, yFmt: (v) => fmtAxis(v, 'ms'), valueFmt: fmtPctVal, xTitle: t('apv.percentile'), ariaLabel: alt }),
        empty: chartEmpty(t('apv.curve_none')),
        sub: !c.a || (hasB && !c.b) ? h('p', { class: 'muted small' }, t('apv.curve_missing', { runs: [!c.a ? 'A' : null, hasB && !c.b ? 'B' : null].filter(Boolean).join(' / ') })) : null,
      });
    });
    return vizCard(
      { title: t('apv.curves_title'), hint: t('apv.curves_hint'), wide: true },
      routerHop(),
      legend,
      curves.length ? h('div', { class: 'rv-panels' }, panels) : chartEmpty(t('apv.curves_none')),
      curves.length
        ? dataTable(t('chart.data_table'), [t('apv.metric_col'), t('apv.percentile'), 'A', ...(hasB ? ['B'] : [])], () =>
            curves.flatMap((c) => PCT_KEYS.map((k, i) => [t(`apv.metric.${c.id}`), k, fmtPctVal(c.a?.values[i] ?? null), ...(hasB ? [fmtPctVal(c.b?.values[i] ?? null)] : [])])),
          )
        : null,
    );
  }

  // ---- the HTTP phases
  function phasesCard(sa, sb) {
    const parts = HTTP_PHASES.map((k, i) => ({ key: k, name: t(`apv.phase.${k}`), cls: `bf${i + 1}` }));
    const rows = [{ id: 'a', label: 'A', phases: httpPhases(sa.value) }, ...(ctx.B ? [{ id: 'b', label: 'B', phases: httpPhases(sb?.value) }] : [])].filter((r) => r.phases);
    const card = stackCard(rs, { title: t('apv.phases_title'), hint: `${t('apv.phases_hint')} ${t('apv.router_hop')}`, parts, valueFmt: fmtMs, emptyText: t('apv.phases_none') });
    card.update({ rows: rows.map((r) => ({ id: r.id, label: r.label, values: r.phases })) });
    const missing = !httpPhases(sa.value) || (ctx.B && !httpPhases(sb?.value));
    if (missing && rows.length) card.el.querySelector('.card-body').prepend(note(t('apv.phases_some_missing'), 'info'));
    return card.el;
  }

  // ---- the per-request view of A
  function requestsCard(entry) {
    const title = t('apv.req_title');
    if (entry.state === 'loading' || entry.state === 'none') return vizCard({ title, wide: true }, loadingBlock());
    if (entry.state === 'fail')
      return vizCard(
        { title, wide: true },
        failBlock(entry.error, () => {
          requests.retry(routeId(ctx.A), again);
          paint();
        }),
      );
    const req = cleanRequests(entry.value);
    if (!req.rows.length) return vizCard({ title, hint: t('apv.req_hint'), wide: true }, note(t('apv.req_none'), 'info'));
    const hist = requestHistogram(req.rows, REQUEST_MEASURES.find((m) => m.id === measure).field);
    const errs = errorCodes(req.rows);
    const lanes = swimLanes(req.rows);
    const picker = pickerBar({
      label: t('apv.req_measure'),
      values: REQUEST_MEASURES.map((m) => m.id),
      current: measure,
      labelOf: (v) => t(`apv.metric.${v}`),
      id: 'rq',
      onPick: (v) => {
        measure = v;
        hash.set({ rq: v === 'latency' ? null : v });
        paint();
      },
    });
    const measName = t(`apv.metric.${measure}`);
    const histBlock = chartPanel(rs, {
      title: t('apv.hist_title', { metric: measName }),
      build: (w) =>
        hist
          ? histogramChart({ hist, markers: { p50: hist.p50, p95: hist.p95 }, xFmt: fmtOffsetMs, ariaLabel: t('apv.hist_alt', { metric: measName, n: hist.n }), width: w, height: w < 420 ? 170 : 190 })
          : null,
      empty: chartEmpty(t('apv.hist_none', { metric: measName })),
      sub: hist ? h('p', { class: 'muted small' }, t('apv.hist_note', { n: fmtInt(hist.n), p50: fmtMs(hist.p50), p95: fmtMs(hist.p95), failed: fmtInt(hist.failed) })) : null,
    });
    const swimBlock = chartPanel(rs, {
      title: t('apv.swim_title'),
      build: (w) => swimChart({ lanes: lanes.lanes, items: lanes.items, width: w, ariaLabel: t('apv.swim_alt', { workers: lanes.workers, shown: lanes.shown }) }),
      empty: chartEmpty(t('apv.swim_none')),
      legend: h(
        'ul',
        { class: 'legend' },
        h('li', h('i', { class: 'sw sw-ok', 'aria-hidden': 'true' }), h('span', t('apv.swim_ok'))),
        h('li', h('i', { class: 'sw sw-err', 'aria-hidden': 'true' }), h('span', t('apv.swim_err'))),
      ),
      sub:
        lanes.sampled || lanes.workers > lanes.lanes.length
          ? h('p', { class: 'muted small' }, t('apv.swim_note', { shown: fmtInt(lanes.shown), workers: lanes.workers, lanes: lanes.lanes.length }))
          : null,
    });
    const codeRows = errs.codes.slice(0, 8).map((c) => ({
      id: String(c.code),
      label: c.code === 0 ? t('apv.no_code') : t('apv.http_code', { code: c.code }),
      bars: [
        {
          key: 'n',
          value: c.count,
          cls: 'hb-bad',
          text: t('apv.code_text', { code: c.code === 0 ? t('apv.no_code') : c.code, count: c.count, pct: `${(c.share * 100).toFixed(0)}%` }),
          valueText: `${c.count} · ${(c.share * 100).toFixed(0)}%`,
        },
      ],
    }));
    const errBlock = chartPanel(rs, {
      title: t('apv.err_title'),
      build: (w) => (codeRows.length ? hbarChart({ rows: codeRows, width: w, ariaLabel: t('apv.err_alt', { errors: errs.errors, total: errs.total }), axisFmt: (v) => String(v) }) : null),
      empty: h('p', { class: 'note ok', role: 'status' }, t('apv.err_none', { total: fmtInt(errs.total) })),
      sub: errs.errors ? h('p', { class: 'muted small' }, t('apv.err_note', { errors: fmtInt(errs.errors), total: fmtInt(errs.total), pct: `${(errs.rate * 100).toFixed(1)}%` })) : null,
    });
    return vizCard(
      { title, hint: t('apv.req_hint'), wide: true },
      routerHop(),
      req.truncated || req.total > req.rows.length
        ? note(t('apv.req_truncated', { returned: fmtInt(req.rows.length), total: fmtInt(req.total) }), 'info')
        : h('p', { class: 'muted small' }, t('apv.req_count', { n: fmtInt(req.rows.length) })),
      h('div', { class: 'row gap wrap rv-controls' }, picker),
      h('div', { class: 'rv-panels two' }, histBlock, errBlock),
      swimBlock,
      dataTable(t('chart.data_table'), [t('apv.col_worker'), t('apv.col_requests'), t('apv.col_failed')], () => lanes.lanes.map((l) => [`${t('viz.worker')} ${l.label}`, l.n, l.errors])),
    );
  }

  /** What selecting a run shows: its facts (the summary's own numbers), then the cards. */
  function factsLine(tag, row, entry) {
    const f = entry.state === 'ok' ? summaryFacts(entry.value) : null;
    const bits = f
      ? [
          f.model,
          f.concurrency != null ? t('apv.f_conc', { n: f.concurrency }) : null,
          f.requests != null ? t('apv.f_requests', { n: fmtInt(f.requests) }) : null,
          f.isl != null || f.osl != null ? t('apv.f_tokens', { isl: f.isl == null ? '–' : fmtInt(f.isl), osl: f.osl == null ? '–' : fmtInt(f.osl) }) : null,
          f.duration_s != null ? fmtDur(f.duration_s * 1000) : null,
        ].filter(Boolean)
      : [];
    return h('li', h('strong', tag), ' ', h('code', row.id), bits.length ? h('span', { class: 'muted' }, ` · ${bits.join(' · ')}`) : null);
  }

  function paint() {
    rs?.dispose();
    rs = scope.child();
    repaintKeepFocus(el, () => {
      const out = [h('h2', { class: 'rv-h' }, t('rv.detail')), scatterCard()];
      const { A, B } = ctx;
      if (!A) return [...out, h('p', { class: 'hint' }, t('apv.select_hint'))];
      const sa = summaries.peek(routeId(A));
      const sb = B ? summaries.peek(routeId(B)) : null;
      const facts = h('ul', { class: 'rv-facts' }, factsLine('A', A, sa), B ? factsLine('B', B, sb) : null);
      const pending = [sa, sb].filter(Boolean);
      if (pending.some((e) => e.state === 'loading' || e.state === 'none')) return [...out, facts, vizCard({ title: t('apv.curves_title'), wide: true }, loadingBlock())];
      const failed = pending.find((e) => e.state === 'fail');
      if (sa.state === 'fail')
        return [
          ...out,
          facts,
          vizCard(
            { title: t('apv.curves_title'), wide: true },
            failBlock(sa.error, () => {
              summaries.retry(routeId(A), again);
              paint();
            }),
          ),
          requestsCard(requests.peek(routeId(A))),
        ];
      return [
        ...out,
        facts,
        failed ? note(t('apv.b_failed'), 'warn') : null,
        curvesCard(sa, sb?.state === 'ok' ? sb : null),
        phasesCard(sa, sb?.state === 'ok' ? sb : null),
        requestsCard(requests.peek(routeId(A))),
      ];
    });
  }

  return {
    el,
    update(c) {
      ctx = c;
      if (c.A) {
        summaries.load(routeId(c.A), again);
        requests.load(routeId(c.A), again);
      }
      if (c.B) summaries.load(routeId(c.B), again);
      paint();
    },
  };
}
