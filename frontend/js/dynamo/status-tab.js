// The Status tab of the Dynamo page (addendum 9.14): the frontend and worker state, the numbers now, and under one window picker: latency
// FOR THE WINDOW (quantiles of the requests that finished since the previous poll) next to the since-start quantiles (labelled so), the
// latency breakdown (queue -> prefill -> decode, p50 and p95), a load chart (running / waiting, KV usage, preemptions), output tokens per
// second, the prefix-cache hit ratio, the capacity panel and, when the worker reports it, spec-decode acceptance by position. One series
// request feeds every chart; the window and each chart's line picker live in the URL hash. Cards are updated in place.
import { h } from '../dom.js';
import { t } from '../i18n.js';
import { describeError } from '../errors.js';
import { fmtInt, fmtMs, fmtRatio, fmtTick, fmtCompact } from '../format.js';
import { valueOf } from '../metrics.js';
import { btn, card, emptyBox, errorBox, select, stat } from '../components/ui.js';
import { statusDetails } from '../components/status-panel.js';
import { createHashState } from '../charts/hash-state.js';
import { seriesCard } from '../charts/series-card.js';
import { linesCard } from '../charts/lines-card.js';
import { stackCard } from '../charts/stack-card.js';
import { segPicker } from '../charts/controls.js';
import { labelledBar, swap } from '../charts/parts.js';
import { normalizeAnswer, rateSegments, valueSegments, pickRateScale, scaleSegments } from '../series-model.js';
import { subAnswer } from '../series-math.js';
import { DYN_KEYS, DYN_WINDOWS, PHASES, PHASE_QUANTILES, seriesOfKey, phaseSummary, phaseSeries, outputTokens, capacityModel, specAcceptModel } from '../dynamo-model.js';

const GAP_S = { '15m': 5, '1h': 15, '24h': 30, '7d': 60 };
const ms = (v) => (v == null ? '–' : fmtMs(v));
const pct = (v) => (v == null ? '–' : fmtRatio(v, 0));

export function statusTab(scope, ctx) {
  const hash = createHashState({ path: 'dynamo', tab: 'status', initial: ctx?.initial !== false });
  let win = DYN_WINDOWS.includes(hash.get('window')) ? hash.get('window') : '15m';
  let q = PHASE_QUANTILES.includes(hash.get('q')) ? hash.get('q') : 'p50';
  let answer = null,
    snap = null,
    seq = 0,
    lastAt = 0;
  const winFn = () => win;
  const compHost = h('div'),
    statsHost = h('div', { class: 'stats' }),
    note = h('div'),
    seriesNote = h('div'); // the series request's own notes (truncated, a failed refresh), apart from the metrics-unavailable note
  ctx.onStatus((st) => compHost.replaceChildren(statusDetails(st)));

  // ------------------------------------------------------------------------------------------------------------ the numbers now
  async function loadStats() {
    try {
      const m = await scope.api.metrics('dynamo');
      if (!scope.alive) return;
      snap = m;
      if (!m.available) {
        swap(statsHost);
        swap(note, h('p', { class: 'note warn', role: 'status' }, m.error || t('dyn.metrics_unavailable')));
        body.hidden = true;
        return;
      }
      swap(note);
      body.hidden = false;
      const v = (k, l) => valueOf(m, k, l);
      swap(
        statsHost,
        stat(t('dyn.requests'), fmtInt(v('dyn.requests_total'))),
        stat(t('dyn.in_flight'), fmtInt(v('dyn.in_flight'))),
        stat(t('dynv.running'), fmtInt(v('dyn.requests_running')), t('dynv.waiting_sub', { count: fmtInt(v('dyn.requests_waiting')) })),
        stat(t('dyn.ttft_p95'), fmtMs(v('dyn.ttft_ms', { q: 'p95' })), t('dynv.since_start')),
        stat(t('dyn.itl_p95'), fmtMs(v('dyn.itl_ms', { q: 'p95' })), t('dynv.since_start')),
        stat(t('dyn.kv'), fmtRatio(v('dyn.kv_cache_usage'), 0)),
        stat(t('dyn.models'), fmtInt(v('dyn.models'))),
      );
      drawCapacity();
      drawSpec();
    } catch (e) {
      if (scope.alive) swap(note, errorBox(e, loadStats));
    }
  }

  // ------------------------------------------------------------------------------------------------------------ series cards
  const cardOf = (title, key, opts = {}) =>
    seriesCard(scope, {
      title,
      key,
      rate: !!opts.rate,
      yFmt: opts.yFmt ?? fmtTick,
      valueFmt: opts.valueFmt ?? opts.yFmt ?? fmtTick,
      hash,
      win: winFn,
      showMax: () => false,
      emptyText: opts.emptyText,
    });
  const noReq = t('dynv.no_requests'); // the windowed keys are absent when no request finished: say so, never draw 0
  const ttftWin = cardOf(t('dynv.ttft_window'), 'dyn.ttft_window_ms', { yFmt: ms, valueFmt: ms, emptyText: noReq });
  const itlWin = cardOf(t('dynv.itl_window'), 'dyn.itl_window_ms', { yFmt: ms, valueFmt: ms, emptyText: noReq });
  const ttftStart = cardOf(t('dynv.ttft_start'), 'dyn.ttft_ms', { yFmt: ms, valueFmt: ms });
  const itlStart = cardOf(t('dynv.itl_start'), 'dyn.itl_ms', { yFmt: ms, valueFmt: ms });
  const tokens = cardOf(t('dynv.tokens_title'), 'dyn.tokens', { rate: true, yFmt: fmtTick, valueFmt: (v) => (v == null ? '–' : fmtCompact(v)) });
  const prefix = cardOf(t('dynv.prefix_title'), 'dyn.prefix_cache_hit_ratio', { yFmt: (v) => `${Math.round(v * 100)}%`, valueFmt: pct });

  // The load card: three panels on one time axis (running + waiting, KV usage, preemptions per second).
  const load = linesCard(scope, {
    title: t('dynv.load_title'),
    hint: t('dynv.load_hint'),
    wide: true,
    win: winFn,
    panels: [
      { id: 'queue', title: t('dynv.load_requests'), yFmt: fmtTick, valueFmt: (v) => (v == null ? '–' : fmtTick(v)), height: 130 },
      { id: 'kv', title: t('dynv.load_kv'), yFmt: (v) => `${Math.round(v * 100)}%`, valueFmt: pct, height: 130 },
      { id: 'preempt', title: t('dynv.load_preempt'), yFmt: (v) => `${+v.toFixed(2)}`, valueFmt: (v) => (v == null ? '–' : `${+v.toFixed(3)}/s`), height: 130 },
    ],
  });

  // The latency breakdown: a bar each for p50 and p95 (the window mean of every phase), and the chosen quantile over time.
  const breakdown = stackCard(scope, {
    title: t('dynv.phase_title'),
    hint: t('dynv.phase_hint'),
    parts: [
      { key: 'queue', name: t('dynv.phase_queue'), cls: 'bf3' },
      { key: 'prefill', name: t('dynv.phase_prefill'), cls: 'bf1' },
      { key: 'decode', name: t('dynv.phase_decode'), cls: 'bf2' },
    ],
    valueFmt: ms,
    axisFmt: (v) => fmtMs(v),
    rowsTitle: t('dynv.phase_mean'),
    wide: true,
    emptyText: noReq,
  });
  const qPicker = segPicker({
    values: PHASE_QUANTILES,
    current: () => q,
    labelOf: (x) => x,
    label: t('dynv.phase_q'),
    onPick: (x) => {
      q = x;
      hash.set({ q: x === 'p50' ? null : x });
      qPicker.redraw();
      draw();
    },
  });

  const capHost = h('div');
  const specHost = h('div');
  function drawCapacity() {
    const m = capacityModel(snap?.points || []);
    const tableEl = m.any
      ? h(
          'div',
          { class: 'tablewrap' },
          h(
            'table',
            { class: 'compact', 'aria-label': t('dynv.cap_title') },
            h('thead', h('tr', h('th', { scope: 'col' }, t('dynv.cap_item')), h('th', { scope: 'col', class: 'al-right' }, t('dynv.cap_value')), h('th', { scope: 'col' }, t('dynv.cap_used')))),
            h(
              'tbody',
              m.rows.map((r) =>
                h(
                  'tr',
                  h('th', { scope: 'row' }, t(`dynv.cap_${r.kind}`)),
                  h('td', { class: 'num al-right' }, fmtInt(r.value)),
                  h(
                    'td',
                    r.usedRatio == null
                      ? '–'
                      : h(
                          'div',
                          { class: 'sy-cell' },
                          labelledBar(r.usedRatio, 1, `${t(`dynv.cap_${r.kind}`)}: ${pct(r.usedRatio)}`, 'bf1'),
                          h('span', { class: 'num' }, `${fmtInt(r.used)} · ${pct(r.usedRatio)}`),
                        ),
                  ),
                ),
              ),
            ),
          ),
        )
      : emptyBox(t('dynv.cap_none'), t('dynv.cap_none_hint'));
    capHost.replaceChildren(card(t('dynv.cap_title'), h('div', { class: 'stack-v' }, m.any ? h('p', { class: 'hint' }, t('dynv.cap_hint')) : null, tableEl)));
  }
  function drawSpec() {
    const m = specAcceptModel(snap?.points || []);
    if (!m.any) {
      specHost.replaceChildren();
      return;
    }
    specHost.replaceChildren(
      card(
        t('dynv.spec_title'),
        h(
          'div',
          { class: 'stack-v' },
          h('p', { class: 'hint' }, t('dynv.spec_hint')),
          m.overall != null ? h('p', { class: 'muted small' }, t('dynv.spec_overall', { value: pct(m.overall) })) : null,
          h(
            'div',
            { class: 'spec-rows' },
            m.rows.map((r) =>
              h(
                'div',
                { class: 'busy-row' },
                h('span', { class: 'busy-name' }, t('dynv.spec_pos', { pos: r.pos })),
                labelledBar(r.ratio, 1, `${t('dynv.spec_pos', { pos: r.pos })}: ${pct(r.ratio)}`, 'bf2'),
                h('span', { class: 'num busy-val' }, pct(r.ratio)),
              ),
            ),
          ),
        ),
      ),
    );
  }

  // ------------------------------------------------------------------------------------------------------------ draw
  function draw() {
    if (!answer) return;
    const series = normalizeAnswer(answer);
    const step = answer.step_s;
    const meta = { from: answer.from, to: answer.to, stepS: step };
    for (const [c, key] of [
      [ttftWin, 'dyn.ttft_window_ms'],
      [itlWin, 'dyn.itl_window_ms'],
      [ttftStart, 'dyn.ttft_ms'],
      [itlStart, 'dyn.itl_ms'],
      [prefix, 'dyn.prefix_cache_hit_ratio'],
    ])
      c.update(subAnswer(answer, key));
    const out = outputTokens(series);
    tokens.update({ ...answer, series: out.series.map((s) => ({ key: s.key, label: null, points: s.points })) });
    // load panels
    const line = (name, idx, key, rate = false) => {
      const s = seriesOfKey(series, key)[0];
      if (!s) return null;
      const segments = rate ? rateSegments(s.points, step) : valueSegments(s.points, step);
      return segments.some((x) => x.length) ? { name, idx, segments } : null;
    };
    // preemptions are rare: per minute when the rate is below 0.1 a second (the panel title says which)
    const pre = seriesOfKey(series, 'dyn.preemptions_total')[0];
    const preRaw = pre ? rateSegments(pre.points, step) : [];
    const preScale = pickRateScale([preRaw]);
    const preSegs = scaleSegments(preRaw, preScale.factor);
    const preemptPanel = {
      title: preScale.per === 'min' ? t('dynv.load_preempt_min') : t('dynv.load_preempt'),
      valueFmt: (v) => (v == null ? '–' : `${+v.toPrecision(3)}${preScale.per === 'min' ? '/min' : '/s'}`),
      lines: preSegs.some((x) => x.length) ? [{ name: t('dynv.preempt'), idx: 3, segments: preSegs }] : [],
    };
    load.update({
      from: answer.from,
      to: answer.to,
      panels: [
        { lines: [line(t('dynv.running'), 0, 'dyn.requests_running'), line(t('dynv.waiting'), 1, 'dyn.requests_waiting')].filter(Boolean) },
        { lines: [line(t('dyn.kv'), 2, 'dyn.kv_cache_usage')].filter(Boolean) },
        preemptPanel,
      ],
    });
    // breakdown
    const sum = phaseSummary(series);
    const rowOf = (qn) => ({
      id: qn,
      label: qn,
      values: Object.fromEntries(PHASES.map((p) => [p, sum[qn][p]])),
      totalText: sum[qn].total == null ? '–' : `${sum[qn].partial ? '≥ ' : ''}${fmtMs(sum[qn].total)}`,
      note: sum[qn].partial ? t('dynv.phase_partial', { q: qn }) : null,
    });
    const rows = PHASE_QUANTILES.filter((qn) => sum[qn].any).map(rowOf);
    const ps = phaseSeries(series, q, meta);
    breakdown.update({ rows, time: ps.any ? [{ label: t('dynv.phase_over_time', { q }), ts: ps.ts, bucketS: ps.bucketS, series: ps.series }] : [] });
  }

  async function loadSeries(force = false) {
    if (!force && Date.now() - lastAt < (GAP_S[win] ?? 15) * 1000) return;
    lastAt = Date.now();
    const my = ++seq;
    try {
      const a = await scope.api.series('dynamo', { keys: DYN_KEYS, window: win, split: true });
      if (!scope.alive || my !== seq) return;
      answer = a;
      swap(seriesNote, a.truncated ? h('p', { class: 'note info', role: 'status' }, t('series.truncated')) : '');
      draw();
    } catch (e) {
      if (!scope.alive || my !== seq) return;
      const msg = describeError(e);
      if (!answer) for (const c of [ttftWin, itlWin, ttftStart, itlStart, tokens, prefix, load, breakdown]) c.fail(msg);
      else swap(seriesNote, h('p', { class: 'note warn', role: 'status' }, msg));
    }
  }

  const sel = select(
    DYN_WINDOWS.map((w) => ({ value: w, label: t(`window.${w}`) })),
    win,
    (v) => {
      win = v;
      hash.set({ window: v === '15m' ? null : v });
      loadSeries(true);
    },
    { 'aria-label': t('routing.window') },
  );
  const body = h(
    'div',
    { class: 'stack-v', hidden: true },
    h('div', { class: 'row gap wrap between' }, h('h2', t('dynv.latency_head')), h('p', { class: 'muted small' }, t('dynv.latency_lead'))),
    h('div', { class: 'grid two' }, ttftWin.el, itlWin.el, ttftStart.el, itlStart.el),
    h('div', { class: 'row gap wrap between' }, h('h2', t('dynv.breakdown_head')), h('div', { class: 'row gap' }, h('span', { class: 'muted small' }, t('dynv.phase_over_time_pick')), qPicker)),
    breakdown.el,
    h('h2', t('dynv.load_head')),
    load.el,
    h('div', { class: 'grid two' }, tokens.el, prefix.el),
    h('h2', t('dynv.engine_head')),
    h('div', { class: 'grid two' }, capHost, specHost),
  );
  drawCapacity();
  loadStats();
  loadSeries(true);
  scope.every(10000, loadStats);
  scope.every(5000, () => loadSeries(false));
  return h(
    'div',
    { class: 'stack-v' },
    card(t('dyn.serving'), compHost),
    h(
      'div',
      { class: 'row gap wrap between' },
      h('h2', t('dyn.metrics')),
      h('div', { class: 'row gap wrap' }, sel, btn(t('common.refresh'), { size: 'sm', icon: 'restart', onClick: () => (loadStats(), loadSeries(true)) })),
    ),
    note,
    statsHost,
    seriesNote,
    body,
  );
}
