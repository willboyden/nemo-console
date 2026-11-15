// Model Optimizer detail (addendum 9.15), from the `modelopt.job` events of the runs shown: how long a running job has run against what a finished job of the same method
// takes (a stuck job stands out), a timeline of duration bars (colour = method, the outline, glyph and word = verdict: nothing is colour-only), and base against output size per
// job with the ratio. A job with no size or duration has no bar (an en dash), never a zero-length one. Words: i18n keys `mov.*`, `rv.*`, `viz.*`.
import { h } from '../dom.js';
import { t } from '../i18n.js';
import { fmtBytes } from '../format.js';
import { fmtDur } from '../viz/fmt.js';
import { fmtRowTime, VERDICT_GLYPH } from '../runs.js';
import { jobsOf, methodsOf, timelineRows, sizeRows, runningJobs, ratioParts } from '../modelopt-model.js';
import { hbarChart } from '../charts/bar-charts.js';
import { dataTable, chartEmpty } from '../charts/parts.js';
import { badge } from '../components/ui.js';
import { vizCard, chartPanel, repaintKeepFocus } from './kit.js';

const pct = (v) => (v == null ? '–' : `${v.toFixed(v < 10 ? 1 : 0)}%`);

export function modeloptDetail(scope) {
  const el = h('div', { class: 'stack-v rv-detail' });
  let ctx = null,
    rs = null;
  const labelW = (w) => (w < 520 ? 128 : w < 760 ? 190 : 250);

  // ---- running jobs
  function runningCard(jobs) {
    const run = runningJobs(jobs);
    const rows = run.map((r) => ({
      id: r.id,
      label: r.label,
      bars: [
        { key: 'age', value: r.ageMs, cls: `${r.stuck ? 'hb-warn' : 'hb-info'} hb-running`, text: t('mov.age_tip', { job: r.label, age: fmtDur(r.ageMs) }), valueText: fmtDur(r.ageMs) },
        ...(r.typicalMs != null
          ? [{ key: 'typical', value: r.typicalMs, cls: 'hb-muted', text: t('mov.typical_tip', { typical: fmtDur(r.typicalMs) }), valueText: t('mov.typical_short', { typical: fmtDur(r.typicalMs) }) }]
          : []),
      ],
    }));
    return vizCard(
      { title: t('mov.running_title'), hint: t('mov.running_hint') },
      run.length
        ? [
            h(
              'ul',
              { class: 'rv-facts' },
              run.map((r) =>
                h(
                  'li',
                  h('code', r.label),
                  ' ',
                  r.stuck ? badge(`! ${t('mov.stuck')}`, 'warn') : badge(`◌ ${t('mov.running')}`, 'info'),
                  h(
                    'span',
                    { class: 'muted' },
                    ` · ${t('mov.age_line', { age: fmtDur(r.ageMs) })}${r.typicalMs != null ? ` · ${t('mov.typical_line', { typical: fmtDur(r.typicalMs) })}` : ` · ${t('mov.no_typical')}`}`,
                  ),
                ),
              ),
            ),
            chartPanel(rs, {
              build: (w) =>
                hbarChart({
                  rows,
                  width: w,
                  ariaLabel: t('mov.running_alt', { count: run.length }),
                  axisFmt: (v) => (v === 0 ? '0' : fmtDur(v)),
                  labelW: labelW(w),
                  tailW: 92,
                  onPick: (id) => ctx.pick(id, false),
                }),
              empty: chartEmpty(t('mov.running_none')),
            }),
          ]
        : h('p', { class: 'note ok', role: 'status' }, t('mov.none_running')),
    );
  }

  // ---- timeline
  function timelineCard(jobs) {
    const rows = timelineRows(jobs);
    const methods = methodsOf(jobs);
    const cls = (method) => `bf${(Math.max(0, methods.indexOf(method)) % 6) + 1}`;
    const seen = new Map();
    for (const r of rows) if (!seen.has(r.vclass)) seen.set(r.vclass, r);
    const bars = rows.map((r) => ({
      id: r.id,
      label: `${fmtRowTime(r)} ${jobs.find((j) => j.id === r.id)?.recipe ?? r.id}`,
      bars: [
        {
          key: 'd',
          value: r.value,
          cls: `${r.method ? cls(r.method) : 'hb-muted'} hb-v-${r.vclass}${r.running ? ' hb-running' : ''}`,
          text: t('mov.job_tip', { job: r.label, verdict: r.verdict, method: r.method ?? '–', duration: r.value == null ? '–' : fmtDur(r.value) }),
          valueText: `${VERDICT_GLYPH[r.vclass]} ${r.value == null ? '–' : fmtDur(r.value)}`,
        },
      ],
    }));
    return vizCard(
      { title: t('mov.timeline_title'), hint: t('mov.timeline_hint'), wide: true },
      h(
        'ul',
        { class: 'legend' },
        methods.map((m, i) => h('li', h('i', { class: `sw bf${(i % 6) + 1}`, 'aria-hidden': 'true' }), h('span', m))),
        [...seen.values()].map((r) => h('li', h('i', { class: `sw sw-v hb-v-${r.vclass}`, 'aria-hidden': 'true' }), h('span', `${VERDICT_GLYPH[r.vclass]} ${r.verdict}`))),
      ),
      chartPanel(rs, {
        build: (w) =>
          rows.length
            ? hbarChart({
                rows: bars,
                width: w,
                ariaLabel: t('mov.timeline_alt', { count: rows.length }),
                axisFmt: (v) => (v === 0 ? '0' : fmtDur(v)),
                labelW: labelW(w),
                tailW: 92,
                onPick: (id) => ctx.pick(id, false),
                selected: { a: ctx.A?.id, b: ctx.B?.id },
                missingText: '–',
              })
            : null,
        empty: chartEmpty(t('mov.timeline_none')),
      }),
      rows.length
        ? dataTable(t('chart.data_table'), [t('rh.col_run'), t('rh.f.method'), t('rh.fact_verdict'), t('mov.col_duration')], () =>
            rows.map((r) => [r.label, r.method ?? '–', r.verdict, r.value == null ? '–' : fmtDur(r.value)]),
          )
        : null,
    );
  }

  // ---- size
  function sizeCard(jobs) {
    const { rows, missing } = sizeRows(jobs);
    const bars = rows.map((r) => {
      const p = ratioParts(r.ratio);
      return {
        id: r.id,
        label: `${jobs.find((j) => j.id === r.id)?.recipe ?? r.label}`,
        bars: [
          { key: 'base', value: r.base, cls: 'hb-muted', text: t('mov.base_tip', { size: fmtBytes(r.base, 2) }), valueText: fmtBytes(r.base, 1) },
          {
            key: 'out',
            value: r.output,
            cls: 'bf2',
            text: t('mov.out_tip', { size: fmtBytes(r.output, 2), pct: pct(p.pct), saved: pct(p.saved) }),
            valueText: `${fmtBytes(r.output, 1)} · ${pct(p.pct)}`,
          },
        ],
      };
    });
    return vizCard(
      { title: t('mov.size_title'), hint: t('mov.size_hint'), wide: true },
      h(
        'ul',
        { class: 'legend' },
        h('li', h('i', { class: 'sw hb-muted-sw', 'aria-hidden': 'true' }), h('span', t('mov.base'))),
        h('li', h('i', { class: 'sw bf2', 'aria-hidden': 'true' }), h('span', t('mov.output'))),
      ),
      chartPanel(rs, {
        build: (w) =>
          bars.length
            ? hbarChart({
                rows: bars,
                width: w,
                ariaLabel: t('mov.size_alt', { count: bars.length }),
                axisFmt: (v) => fmtBytes(v, 0),
                labelW: labelW(w),
                tailW: w < 520 ? 112 : 132,
                onPick: (id) => ctx.pick(id, false),
              })
            : null,
        empty: chartEmpty(t('mov.size_none')),
      }),
      missing > 0 ? h('p', { class: 'muted small' }, t('mov.size_missing', { count: missing })) : null,
      rows.length
        ? dataTable(t('chart.data_table'), [t('rh.col_run'), t('mov.base'), t('mov.output'), t('mov.col_ratio'), t('mov.col_saved')], () =>
            rows.map((r) => [r.label, fmtBytes(r.base, 2), fmtBytes(r.output, 2), pct(ratioParts(r.ratio).pct), pct(ratioParts(r.ratio).saved)]),
          )
        : null,
    );
  }

  function paint() {
    rs?.dispose();
    rs = scope.child();
    repaintKeepFocus(el, () => {
      const jobs = jobsOf(ctx.view, Date.now());
      return [h('h2', { class: 'rv-h' }, t('rv.detail')), runningCard(jobs), timelineCard(jobs), sizeCard(jobs)];
    });
  }
  // a running job's age moves: redraw each minute
  scope.every(60000, () => scope.alive && ctx && paint());
  return {
    el,
    update(c) {
      ctx = c;
      paint();
    },
  };
}
