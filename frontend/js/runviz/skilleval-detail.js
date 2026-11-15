// SkillEvaluator run detail (addendum 9.15), hung on the run history's selection, from `GET /systems/skillevaluator/runs/{id}/summary` (viewer route: numbers and
// identifiers only; no finding text, criteria, notes or conclusions, which are skill- or model-influenced content): a validator x run heatmap of status and finding
// severity over the latest Tier 1 runs shown, rubric bars (score and pass) and quality dimensions for run A (and B), and the Tier 3 lift as a dumbbell per agent and per
// metric (with vs without the skill). The Tier 3 card says a skill can influence its own Tier 3 score. One fetch per run, kept for the page's life; at most
// HEAT_RUNS runs feed the heatmap. A figure a run lacks is an en dash, never 0. Words: i18n keys `skv.*`, `rv.*`, `viz.*`.
import { h } from '../dom.js';
import { t } from '../i18n.js';
import { fmtRowTime } from '../runs.js';
import { validatorMatrix, rubricOf, dimensionsOf, scoreMax, liftAgents, liftRows, SEVERITIES } from '../skilleval-model.js';
import { hbarChart, dumbbellChart } from '../charts/bar-charts.js';
import { cellMatrix } from '../charts/cell-matrix.js';
import { dataTable, chartEmpty } from '../charts/parts.js';
import { vizCard, chartPanel, note, loadingBlock, failBlock, fetchCache, routeId, enc, repaintKeepFocus } from './kit.js';

const SYS = 'skillevaluator';
/** How many of the latest Tier 1 runs feed the heatmap (one summary fetch each). */
export const HEAT_RUNS = 10;
const hasTier = (row, k) => !!row?.event?.data?.[k] && typeof row.event.data[k] === 'object';
const newestWith = (rows, k) => [...rows].reverse().find((r) => hasTier(r, k)) ?? null;
const score = (v) => (v == null ? '–' : String(+v.toFixed(3)));
const signed = (d) => `${d > 0 ? '+' : d < 0 ? '−' : ''}${Math.abs(d).toFixed(3)}`;

export function skillevalDetail(scope) {
  const el = h('div', { class: 'stack-v rv-detail' });
  const summaries = fetchCache((id) => scope.api.get(`/systems/${SYS}/runs/${enc(id)}/summary`));
  let ctx = null,
    rs = null;
  const again = () => {
    if (scope.alive && ctx) paint();
  };
  const entry = (row) => (row ? summaries.peek(routeId(row)) : null);
  const stateCard = (title, e, wide = true) =>
    e.state === 'fail'
      ? vizCard(
          { title, wide },
          failBlock(e.error, () => {
            summaries.retry(e.key, again);
            paint();
          }),
        )
      : vizCard({ title, wide }, loadingBlock());

  // ---- validators x runs
  function heatCard(runsAll) {
    const rows = runsAll.filter((r) => hasTier(r, 'tier1')).slice(-HEAT_RUNS);
    const loaded = rows.map((r) => ({ row: r, e: entry(r) })).filter((x) => x.e.state === 'ok');
    const pending = rows.length - loaded.length - rows.filter((r) => entry(r).state === 'fail').length;
    const failed = rows.filter((r) => entry(r).state === 'fail').length;
    const m = validatorMatrix(loaded.map((x) => ({ id: x.row.id, label: x.row.label, ts: x.row.ts, summary: x.e.value })));
    const cols = m.cols.map((c) => ({ id: c.id, label: fmtRowTime(c), title: `${c.label} · ${fmtRowTime(c)}` }));
    const cell = (ri, ci) => {
      const c = m.rows[ri].cells[ci];
      if (!c) return null;
      const counts = SEVERITIES.filter((s) => c.counts[s] > 0).map((s) => `${c.counts[s]} ${t(`skv.sev.${s}`)}`);
      const word = t(`skv.state.${c.state}`);
      return {
        cls: `cm-${c.state}`,
        tone: c.state,
        glyph: c.glyph,
        text: c.letter,
        label: t('skv.cell', { validator: m.rows[ri].name, run: cols[ci].title, state: word, findings: counts.length ? counts.join(', ') : t('skv.no_findings') }),
      };
    };
    const legend = h(
      'ul',
      { class: 'legend' },
      ['ok', 'warn', 'bad', 'muted'].map((s) => h('li', h('i', { class: `sw cm-${s} cm-sw`, 'aria-hidden': 'true' }), h('span', t(`skv.key.${s}`)))),
      h('li', h('span', { class: 'muted small' }, t('skv.letters'))),
    );
    return vizCard(
      { title: t('skv.heat_title'), hint: t('skv.heat_hint', { runs: HEAT_RUNS }), wide: true },
      pending > 0 ? note(t('skv.heat_loading', { count: pending }), 'info') : null,
      failed > 0 ? note(t('skv.heat_failed', { count: failed }), 'warn') : null,
      rows.length ? legend : null,
      chartPanel(rs, {
        build: (w) =>
          m.rows.length
            ? cellMatrix({
                cols,
                rows: m.rows.map((r) => ({ id: r.name, label: r.name })),
                cell,
                width: w,
                ariaLabel: t('skv.heat_alt', { validators: m.rows.length, runs: cols.length }),
                noDataLabel: t('skv.not_run'),
                onPick: (id) => ctx.pick(id, false),
                selected: { a: ctx.A?.id, b: ctx.B?.id },
              })
            : null,
        empty: rows.length ? (pending > 0 ? loadingBlock() : chartEmpty(t('skv.heat_none'))) : chartEmpty(t('skv.heat_no_runs')),
      }),
      m.total > m.rows.length ? h('p', { class: 'muted small' }, t('skv.heat_more', { shown: m.rows.length, total: m.total })) : null,
      m.rows.length
        ? dataTable(t('chart.data_table'), [t('skv.validator'), ...cols.map((c) => c.title)], () =>
            m.rows.map((r) => [r.name, ...r.cells.map((c) => (c ? `${c.glyph} ${t(`skv.state.${c.state}`)}${c.letter ? ` (${c.letter}${c.total})` : ''}` : '–'))]),
          )
        : null,
    );
  }

  // ---- rubric and dimensions
  function rubricCard(A, B, defaulted) {
    const title = t('skv.rubric_title');
    const ea = entry(A);
    if (!A) return vizCard({ title, wide: true }, chartEmpty(t('skv.rubric_none')));
    if (ea.state !== 'ok') return stateCard(title, { ...ea, key: routeId(A) });
    const eb = B ? entry(B) : null;
    const ra = rubricOf(ea.value),
      rb = eb?.state === 'ok' ? rubricOf(eb.value) : [];
    const ids = [...new Set([...ra.map((r) => r.id), ...rb.map((r) => r.id)])];
    const find = (list, id) => list.find((r) => r.id === id);
    const max = scoreMax([...ra, ...rb].map((r) => r.score));
    const bar = (tag, key, cls, r) =>
      r && r.score != null
        ? {
            key,
            value: r.score,
            cls: `${cls} ${r.pass === false ? 'hb-v-bad' : r.pass === true ? 'hb-v-ok' : ''}`.trim(),
            text: t('skv.rubric_tip', {
              run: tag,
              id: r.id,
              score: score(r.score),
              pass: r.pass == null ? '–' : t(r.pass ? 'skv.pass' : 'skv.fail'),
              judge: r.judgePass == null ? '–' : t(r.judgePass ? 'skv.pass' : 'skv.fail'),
              importance: r.importance ?? '–',
            }),
            valueText: `${score(r.score)} ${r.pass == null ? '' : r.pass ? '✓' : '✕'}`.trim(),
          }
        : { key, value: null, cls, text: t('skv.rubric_missing', { run: tag, id: r?.id ?? '' }) };
    const rows = ids.map((id) => ({ id, label: id, bars: [bar('A', 'a', 'bf1', find(ra, id)), ...(B ? [bar('B', 'b', 'bf2 hb-b', find(rb, id))] : [])] }));
    const da = dimensionsOf(ea.value),
      db = eb?.state === 'ok' ? dimensionsOf(eb.value) : [];
    const dnames = [...new Set([...da.map((d) => d.name), ...db.map((d) => d.name)])];
    const dmax = scoreMax([...da, ...db].map((d) => d.value));
    const dimRows = dnames.map((n) => ({
      id: n,
      label: n,
      bars: [
        {
          key: 'a',
          value: da.find((d) => d.name === n)?.value ?? null,
          cls: 'bf1',
          text: t('skv.dim_tip', { run: 'A', name: n, value: score(da.find((d) => d.name === n)?.value) }),
          valueText: score(da.find((d) => d.name === n)?.value),
        },
        ...(B
          ? [
              {
                key: 'b',
                value: db.find((d) => d.name === n)?.value ?? null,
                cls: 'bf2 hb-b',
                text: t('skv.dim_tip', { run: 'B', name: n, value: score(db.find((d) => d.name === n)?.value) }),
                valueText: score(db.find((d) => d.name === n)?.value),
              },
            ]
          : []),
      ],
    }));
    return vizCard(
      { title, hint: t('skv.rubric_hint'), wide: true },
      defaulted ? note(t('skv.default_run', { run: A.label, time: fmtRowTime(A) }), 'info') : null,
      B && eb?.state !== 'ok' ? note(t('skv.b_unavailable'), 'warn') : null,
      h(
        'ul',
        { class: 'legend' },
        h('li', h('i', { class: 'sw bf1', 'aria-hidden': 'true' }), h('span', `A · ${A.label}`)),
        B ? h('li', h('i', { class: 'sw bf2 hb-b-sw', 'aria-hidden': 'true' }), h('span', `B · ${B.label}`)) : null,
        h('li', h('span', { class: 'muted small' }, t('skv.pass_key'))),
      ),
      chartPanel(rs, {
        title: t('skv.rubric_sub'),
        build: (w) =>
          rows.length ? hbarChart({ rows, width: w, ariaLabel: t('skv.rubric_alt', { count: rows.length }), axisFmt: (v) => String(+v.toFixed(2)), domain: { min: 0, max }, tailW: 84 }) : null,
        empty: chartEmpty(t('skv.rubric_empty')),
      }),
      dimRows.length
        ? chartPanel(rs, {
            title: t('skv.dim_title'),
            build: (w) =>
              hbarChart({ rows: dimRows, width: w, ariaLabel: t('skv.dim_alt', { count: dimRows.length }), axisFmt: (v) => String(+v.toFixed(2)), domain: { min: 0, max: dmax }, tailW: 84 }),
            empty: chartEmpty(t('skv.rubric_empty')),
          })
        : null,
      rows.length
        ? dataTable(t('chart.data_table'), [t('skv.criterion'), t('skv.importance'), 'A', t('skv.pass_col'), t('skv.judge_col'), ...(B ? ['B'] : [])], () =>
            ids.map((id) => [
              id,
              find(ra, id)?.importance ?? find(rb, id)?.importance ?? '–',
              score(find(ra, id)?.score),
              find(ra, id)?.pass == null ? '–' : t(find(ra, id).pass ? 'skv.pass' : 'skv.fail'),
              find(ra, id)?.judgePass == null ? '–' : t(find(ra, id).judgePass ? 'skv.pass' : 'skv.fail'),
              ...(B ? [score(find(rb, id)?.score)] : []),
            ]),
          )
        : null,
    );
  }

  // ---- Tier 3 lift
  function liftCard(A, B, defaulted) {
    const title = t('skv.lift_title');
    const influence = h('p', { class: 'note warn skv-influence', role: 'note' }, h('strong', t('skv.influence')), ' ', t('skv.influence_more'));
    if (!A) return vizCard({ title, hint: t('skv.lift_hint'), wide: true }, influence, chartEmpty(t('skv.lift_none')));
    const ea = entry(A);
    if (ea.state !== 'ok') return stateCard(title, { ...ea, key: routeId(A) });
    const eb = B ? entry(B) : null;
    const aa = liftAgents(ea.value),
      ab = eb?.state === 'ok' ? liftAgents(eb.value) : [];
    const rowsA = liftRows(aa, 'a').map((r) => ({ ...r, label: r.overall ? t('skv.overall') : r.label }));
    const rowsB = liftRows(ab, 'b').map((r) => ({ ...r, label: r.overall ? t('skv.overall') : r.label }));
    const block = (tag, run, rows) =>
      chartPanel(rs, {
        title: `${tag} · ${run.label}`,
        build: (w) =>
          dumbbellChart({
            rows,
            width: w,
            ariaLabel: t('skv.lift_alt', { run: tag, count: rows.filter((r) => !r.header).length }),
            axisFmt: (v) => String(+v.toFixed(2)),
            valueFmt: score,
            deltaFmt: signed,
          }),
        empty: chartEmpty(t('skv.lift_run_none', { run: tag })),
      });
    const table = [...rowsA.map((r) => ['A', r]), ...rowsB.map((r) => ['B', r])].filter(([, r]) => !r.header);
    return vizCard(
      { title, hint: t('skv.lift_hint'), wide: true },
      influence,
      defaulted ? note(t('skv.default_run', { run: A.label, time: fmtRowTime(A) }), 'info') : null,
      h(
        'ul',
        { class: 'legend' },
        h('li', h('i', { class: 'db-key without', 'aria-hidden': 'true' }), h('span', t('skv.without'))),
        h('li', h('i', { class: 'db-key with', 'aria-hidden': 'true' }), h('span', t('skv.with'))),
        h('li', h('span', { class: 'muted small' }, t('skv.lift_dir'))),
      ),
      block('A', A, rowsA),
      B ? (eb?.state === 'ok' ? block('B', B, rowsB) : note(t('skv.b_unavailable'), 'warn')) : null,
      table.length
        ? dataTable(t('chart.data_table'), [t('rh.col_run'), t('skv.metric'), t('skv.without'), t('skv.with'), t('skv.delta')], () =>
            table.map(([tag, r]) => [tag, r.label, score(r.a), score(r.b), r.delta == null ? '–' : signed(r.delta)]),
          )
        : null,
    );
  }

  function paint() {
    rs?.dispose();
    rs = scope.child();
    repaintKeepFocus(el, () => {
      const { view, A, B } = ctx;
      const A1 = A ?? newestWith(view, 'tier1');
      const A3 = A ?? newestWith(view, 'tier3');
      return [h('h2', { class: 'rv-h' }, t('rv.detail')), heatCard(view), rubricCard(A1, B, !A && !!A1), liftCard(A3, B, !A && !!A3)];
    });
  }

  return {
    el,
    update(c) {
      ctx = c;
      const want = new Set();
      for (const r of c.view.filter((x) => hasTier(x, 'tier1')).slice(-HEAT_RUNS)) want.add(routeId(r));
      for (const r of [c.A, c.B, c.A ? null : newestWith(c.view, 'tier1'), c.A ? null : newestWith(c.view, 'tier3')]) if (r) want.add(routeId(r));
      for (const id of want) summaries.load(id, again);
      paint();
    },
  };
}
