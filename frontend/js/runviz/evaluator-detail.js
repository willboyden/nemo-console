// NeMo Evaluator run detail (addendum 9.15), from the `evaluator.run` events only (no extra route): scores with 95% confidence-interval bars per benchmark for run A
// and (when chosen) run B, a model x benchmark matrix of the newest score per pair, cost against score (tokens or time on x) and failure-rate bars. Run A is the run
// selected in the history; with none selected the newest run that has benchmarks is shown and the card says so. A figure an older event lacks (an interval, a token
// count) is an en dash and no bar or point, never zero. Words: i18n keys `evv.*`, `rv.*`, `viz.*`.
import { h } from '../dom.js';
import { t } from '../i18n.js';
import { fmtInt, fmtCompact } from '../format.js';
import { fmtDur, fmtPercent } from '../viz/fmt.js';
import { fmtRowTime } from '../runs.js';
import { levelOf } from '../viz/heat.js';
import { ciRows, newestWithBenchmarks, latestMatrix, cellKey, costPoints, COST_AXES, failureRows } from '../evaluator-model.js';
import { hbarChart } from '../charts/bar-charts.js';
import { cellMatrix } from '../charts/cell-matrix.js';
import { pointsChart, markerKey } from '../charts/curve-chart.js';
import { heatLegend } from '../charts/heatmap.js';
import { dataTable, chartEmpty } from '../charts/parts.js';
import { vizCard, chartPanel, note, repaintKeepFocus, pickerBar } from './kit.js';

const score3 = (v) => (v == null ? '–' : v.toFixed(3));
const score2 = (v) => (v == null ? '–' : v.toFixed(2));

export function evaluatorDetail(scope, { hash }) {
  const el = h('div', { class: 'stack-v rv-detail' });
  let ctx = null,
    rs = null;
  let axis = COST_AXES.some((a) => a.id === hash.get('cx')) ? hash.get('cx') : 'tokens';

  // ---- scores with intervals
  function ciCard(A, B, defaulted) {
    const rows = ciRows(A, B);
    const unit = rows.every((r) => [r.a?.score, r.b?.score, r.a?.hi, r.b?.hi].filter((v) => v != null).every((v) => v >= 0 && v <= 1));
    const bar = (tag, key, cls, d, bench) => {
      if (!d) return { key, value: null, cls, text: t('evv.no_score', { run: tag, bench }) };
      const ci = d.lo != null ? t('evv.ci', { lo: score3(d.lo), hi: score3(d.hi) }) : t('evv.no_ci');
      return {
        key,
        value: d.score,
        lo: d.lo,
        hi: d.hi,
        cls,
        text: `${tag} · ${bench}: ${score3(d.score)} (${ci}${d.samples != null ? `, ${t('evv.samples', { n: fmtInt(d.samples) })}` : ''})`,
        valueText: score3(d.score),
      };
    };
    const barRows = rows.map((r) => ({ id: r.bench, label: r.bench, bars: [bar('A', 'a', 'bf1', r.a, r.bench), ...(B ? [bar('B', 'b', 'bf2 hb-b', r.b, r.bench)] : [])] }));
    const noCi = rows.filter((r) => (r.a && r.a.lo == null) || (r.b && r.b.lo == null)).length;
    const alt = t('evv.ci_alt', { count: rows.length });
    return vizCard(
      { title: t('evv.ci_title'), hint: t('evv.ci_hint'), wide: true },
      defaulted && A ? note(t('evv.default_run', { run: A.label, time: fmtRowTime(A) }), 'info') : null,
      !A && !B ? chartEmpty(t('evv.none')) : null,
      h(
        'ul',
        { class: 'legend' },
        A ? h('li', h('i', { class: 'sw bf1', 'aria-hidden': 'true' }), h('span', `A · ${A.label}`)) : null,
        B ? h('li', h('i', { class: 'sw bf2 hb-b-sw', 'aria-hidden': 'true' }), h('span', `B · ${B.label}`)) : null,
        h('li', h('i', { class: 'sw-whisker', 'aria-hidden': 'true' }), h('span', t('evv.whisker'))),
      ),
      chartPanel(rs, {
        build: (w) => (rows.length ? hbarChart({ rows: barRows, width: w, ariaLabel: alt, axisFmt: (v) => String(+v.toFixed(2)), domain: unit ? { min: 0, max: 1 } : null }) : null),
        empty: chartEmpty(t('evv.none')),
      }),
      noCi ? h('p', { class: 'muted small' }, t('evv.no_ci_note', { count: noCi })) : null,
      rows.length
        ? dataTable(t('chart.data_table'), [t('evv.col_bench'), 'A', 'A ' + t('evv.col_ci'), ...(B ? ['B', 'B ' + t('evv.col_ci')] : [])], () =>
            rows.map((r) => [
              r.bench,
              score3(r.a?.score),
              r.a?.lo != null ? `${score3(r.a.lo)}–${score3(r.a.hi)}` : '–',
              ...(B ? [score3(r.b?.score), r.b?.lo != null ? `${score3(r.b.lo)}–${score3(r.b.hi)}` : '–'] : []),
            ]),
          )
        : null,
    );
  }

  // ---- model x benchmark
  function matrixCard(m) {
    const models = m.models,
      benches = m.benches;
    const cell = (ri, ci) => {
      const c = m.cells.get(cellKey(models[ri], benches[ci]));
      if (!c) return null;
      const level = levelOf(c.score, m.domain);
      return { cls: `heatc fh${level}`, text: score2(c.score), level, label: t('evv.cell', { model: models[ri], bench: benches[ci], score: score3(c.score), when: fmtRowTime({ ts: c.ts }) }) };
    };
    const byRun = (ri, ci) => m.cells.get(cellKey(models[ri], benches[ci]))?.runId;
    const alt = t('evv.matrix_alt', { models: models.length, benches: benches.length });
    return vizCard(
      { title: t('evv.matrix_title'), hint: t('evv.matrix_hint') },
      chartPanel(rs, {
        build: (w) =>
          models.length
            ? cellMatrix({
                cols: benches.map((b) => ({ id: b, label: b, title: b })),
                rows: models.map((x) => ({ id: x, label: x })),
                cell,
                width: w,
                ariaLabel: alt,
                noDataLabel: t('evv.no_result'),
                onCell: (ri, ci) => byRun(ri, ci) && ctx.pick(byRun(ri, ci), false),
              })
            : null,
        empty: chartEmpty(t('evv.matrix_none')),
        legend: models.length ? heatLegend({ domain: { ...m.domain, curve: 'linear' }, fmt: score2 }) : null,
      }),
      models.length ? dataTable(t('chart.data_table'), [t('evv.col_model'), ...benches], () => models.map((x) => [x, ...benches.map((b) => score3(m.cells.get(cellKey(x, b))?.score ?? null))])) : null,
    );
  }

  // ---- cost vs score
  function costCard(rows) {
    const { points, groups, skipped } = costPoints(rows, axis);
    const byId = new Map(rows.map((r) => [r.id, r]));
    const tokens = axis === 'tokens';
    const xFmt = tokens ? (v) => fmtCompact(v) : (v) => fmtDur(v * 1000);
    const pts = points.map((p) => ({ ...p, text: t('evv.cost_tip', { run: byId.get(p.id)?.label ?? p.id, bench: p.bench, x: tokens ? fmtInt(p.x) : fmtDur(p.x * 1000), score: score3(p.y) }) }));
    const unit = points.every((p) => p.y >= 0 && p.y <= 1);
    const xs = points.map((p) => p.x);
    const picker = pickerBar({
      label: t('evv.cost_axis'),
      values: COST_AXES.map((a) => a.id),
      current: axis,
      labelOf: (v) => t(`evv.axis.${v}`),
      id: 'cx',
      onPick: (v) => {
        axis = v;
        hash.set({ cx: v === 'tokens' ? null : v });
        paint();
      },
    });
    return vizCard(
      { title: t('evv.cost_title'), hint: t('evv.cost_hint') },
      h('div', { class: 'row gap wrap rv-controls' }, picker),
      chartPanel(rs, {
        build: (w) =>
          pointsChart({
            points: pts,
            width: w,
            height: w < 420 ? 240 : 280,
            logX: xs.length > 1 && Math.max(...xs) / Math.min(...xs) > 10,
            xFmt,
            yFmt: (v) => String(+v.toFixed(2)),
            xTitle: t(`evv.axis_title.${axis}`),
            yTitle: t('evv.score'),
            yDomain: unit ? { min: 0, max: 1 } : null,
            a: ctx.A?.id,
            b: ctx.B?.id,
            onPick: (id, o) => ctx.pick(id, o.compare),
            ariaLabel: t('evv.cost_alt', { count: points.length }),
          }).svg,
        empty: chartEmpty(t('evv.cost_none')),
      }),
      points.length
        ? h(
            'ul',
            { class: 'legend' },
            groups.map((g) => h('li', markerKey(g.g), h('span', `${g.bench} (${g.count})`))),
          )
        : null,
      skipped ? h('p', { class: 'muted small' }, t('evv.cost_skipped', { count: skipped })) : null,
      points.length
        ? dataTable(t('chart.data_table'), [t('rh.col_run'), t('evv.col_bench'), t(`evv.axis.${axis}`), t('evv.score')], () =>
            points.map((p) => [byId.get(p.id)?.label ?? p.id, p.bench, tokens ? fmtInt(p.x) : fmtDur(p.x * 1000), score3(p.y)]),
          )
        : null,
    );
  }

  // ---- failure rates
  function failureCard(m) {
    const f = failureRows(m);
    const rows = f.rows.map((r) => ({
      id: r.runId,
      label: `${r.model} · ${r.bench}`,
      bars: [{ key: 'f', value: r.rate, cls: 'hb-bad', text: t('evv.fail_tip', { model: r.model, bench: r.bench, pct: fmtPercent(r.rate, 1) }), valueText: fmtPercent(r.rate, 1) }],
    }));
    return vizCard(
      { title: t('evv.fail_title'), hint: t('evv.fail_hint') },
      chartPanel(rs, {
        build: (w) =>
          rows.length
            ? hbarChart({ rows, width: w, ariaLabel: t('evv.fail_alt', { count: rows.length }), axisFmt: (v) => fmtPercent(v, 0), labelW: w < 520 ? 150 : 270, onPick: (id) => ctx.pick(id, false) })
            : null,
        empty: chartEmpty(t('evv.fail_none')),
      }),
      f.missing ? h('p', { class: 'muted small' }, t('evv.fail_missing', { count: f.missing })) : null,
      f.total > rows.length ? h('p', { class: 'muted small' }, t('evv.fail_more', { shown: rows.length, total: f.total })) : null,
      rows.length ? dataTable(t('chart.data_table'), [t('evv.col_model'), t('evv.col_bench'), t('evv.col_fail')], () => f.rows.map((r) => [r.model, r.bench, fmtPercent(r.rate, 1)])) : null,
    );
  }

  function paint() {
    rs?.dispose();
    rs = scope.child();
    repaintKeepFocus(el, () => {
      const { view, A, B } = ctx;
      const A2 = A ?? newestWithBenchmarks(view);
      const m = latestMatrix(view);
      return [h('h2', { class: 'rv-h' }, t('rv.detail')), ciCard(A2, B, !A), h('div', { class: 'grid two rv-grid' }, matrixCard(m), costCard(view)), failureCard(m)];
    });
  }
  return {
    el,
    update(c) {
      ctx = c;
      paint();
    },
  };
}
