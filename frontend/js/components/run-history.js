// Run history (addendum 9.13): what a run-based page's recent runs add up to, built ONLY from
// `GET /systems/{id}/events?limit=500` (each event's `data`), so it needs no route of its own.
//   * a verdict timeline: one mark per run (colour + glyph + word), hover = the run's summary, click = select it
//   * a trend of one chosen number across the runs, one line per group (model, preset ... as the descriptor lists them)
//   * an A/B compare: two runs side by side, the difference, and a better / worse mark where the number has a direction
//   * filters from the values seen; the whole selection (run, vs, metric, group, filters) lives in the URL hash
// A descriptor (js/run-descriptors.js) says which fields and numbers a system has. Every value from an event is shown as text
// (textContent through h()); a field the event lacks is an en dash, never 0.
import { h, append } from '../dom.js';
import { t } from '../i18n.js';
import { describeError } from '../errors.js';
import { badge, btn, emptyBox, select, skeleton } from './ui.js';
import { createHashState } from '../charts/hash-state.js';
import { mountFit } from '../charts/fit.js';
import { trendChart } from '../charts/trend-chart.js';
import { lineSwatch } from '../charts/series-chart.js';
import { chartEmpty, dataTable } from '../charts/parts.js';
import {
  shapeRuns,
  resolveNumbers,
  enumValues,
  applyFilters,
  trendGroups,
  compareRuns,
  resolveRunState,
  fmtRunValue,
  fmtDelta,
  fmtAxis,
  fmtRowTime,
  getField,
  valueOf,
  VERDICT_GLYPH,
  FILTER_PREFIX,
} from '../runs.js';

const MARK_KIND = { better: 'ok', worse: 'bad', same: 'muted' };
const MARK_GLYPH = { better: '✓', worse: '✕', same: '=' };
const present = (v) => v != null && v !== '';
const pctText = (p) => (p == null ? '–' : `${p > 0 ? '+' : p < 0 ? '−' : ''}${Math.abs(p * 100).toFixed(1)}%`);
const verdictChip = (r) => h('span', { class: `rh-verdict ${r.vclass}` }, h('span', { 'aria-hidden': 'true' }, VERDICT_GLYPH[r.vclass]), ' ', r.verdict);

/**
 * @param scope a panel scope;  @param ctx the tab context (ctx.initial: a deep link's URL is meant);  @param desc a run descriptor
 */
export function runHistoryPanel(scope, ctx, desc) {
  const hash = createHashState({ path: desc.system, tab: 'history', initial: ctx?.initial !== false });
  const host = h('div', { class: 'stack-v rh', 'aria-busy': 'true' }, skeleton(3));
  let events = null,
    lastRaw = '',
    rs = null;
  const labelOf = (def, suffix) => (suffix == null ? t(def.label) : t(def.label, { name: suffix }));

  async function load() {
    try {
      const evs = await scope.api.events(desc.system, 500);
      if (!scope.alive) return;
      const raw = JSON.stringify(evs);
      if (raw === lastRaw) return;
      lastRaw = raw;
      events = evs;
      render();
    } catch (e) {
      if (!scope.alive) return;
      // Keep what is on screen on a failed refresh; a first load that fails says so and offers a retry (not an error state:
      // a system that is off has no events to read).
      if (events == null) {
        host.replaceChildren(emptyBox(t('rh.load_failed'), describeError(e), btn(t('common.retry'), { icon: 'restart', onClick: load })));
        host.removeAttribute('aria-busy');
      }
    }
  }

  function render() {
    const focused = document.activeElement?.dataset?.rhFocus;
    rs?.dispose();
    rs = scope.child();
    const rows = shapeRuns(events, desc);
    host.removeAttribute('aria-busy');
    if (!rows.length) {
      host.replaceChildren(emptyBox(t('rh.empty'), t('rh.empty_hint')));
      return;
    }
    const numbers = resolveNumbers(rows, desc, labelOf);
    const st = resolveRunState(hash.all(), { rows, numbers, desc });
    const view = applyFilters(rows, st.filters);
    const byId = new Map(rows.map((r) => [r.id, r]));
    const A = st.run ? byId.get(st.run) : null,
      B = st.vs ? byId.get(st.vs) : null;
    const patch = (p, opts) => {
      hash.set(p, opts);
      render();
    };
    // Click selects a run (A, the baseline; clicking it again clears it); Shift+click picks the run to compare it with (B).
    const pickA = (id) => patch({ run: id === st.run ? null : id, vs: id === st.vs ? null : st.vs });
    const pickB = (id) => (!A ? pickA(id) : id === st.run ? null : patch({ vs: id === st.vs ? null : id }));
    const pick = (id, compare) => (compare ? pickB(id) : pickA(id));

    host.replaceChildren(filtersBar(rows, st, patch, view), timelineCard(view, st, A, B, pick), trendCard(view, st, numbers, patch, pick, A, B), compareCard(rows, A, B, numbers, patch));
    if (focused) host.querySelector(`[data-rh-focus="${CSS.escape(focused)}"]`)?.focus();
  }

  // ---- filters (enum dropdowns from the values seen)
  function filtersBar(rows, st, patch, view) {
    const seen = enumValues(rows, desc.filters || []);
    const fields = (desc.filters || []).filter((f) => seen[f].length > 1 || st.filters[f]);
    const any = Object.keys(st.filters).length > 0;
    const els = fields.map((f) => {
      const sel = select(
        [{ value: '', label: t('rh.all') }, ...seen[f].map((x) => ({ value: x.value, label: `${x.value} (${x.count})` }))],
        st.filters[f] ?? '',
        (v) => patch({ [`${FILTER_PREFIX}${f}`]: v || null }),
        { id: `rh-f-${f}`, dataset: { rhFocus: `f-${f}` } },
      );
      return h('div', { class: 'field filter' }, h('label', { for: sel.id }, t(`rh.f.${f}`)), sel);
    });
    return h(
      'div',
      { class: 'rt-filters rh-filters' },
      els,
      h('span', { class: 'muted small rh-count', role: 'status' }, t('rh.count', { shown: view.length, total: rows.length })),
      any
        ? btn(t('rh.reset'), {
            size: 'sm',
            icon: 'close',
            onClick: () => {
              hash.clear(FILTER_PREFIX);
              render();
            },
          })
        : null,
    );
  }

  // ---- verdict timeline
  function timelineCard(view, st, A, B, pick) {
    const seen = new Map();
    for (const r of view) {
      const e = seen.get(r.verdict);
      if (e) e.n++;
      else seen.set(r.verdict, { n: 1, r });
    }
    const marks = view.map((r) => {
      const isA = r.id === st.run,
        isB = r.id === st.vs;
      const tip = [`${r.label} · ${fmtRowTime(r)}`, r.summary].filter(Boolean).join('\n');
      const b = h(
        'button',
        {
          type: 'button',
          class: `rh-mark ${r.vclass}${isA ? ' is-a' : ''}${isB ? ' is-b' : ''}`,
          title: tip,
          'aria-label': t('rh.mark', { label: r.label, verdict: r.verdict, time: fmtRowTime(r) }),
          'aria-pressed': String(isA || isB),
          dataset: { rhFocus: `m-${r.id}` },
        },
        h('span', { 'aria-hidden': 'true' }, VERDICT_GLYPH[r.vclass]),
        isA || isB ? h('span', { class: 'rh-tag', 'aria-hidden': 'true' }, isA ? 'A' : 'B') : null,
      );
      b.addEventListener('click', (e) => pick(r.id, e.shiftKey));
      return b;
    });
    const legend = h(
      'ul',
      { class: 'legend rh-legend' },
      [...seen.entries()].map(([v, x]) => h('li', h('span', { class: `rh-mark mini ${x.r.vclass}`, 'aria-hidden': 'true' }, VERDICT_GLYPH[x.r.vclass]), h('span', `${v} (${x.n})`))),
    );
    const first = view[0],
      last = view.at(-1);
    return h(
      'section',
      { class: 'card rh-card', 'aria-label': t('rh.timeline') },
      h('header', { class: 'card-head' }, h('h3', t('rh.timeline')), h('span', { class: 'muted small' }, t('rh.timeline_hint'))),
      h(
        'div',
        { class: 'card-body' },
        view.length ? h('div', { class: 'rh-timeline', role: 'group', 'aria-label': t('rh.timeline') }, marks) : chartEmpty(t('rh.no_match')),
        view.length ? h('div', { class: 'row gap wrap between muted small' }, legend, h('span', t('rh.span', { from: fmtRowTime(first), to: fmtRowTime(last) }))) : null,
        A ? h('p', { class: 'rh-summary' }, h('strong', 'A'), ' ', h('code', A.id), ' · ', verdictChip(A), A.summary ? h('span', { class: 'muted' }, ` — ${A.summary}`) : null) : null,
        B ? h('p', { class: 'rh-summary' }, h('strong', 'B'), ' ', h('code', B.id), ' · ', verdictChip(B), B.summary ? h('span', { class: 'muted' }, ` — ${B.summary}`) : null) : null,
      ),
    );
  }

  // ---- trend of one number
  function trendCard(view, st, numbers, patch, pick, A, B) {
    const head = h('header', { class: 'card-head' }, h('h3', t('rh.trend')));
    const body = h('div', { class: 'card-body' });
    const out = h('section', { class: 'card rh-card', 'aria-label': t('rh.trend') }, head, body);
    const def = numbers.find((n) => n.field === st.metric);
    if (!def) {
      body.append(chartEmpty(t('rh.no_numbers')));
      return out;
    }
    const metricSel = select(
      numbers.map((n) => ({ value: n.field, label: n.label })),
      def.field,
      (v) => patch({ metric: v }),
      { id: 'rh-metric', dataset: { rhFocus: 'metric' } },
    );
    const groupSel = select([...(desc.groupBy || []).map((f) => ({ value: f, label: t(`rh.f.${f}`) })), { value: 'none', label: t('rh.group_none') }], st.group || 'none', (v) => patch({ group: v }), {
      id: 'rh-group',
      dataset: { rhFocus: 'group' },
    });
    head.append(
      h(
        'div',
        { class: 'row gap wrap' },
        h('div', { class: 'field filter' }, h('label', { for: 'rh-metric' }, t('rh.metric')), metricSel),
        (desc.groupBy || []).length ? h('div', { class: 'field filter' }, h('label', { for: 'rh-group' }, t('rh.group_by')), groupSel) : null,
      ),
    );
    const groups = trendGroups(view, def, st.group);
    const pts = groups.flatMap((g) => g.points);
    // Dates (MM-DD) when the runs span days; with everything inside three days the date alone would repeat, so add the time.
    const span = (view.at(-1)?.ts ?? 0) - (view[0]?.ts ?? 0);
    const dateOf = (i) => (span < 3 * 86400 ? fmtRowTime(view[i]) : fmtRowTime(view[i]).slice(0, 5));
    const labelFor = (id) => view.find((r) => r.id === id)?.label ?? id;
    const alt = t('rh.trend_alt', { metric: def.label, points: pts.length, runs: view.length });
    const build = (w) =>
      trendChart({
        groups,
        n: view.length,
        width: w,
        height: w < 420 ? 190 : 220,
        yFmt: (v) => fmtAxis(v, def.unit),
        valueFmt: (v) => fmtRunValue(v, def.unit),
        dateOf,
        xTickCount: Math.max(3, Math.min(6, Math.floor(w / (span < 3 * 86400 ? 120 : 80)))),
        labelOf: labelFor,
        a: A?.id,
        b: B?.id,
        onPick: (id, o) => pick(id, o.compare),
        ariaLabel: alt,
      });
    const chartHost = h('div', { class: 'chart-wrap-lg' });
    const first = build(560);
    if (first) mountFit(rs, chartHost, build, first);
    else chartHost.append(chartEmpty(t('rh.no_values', { metric: def.label })));
    body.append(chartHost);
    if (first) {
      if (st.group)
        body.append(
          h(
            'ul',
            { class: 'legend' },
            groups.map((g, gi) => h('li', lineSwatch(gi), h('span', `${/^\d+$/.test(g.key) ? `${t(`rh.f.${st.group}`)} ` : ''}${g.key} (${g.count})`))),
          ),
        );
      body.append(
        h('p', { class: 'muted small' }, t('rh.trend_hint')),
        dataTable(t('chart.data_table'), [t('rh.col_run'), t('rh.col_time'), t('rh.col_group'), t('rh.col_value')], () =>
          groups.flatMap((g) => g.points.map((p) => [labelFor(p.id), fmtRowTime({ ts: p.ts }), g.key, fmtRunValue(p.v, def.unit)])),
        ),
      );
    }
    return out;
  }

  // ---- A/B compare
  function compareCard(rows, A, B, numbers, patch) {
    const newest = [...rows].reverse();
    const opts = [{ value: '', label: '–' }, ...newest.map((r) => ({ value: r.id, label: `${fmtRowTime(r)} · ${r.label} · ${r.verdict}` }))];
    const selA = select(opts, A?.id ?? '', (v) => patch({ run: v || null, vs: v && v === B?.id ? null : B?.id }), { id: 'rh-a', dataset: { rhFocus: 'sel-a' } });
    const selB = select(opts, B?.id ?? '', (v) => patch({ vs: v && v !== A?.id ? v : null }), { id: 'rh-b', dataset: { rhFocus: 'sel-b' } });
    const body = h('div', { class: 'card-body' });
    body.append(
      h(
        'div',
        { class: 'row gap wrap bottom rh-pick' },
        h('div', { class: 'field filter grow' }, h('label', { for: 'rh-a' }, t('rh.run_a')), selA),
        h('div', { class: 'field filter grow' }, h('label', { for: 'rh-b' }, t('rh.run_b')), selB),
        btn(t('rh.swap'), { size: 'sm', disabled: !A || !B, onClick: () => patch({ run: B?.id ?? null, vs: A?.id ?? null }) }),
        btn(t('rh.clear'), { size: 'sm', disabled: !A && !B, onClick: () => patch({ run: null, vs: null }) }),
      ),
    );
    if (!A) {
      body.append(h('p', { class: 'hint' }, t('rh.compare_hint')));
      return h('section', { class: 'card rh-card', 'aria-label': t('rh.compare') }, h('header', { class: 'card-head' }, h('h3', t('rh.compare'))), body);
    }
    const cmp = compareRuns(A, B, numbers, { outcomeVerdict: !!desc.outcomeVerdict });
    const head = [t('rh.col_number'), `A · ${fmtRowTime(A)}`, ...(B ? [`B · ${fmtRowTime(B)}`, t('rh.col_diff'), t('rh.col_pct'), t('rh.col_mark')] : [])];
    const facts = [
      [t('rh.fact_id'), h('code', A.id), B ? h('code', B.id) : null],
      [t('rh.fact_verdict'), verdictChip(A), B ? verdictChip(B) : null],
      ...[...new Set(desc.groupBy || [])]
        .filter((f) => !numbers.some((n) => n.field === f))
        .map((f) => [t(`rh.f.${f}`), present(getField(A.fields, f)) ? String(getField(A.fields, f)) : '–', B ? (present(getField(B.fields, f)) ? String(getField(B.fields, f)) : '–') : null]),
    ];
    const tr = (cells, cls) =>
      h(
        'tr',
        { class: cls },
        cells.map((c, i) => (i === 0 ? h('th', { scope: 'row' }, c) : h('td', { class: `num al-right${B && i === 4 ? ' rh-col-pct' : ''}` }, c))),
      );
    const factRows = facts.map(([label, a, b], i) => tr(B ? [label, a, b, '', '', ''] : [label, a], i === 0 ? 'rh-fact rh-fact-id' : 'rh-fact'));
    const numRows = (B ? cmp : numbers.filter((n) => valueOf(A, n) != null).map((n) => ({ ...n, a: valueOf(A, n) }))).map((r) => {
      if (!B) return tr([r.label, fmtRunValue(r.a, r.unit)]);
      const mark = r.mark === 'na' || r.mark === 'none' ? h('span', { class: 'muted' }, '–') : badge(`${MARK_GLYPH[r.mark]} ${t(`rh.mark_${r.mark}`)}`, MARK_KIND[r.mark]);
      return tr([r.label, fmtRunValue(r.a, r.unit), fmtRunValue(r.b, r.unit), fmtDelta(r.diff, r.unit), pctText(r.pct), mark], `rh-num rh-${r.mark}`);
    });
    append(body, [
      h(
        'div',
        { class: 'tablewrap' },
        h(
          'table',
          { class: 'compact rh-table', 'aria-label': t('rh.compare') },
          h(
            'thead',
            h(
              'tr',
              head.map((c, i) => h('th', { scope: 'col', class: `${i ? 'al-right' : ''}${B && i === 4 ? ' rh-col-pct' : ''}`.trim() }, c)),
            ),
          ),
          h('tbody', factRows, numRows),
        ),
      ),
      B && !cmp.length ? h('p', { class: 'hint' }, t('rh.no_common')) : null,
      B ? h('p', { class: 'muted small' }, t('rh.compare_note')) : null,
    ]);
    return h('section', { class: 'card rh-card', 'aria-label': t('rh.compare') }, h('header', { class: 'card-head' }, h('h3', t('rh.compare'))), body);
  }

  load();
  scope.every(30000, load);
  return h('div', { class: 'stack-v' }, h('p', { class: 'hint' }, t('rh.intro')), host);
}
