// Data Designer, Runs tab (addendum 9.15): the column profile of the selected run, from `GET /systems/datadesigner/runs/{id}/profile` (per-column aggregates of
// records.jsonl), and a timeline of the runs (duration by script, verdict colours, records requested vs written).
//
// ROLE: the records are model-written content, so the most frequent VALUES of a column are for an admin only. The route withholds them from a viewer (`top` null,
// `values_hidden: true`); this page shows the aggregates to everyone and the top values only when isAdmin() and the answer says they are not hidden. A viewer is
// told why they are missing, and nothing asks for them. Column names come from the operator's script and are shown to both. Every value is text (textContent).
// A figure the answer lacks is an en dash, never 0. Words: i18n keys `ddv.*`, `rv.*`, `viz.*`.
import { h } from '../dom.js';
import { t } from '../i18n.js';
import { isAdmin } from '../app-context.js';
import { fmtInt } from '../format.js';
import { fmtDur, fmtPercent } from '../viz/fmt.js';
import { fmtRowTime, VERDICT_GLYPH } from '../runs.js';
import { shapeProfile, typeMixRows, topColumns, topCoverage, TYPE_KEYS, ddTimelineRows } from '../dd-profile-model.js';
import { hbarChart } from '../charts/bar-charts.js';
import { stackCard } from '../charts/stack-card.js';
import { rangeBar } from '../charts/range-bar.js';
import { dataTable, chartEmpty, labelledBar } from '../charts/parts.js';
import { select } from '../components/ui.js';
import { vizCard, chartPanel, note, loadingBlock, failBlock, enc } from './kit.js';

const SYS = 'datadesigner';
const num = (v) => (v == null ? '–' : fmtInt(v));
const fmtRange = (r) => (r ? `${num(r.min)} · ${num(r.p50)} · ${num(r.p95)} · ${num(r.max)}` : '–');
/** A range bar with its four figures as text beside it; no range at all is one dash. */
const rangeCell = (r, what) => (r ? h('div', { class: 'ddv-range' }, rangeBar(r, rangeLabel(r, what)), h('span', { class: 'muted small num' }, fmtRange(r))) : h('span', { class: 'muted' }, '–'));
const rangeLabel = (r, what) => (r ? t('ddv.range_label', { what, min: num(r.min), p50: num(r.p50), p95: num(r.p95), max: num(r.max) }) : '');

/**
 * The column profile of one run. `scope` should be a child scope the caller disposes when the run changes. Returns the element (a <div> of cards).
 */
export function profileCard(scope, runId) {
  const host = h('div', { class: 'stack-v ddv-profile' }, vizCard({ title: t('ddv.profile_title'), wide: true }, loadingBlock()));
  const admin = isAdmin();
  async function load() {
    host.replaceChildren(vizCard({ title: t('ddv.profile_title'), wide: true }, loadingBlock()));
    try {
      const answer = await scope.api.get(`/systems/${SYS}/runs/${enc(runId)}/profile`);
      if (!scope.alive) return;
      host.replaceChildren(...build(scope, shapeProfile(answer, { admin })));
    } catch (e) {
      if (scope.alive) host.replaceChildren(vizCard({ title: t('ddv.profile_title'), wide: true }, failBlock(e, load)));
    }
  }
  load();
  return host;
}

function build(scope, p) {
  const title = t('ddv.profile_title');
  if (!p.columns.length) return [vizCard({ title, hint: t('ddv.profile_hint'), wide: true }, note(p.rows === 0 ? t('ddv.no_rows') : t('ddv.no_columns'), 'info'))];
  const out = [];
  // ---- type mix
  const parts = TYPE_KEYS.map((k, i) => ({ key: k, name: t(`ddv.type.${k}`), cls: ['bf1', 'bf2', 'bf3', 'bf5', 'bf4', 'bf6'][i] }));
  const mix = stackCard(scope, { title: t('ddv.types_title'), hint: t('ddv.types_hint'), parts, valueFmt: (v) => fmtInt(v), emptyText: t('ddv.types_none'), labelMax: 150, wide: true });
  mix.update({ rows: typeMixRows(p) });
  // ---- the table of columns
  const head = [t('ddv.col_name'), t('ddv.col_null'), t('ddv.col_distinct'), t('ddv.col_length'), t('ddv.col_numeric')];
  const rowsEls = p.columns.map((c) =>
    h(
      'tr',
      h('th', { scope: 'row', class: 'ddv-name' }, h('code', c.name), c.dominant ? h('span', { class: 'muted small' }, ` ${t(`ddv.type.${c.dominant}`)}`) : null),
      h(
        'td',
        { class: 'ddv-null' },
        h(
          'div',
          { class: 'sy-cell' },
          labelledBar(c.nullRate, 1, t('ddv.null_label', { name: c.name, rate: fmtPercent(c.nullRate, 1) }), 'bf-bad'),
          h('span', { class: 'num' }, fmtPercent(c.nullRate, 1)),
        ),
      ),
      h('td', { class: 'num al-right' }, c.distinct == null ? '–' : `${c.distinctCapped ? '≥ ' : ''}${fmtInt(c.distinct)}`),
      h('td', rangeCell(c.length, `${c.name} ${t('ddv.col_length')}`)),
      h('td', rangeCell(c.numeric, `${c.name} ${t('ddv.col_numeric')}`)),
    ),
  );
  const table = vizCard(
    { title: t('ddv.columns_title'), hint: t('ddv.columns_hint'), wide: true },
    h('p', { class: 'muted small' }, t('ddv.facts', { rows: p.rows == null ? '–' : fmtInt(p.rows), columns: p.columns.length })),
    p.truncated ? note(t('ddv.truncated'), 'info') : null,
    h(
      'div',
      { class: 'tablewrap tall' },
      h(
        'table',
        { class: 'compact ddv-table', 'aria-label': t('ddv.columns_title') },
        h(
          'thead',
          h(
            'tr',
            head.map((c, i) => h('th', { scope: 'col', class: i === 2 ? 'al-right' : '' }, c)),
          ),
        ),
        h('tbody', rowsEls),
      ),
    ),
    h('p', { class: 'muted small' }, t('ddv.range_key')),
  );
  out.push(mix.el, table);
  // ---- top values: an admin only
  out.push(p.valuesHidden || !p.topShown ? hiddenCard(p) : topValuesCard(scope, p));
  return out;
}

/** Why the top values are not there: a viewer is told they are admin-only; an admin whose answer has none is told no column was small enough. */
function hiddenCard(p) {
  return vizCard({ title: t('ddv.top_title'), wide: true }, note(p.valuesHidden ? t('ddv.top_hidden') : t('ddv.top_none'), 'info'));
}

function topValuesCard(scope, p) {
  const cols = topColumns(p);
  let current = cols[0].name;
  const chartHost = h('div');
  const sel = select(
    cols.map((c) => ({ value: c.name, label: c.name })),
    current,
    (v) => {
      current = v;
      draw();
    },
    { id: 'ddv-top-col', 'aria-label': t('ddv.top_pick') },
  );
  function draw() {
    const c = cols.find((x) => x.name === current) ?? cols[0];
    const total = c.nonNull ?? c.top.reduce((a, x) => a + x.count, 0);
    const rows = c.top.map((x, i) => ({
      id: `${i}`,
      label: x.value,
      bars: [
        {
          key: 'n',
          value: x.count,
          cls: 'bf1',
          text: t('ddv.top_tip', { value: x.value, count: fmtInt(x.count), pct: total ? fmtPercent(x.count / total, 1) : '–' }),
          valueText: `${fmtInt(x.count)}${total ? ` · ${fmtPercent(x.count / total, 0)}` : ''}`,
        },
      ],
    }));
    const cov = topCoverage(c);
    chartHost.replaceChildren(
      chartPanel(scope, {
        build: (w) => hbarChart({ rows, width: w, ariaLabel: t('ddv.top_alt', { column: c.name, count: rows.length }), axisFmt: (v) => fmtInt(v), tailW: 92 }),
        empty: chartEmpty(t('ddv.top_none')),
        sub: cov != null ? h('p', { class: 'muted small' }, t('ddv.top_cover', { pct: fmtPercent(cov, 0) })) : null,
      }),
      dataTable(t('chart.data_table'), [t('ddv.value'), t('ddv.count')], () => c.top.map((x) => [x.value, fmtInt(x.count)])),
    );
  }
  draw();
  return vizCard(
    { title: t('ddv.top_title'), hint: t('ddv.top_hint'), wide: true },
    h('div', { class: 'field filter ddv-pick' }, h('label', { for: 'ddv-top-col' }, t('ddv.top_pick')), sel),
    chartHost,
  );
}

// ------------------------------------------------------------------------------------------------ the run timeline

/**
 * @param runRows  the `dd.run` rows (runs.shapeRuns, oldest first);  @param selected the selected run id;  @param onPick(id)
 * Returns a card (a new one each call: the Runs tab redraws it with its list).
 */
export function ddTimelineCard(scope, runRows, { selected = null, onPick } = {}) {
  const rows = ddTimelineRows(runRows, Date.now());
  const dur = rows.map((r) => ({
    id: r.id,
    label: `${fmtRowTime(r)} ${r.script ?? r.id}`,
    bars: [
      {
        key: 'd',
        value: r.durationMs,
        cls: `bf1 hb-v-${r.vclass}${r.running ? ' hb-running' : ''}`,
        text: t('ddv.run_tip', { run: r.label, verdict: r.verdict, duration: r.durationMs == null ? '–' : fmtDur(r.durationMs) }),
        valueText: `${VERDICT_GLYPH[r.vclass]} ${r.durationMs == null ? '–' : fmtDur(r.durationMs)}`,
      },
    ],
  }));
  const withRecords = rows.filter((r) => r.requested != null || r.written != null);
  const rec = withRecords.map((r) => ({
    id: r.id,
    label: `${fmtRowTime(r)} ${r.script ?? r.id}`,
    bars: [
      { key: 'req', value: r.requested, cls: 'hb-muted', text: t('ddv.req_tip', { n: r.requested == null ? '–' : fmtInt(r.requested) }), valueText: r.requested == null ? '–' : fmtInt(r.requested) },
      {
        key: 'out',
        value: r.written,
        cls: `bf2 hb-v-${r.vclass}`,
        text: t('ddv.out_tip', { n: r.written == null ? '–' : fmtInt(r.written) }),
        valueText: r.written == null ? '–' : `${fmtInt(r.written)}${r.requested ? ` · ${fmtPercent(r.written / r.requested, 0)}` : ''}`,
      },
    ],
  }));
  const seen = new Map();
  for (const r of rows) if (!seen.has(r.vclass)) seen.set(r.vclass, r.verdict);
  const sel = { a: selected };
  return vizCard(
    { title: t('ddv.timeline_title'), hint: t('ddv.timeline_hint'), wide: true, cls: 'ddv-tl' },
    h(
      'ul',
      { class: 'legend' },
      [...seen.entries()].map(([vc, v]) => h('li', h('i', { class: `sw bf1 sw-v hb-v-${vc}`, 'aria-hidden': 'true' }), h('span', `${VERDICT_GLYPH[vc]} ${v}`))),
    ),
    chartPanel(scope, {
      title: t('ddv.duration_title'),
      build: (w) =>
        dur.length
          ? hbarChart({
              rows: dur,
              width: w,
              ariaLabel: t('ddv.timeline_alt', { count: dur.length }),
              axisFmt: (v) => (v === 0 ? '0' : fmtDur(v)),
              labelW: w < 520 ? 130 : 230,
              tailW: 92,
              onPick,
              selected: sel,
            })
          : null,
      empty: chartEmpty(t('ddv.timeline_none')),
    }),
    withRecords.length
      ? chartPanel(scope, {
          title: t('ddv.records_title'),
          build: (w) =>
            hbarChart({
              rows: rec,
              width: w,
              ariaLabel: t('ddv.records_alt', { count: rec.length }),
              axisFmt: (v) => fmtInt(v),
              labelW: w < 520 ? 130 : 230,
              tailW: w < 520 ? 100 : 124,
              onPick,
              selected: sel,
            }),
          empty: chartEmpty(t('ddv.records_none')),
          legend: h(
            'ul',
            { class: 'legend' },
            h('li', h('i', { class: 'sw hb-muted-sw', 'aria-hidden': 'true' }), h('span', t('ddv.requested'))),
            h('li', h('i', { class: 'sw bf2', 'aria-hidden': 'true' }), h('span', t('ddv.written'))),
          ),
        })
      : null,
    rows.length
      ? dataTable(t('chart.data_table'), [t('rh.col_run'), t('rh.f.mode'), t('rh.fact_verdict'), t('ddv.duration_title'), t('ddv.requested'), t('ddv.written')], () =>
          rows.map((r) => [r.label, r.mode ?? '–', r.verdict, r.durationMs == null ? '–' : fmtDur(r.durationMs), num(r.requested), num(r.written)]),
        )
      : null,
  );
}
