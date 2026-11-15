// NeMo Gym run detail (addendum 9.15), from the `gym.smoke` events only: the per-task reward distribution of run A and (when chosen) run B overlaid, rewarded
// rollouts out of all rollouts as stacked bars per run, and the mean reward and pass@1 trends by model. With no run selected the newest run that has rewards is
// shown (the card says so). A run that recorded no rewards has none: its distribution is missing, never a column of zeros. Words: i18n keys `gymv.*`, `rv.*`, `viz.*`.
import { h } from '../dom.js';
import { t } from '../i18n.js';
import { fmtInt } from '../format.js';
import { fmtRowTime, fmtRunValue, fmtAxis } from '../runs.js';
import { rewardsOf, rewardStats, rewardBins, rolloutRows, modelTrends } from '../gym-model.js';
import { columnChart } from '../charts/bar-charts.js';
import { trendChart } from '../charts/trend-chart.js';
import { lineSwatch } from '../charts/series-chart.js';
import { stackCard } from '../charts/stack-card.js';
import { dataTable, chartEmpty } from '../charts/parts.js';
import { vizCard, chartPanel, note, repaintKeepFocus } from './kit.js';

const r3 = (v) => (v == null ? '–' : v.toFixed(3));
const newestWithRewards = (rows) => [...rows].reverse().find((r) => rewardsOf(r).length > 0) ?? null;

export function gymDetail(scope) {
  const el = h('div', { class: 'stack-v rv-detail' });
  let ctx = null,
    rs = null;

  // ---- reward distribution
  function distributionCard(A, B, defaulted) {
    const ra = A ? rewardsOf(A) : [],
      rb = B ? rewardsOf(B) : [];
    const bins = rewardBins(ra, B ? rb : null);
    const sa = rewardStats(ra),
      sb = rewardStats(rb);
    const series = [{ key: 'a', name: `A · ${A?.label ?? '–'}`, cls: 'bf1', values: bins.a }, ...(B ? [{ key: 'b', name: `B · ${B.label}`, cls: 'bf2 cc-b', values: bins.b }] : [])];
    const line = (tag, s, r) =>
      r
        ? h(
            'li',
            h('strong', tag),
            ' ',
            h('span', r.label),
            h('span', { class: 'muted' }, ` · ${s.n ? t('gymv.stats', { n: s.n, mean: r3(s.mean), full: s.full, zero: s.zero }) : t('gymv.no_rewards')}`),
          )
        : null;
    const missingNote = (tag, r, rewards) => (r && !rewards.length ? note(t('gymv.run_no_rewards', { run: tag }), 'warn') : null);
    return vizCard(
      { title: t('gymv.dist_title'), hint: t('gymv.dist_hint') },
      defaulted && A ? note(t('gymv.default_run', { run: A.label, time: fmtRowTime(A) }), 'info') : null,
      h('ul', { class: 'rv-facts' }, line('A', sa, A), line('B', sb, B)),
      missingNote('A', A, ra),
      missingNote('B', B, rb),
      chartPanel(rs, {
        build: (w) =>
          ra.length || rb.length
            ? columnChart({
                cats: bins.labels,
                series,
                width: w,
                height: w < 420 ? 200 : 220,
                ariaLabel: t('gymv.dist_alt', { a: sa.n, b: sb.n }),
                countText: (n) => t('gymv.tasks', { count: n }),
                yTitle: t('gymv.y_tasks'),
              })
            : null,
        empty: chartEmpty(t('gymv.dist_none')),
        legend: series.length
          ? h(
              'ul',
              { class: 'legend' },
              series.map((s) => h('li', h('i', { class: `sw ${s.cls}`, 'aria-hidden': 'true' }), h('span', s.name))),
            )
          : null,
      }),
      ra.length || rb.length ? dataTable(t('chart.data_table'), [t('gymv.col_bin'), 'A', ...(B ? ['B'] : [])], () => bins.labels.map((l, i) => [l, bins.a[i], ...(B ? [bins.b[i]] : [])])) : null,
    );
  }

  // ---- rewarded / rollouts per run
  function rolloutsCard(view) {
    const rows = rolloutRows(view);
    const parts = [
      { key: 'rewarded', name: t('gymv.rewarded'), cls: 'bf2' },
      { key: 'none', name: t('gymv.no_reward'), cls: 'bf3' },
    ];
    const card = stackCard(rs, { title: t('gymv.rollouts_title'), hint: t('gymv.rollouts_hint'), parts, valueFmt: (v) => fmtInt(v), emptyText: t('gymv.rollouts_none'), labelMax: 260 });
    card.update({
      rows: rows.map((r) => ({
        id: r.id,
        label: `${fmtRowTime(r)} ${r.label.replace(/, \d+ tasks?$/, '')}`.slice(0, 46),
        values: { rewarded: r.rewarded, none: r.none },
        totalText: r.rewarded == null ? t('gymv.of_rollouts_unknown', { rollouts: r.rollouts }) : t('gymv.of_rollouts', { rewarded: r.rewarded, rollouts: r.rollouts }),
      })),
    });
    return card.el;
  }

  // ---- trends by model
  function trendsCard(view) {
    const tr = modelTrends(view);
    const span = (view.at(-1)?.ts ?? 0) - (view[0]?.ts ?? 0);
    const dateOf = (i) => (span < 3 * 86400 ? fmtRowTime(view[i]) : fmtRowTime(view[i]).slice(0, 5));
    const labelOf = (id) => view.find((r) => r.id === id)?.label ?? id;
    const panel = (key, unit) => {
      const groups = tr[key];
      const pts = groups.reduce((a, g) => a + g.points.length, 0);
      const title = t(`rh.n.${key}`);
      return chartPanel(rs, {
        title,
        build: (w) =>
          trendChart({
            groups,
            n: view.length,
            width: w,
            height: w < 420 ? 190 : 210,
            yFmt: (v) => fmtAxis(v, unit),
            valueFmt: (v) => fmtRunValue(v, unit),
            dateOf,
            xTickCount: Math.max(3, Math.min(6, Math.floor(w / 120))),
            labelOf,
            a: ctx.A?.id,
            b: ctx.B?.id,
            onPick: (id, o) => ctx.pick(id, o.compare),
            ariaLabel: t('gymv.trend_alt', { metric: title, points: pts, runs: view.length }),
          }),
        empty: chartEmpty(t('gymv.trend_none', { metric: title })),
      });
    };
    const groups = tr.mean_reward;
    return vizCard(
      { title: t('gymv.trend_title'), hint: t('gymv.trend_hint'), wide: true },
      h('div', { class: 'rv-panels two' }, panel('mean_reward', 'score'), panel('pass_at_1', 'ratio')),
      groups.length
        ? h(
            'ul',
            { class: 'legend' },
            groups.map((g, gi) => h('li', lineSwatch(gi), h('span', `${g.key} (${g.count})`))),
          )
        : null,
      view.length
        ? dataTable(t('chart.data_table'), [t('rh.col_run'), t('rh.f.model'), t('rh.n.mean_reward'), t('rh.n.pass_at_1')], () =>
            view.map((r) => [r.label, r.fields?.model ?? '–', r3(r.fields?.mean_reward), fmtRunValue(r.fields?.pass_at_1, 'ratio')]),
          )
        : null,
    );
  }

  function paint() {
    rs?.dispose();
    rs = scope.child();
    repaintKeepFocus(el, () => {
      const { view, A, B } = ctx;
      const A2 = A ?? newestWithRewards(view);
      return [h('h2', { class: 'rv-h' }, t('rv.detail')), h('div', { class: 'grid two rv-grid' }, distributionCard(A2, B, !A), rolloutsCard(view)), trendsCard(view)];
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
