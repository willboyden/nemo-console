// Trajectories tab: ATIF trajectories (the gateway's, and agents' own) as a table, and with `&id=<id>` one trajectory
// as a vertical timeline of steps with a per-step token chart. Step content is fetched on demand, like span content.
import { h } from '../dom.js';
import { t } from '../i18n.js';
import { fmtInt, fmtBytes } from '../format.js';
import { describeError } from '../errors.js';
import { emptyBox, errorBox, skeleton, btn, card, badge, select } from '../components/ui.js';
import { svg, titled } from '../viz/svg.js';
import { stackedBars } from '../viz/series.js';
import { spreadIndices } from '../viz/scale.js';
import { fmtStamp, fmtDur, fmtTok, shortId } from '../viz/fmt.js';
import { createNav, navLink } from './nav.js';
import { liveLoad } from './live.js';
import { indexStrip } from './index-strip.js';
import { searchBox, resetButton } from './controls.js';
import { colTh, tripletShort, tokenBar, pager, copyBtn, kvList, legendList, dataTable, chartEmpty, swap, dash, statusChip, kindChip } from './ui.js';
import { contentSection, renderStepContent } from './content.js';
import { fitted } from './fit.js';
import { trajectoryProblem } from './pure.js';

export function trajectoriesTab(scope, ctx) {
  const nav = createNav('trajectories', ctx);
  const host = h('div', { class: 'stack-v rt-tab' });
  let sub = null,
    mode = null;
  const apply = () => {
    const key = nav.get('id') ? `d:${nav.get('id')}:${nav.get('source')}` : nav.get('trace') ? `t:${nav.get('trace')}` : 'list';
    if (key === mode) return;
    mode = key;
    sub?.dispose();
    sub = scope.child();
    swap(host, nav.get('id') ? trajectoryDetail(sub, nav) : nav.get('trace') ? resolveByTrace(sub, nav) : trajectoryList(sub, nav));
    if (key !== 'list') window.scrollTo?.({ top: 0 });
  };
  nav.onChange(apply);
  apply();
  return host;
}

const params = (nav) => ({
  limit: [25, 50, 100, 200].includes(Number(nav.get('limit'))) ? Number(nav.get('limit')) : 50,
  offset: Math.max(0, parseInt(nav.get('offset'), 10) || 0),
  ...Object.fromEntries(['source', 'q'].filter((k) => nav.get(k)).map((k) => [k, nav.get(k)])),
});
/** Source (the directory a trajectory came from) is only needed in the query when two sources share an id. */
const withSource = (nav) => (nav.get('source') ? { source: nav.get('source') } : undefined);

/** A link from a trace names the trace, not the trajectory: ask for the trajectories linked to it (`?trace=`), then open the first. */
function resolveByTrace(scope, nav) {
  const trace = nav.get('trace');
  const body = h('div', { class: 'stack-v' }, skeleton(2));
  const mkBack = () => navLink(nav, { trace: null }, { class: 'btn sm' }, `← ${t('rt.traj.back')}`);
  scope.api
    .trajectories({ trace, limit: 1 })
    .then((d) => {
      if (!scope.alive) return;
      const hit = (d.items || [])[0];
      if (hit) nav.set({ id: hit.id, trace: null, source: hit.source }, { push: false });
      else swap(body, emptyBox(t('rt.traj.none_for_trace'), t('rt.traj.none_for_trace_hint'), mkBack()));
    })
    .catch((e) => scope.alive && swap(body, errorBox(e)));
  return h('div', { class: 'stack-v' }, h('div', { class: 'row gap' }, mkBack()), body);
}

function trajectoryList(scope, nav) {
  const tableHost = h('div', { class: 'tbl-host' }, skeleton(4));
  const noteHost = h('div');
  const sources = new Set(['gateway']);
  const refreshAll = () => {
    live.forget();
    live.refresh();
  };
  const srcSel = select([{ value: '', label: t('rt.filter.all') }], nav.get('source'), (v) => {
    nav.set({ source: v, offset: null });
    refreshAll();
  });
  srcSel.id = 'flt-source';
  const srcWrap = h('div', { class: 'field filter' }, h('label', { for: 'flt-source' }, t('rt.f.source')), srcSel);
  const drawSources = () => {
    const cur = nav.get('source');
    const all = [...new Set([...sources, ...(cur ? [cur] : [])])].sort();
    swap(srcSel, h('option', { value: '' }, t('rt.filter.all')), ...all.map((s) => h('option', { value: s }, s)));
    srcSel.value = cur;
  };
  drawSources();
  const search = searchBox({ name: 'q', label: t('rt.f.search'), nav, onChange: refreshAll, placeholder: t('rt.f.traj_search_ph') });
  const reset = resetButton(() => {
    nav.set({ source: null, q: null, offset: null });
    search.input.value = '';
    drawSources();
    refreshAll();
  });
  const strip = indexStrip(scope, nav);
  function render(data) {
    for (const x of data.items || []) sources.add(x.source);
    drawSources();
    const items = data.items || [];
    if (!items.length) return swap(tableHost, emptyBox(t('rt.traj.empty'), t('rt.traj.empty_hint'), btn(t('rt.filter.reset'), { size: 'sm', onClick: () => reset.click() })));
    const idCount = new Map();
    for (const x of items) idCount.set(x.id, (idCount.get(x.id) ?? 0) + 1);
    const head = h(
      'tr',
      ['rt.col.traj', 'rt.col.source', 'rt.col.agent', 'rt.col.steps', 'rt.col.tools', 'rt.col.tokens', 'rt.col.last', 'rt.col.file', 'rt.col.trace'].map((k, i) =>
        colTh(k, i === 3 || i === 4 ? 'al-right' : ''),
      ),
    );
    const rows = items.map((r) => {
      const q = { id: r.id, source: idCount.get(r.id) > 1 ? r.source : null };
      const tr = h(
        'tr',
        { class: 'trace-row' },
        h('td', h('span', { class: 'row gap nowrap' }, navLink(nav, q, { class: 'row-link', title: r.id }, h('code', shortId(r.id))), copyBtn(r.id, t('rt.sd.copy_id')))),
        h('td', kindChip(r.source)),
        h('td', h('div', `${r.agent?.name ?? '–'}${r.agent?.version ? ` ${r.agent.version}` : ''}`), h('div', { class: 'muted small' }, r.agent?.model ?? '–')),
        h('td', { class: 'num al-right' }, fmtInt(r.steps)),
        h('td', { class: 'num al-right' }, fmtInt(r.tool_calls)),
        h(
          'td',
          { class: 'nowrap' },
          h('div', { class: 'num small tok-text' }, tripletShort(r.prompt_tokens, r.completion_tokens, r.cached_tokens)),
          tokenBar(r.prompt_tokens, r.completion_tokens, r.cached_tokens),
        ),
        h('td', { class: 'num nowrap' }, fmtStamp(r.last_ts ?? r.mtime)),
        h('td', h('div', { class: 'small wrap-anywhere' }, h('code', r.file ?? '–')), h('div', { class: 'muted small' }, fmtBytes(r.bytes))),
        h('td', r.trace_id ? navLink(nav, { trace: r.trace_id, id: null, source: null, q: null }, { class: 'row-link nowrap' }, h('code', r.trace_id.slice(0, 8)), 'traces') : dash()),
      );
      tr.addEventListener('click', (e) => !e.target.closest('a,button') && nav.go(q));
      return tr;
    });
    const p = params(nav);
    swap(
      tableHost,
      h('div', { class: 'tablewrap' }, h('table', { class: 'compact trace-table', 'aria-label': t('rt.traj.table') }, h('thead', head), h('tbody', rows))),
      pager({
        offset: data.offset ?? p.offset,
        limit: data.limit ?? p.limit,
        total: data.total ?? items.length,
        onPage: (off) => {
          nav.set({ offset: off || null });
          refreshAll();
        },
      }),
    );
  }
  const live = liveLoad(scope, {
    load: (signal) => scope.api.trajectories(params(nav), { signal }),
    ms: 15000,
    onData: (data, { changed }) => {
      swap(noteHost);
      if (changed || !tableHost.querySelector('table,.state')) render(data);
    },
    onError: (e, { quiet }) => {
      if (quiet && tableHost.querySelector('table')) swap(noteHost, h('p', { class: 'note warn', role: 'status' }, t('rt.refresh_failed', { why: describeError(e) })));
      else swap(tableHost, errorBox(e, refreshAll));
    },
  });
  live.refresh();
  return h('div', { class: 'stack-v' }, strip.el, h('div', { class: 'rt-filters' }, srcWrap, search, h('div', { class: 'field filter end' }, reset)), noteHost, tableHost);
}

const SRC_KIND = { user: 'info', agent: 'ok', system: 'muted' };

/** Per-step token chart: stacked [cached, uncached prompt, completion] for each step that has metrics. */
function stepChart(steps, { onPick, width: W = 700 }) {
  const m = steps.map((s) => ({ s, p: s.metrics?.prompt_tokens, c: s.metrics?.completion_tokens, k: s.metrics?.cached_tokens })).filter((x) => x.p != null || x.c != null);
  if (!m.length) return null;
  const H = 220;
  const cached = m.map((x) => Math.min(x.k ?? 0, x.p ?? 0)),
    unc = m.map((x, i) => Math.max(0, (x.p ?? 0) - cached[i])),
    out = m.map((x) => x.c ?? 0);
  const g = stackedBars({
    ts: m.map((_, i) => i),
    bucketS: 1,
    series: [
      { key: 'cached', values: cached },
      { key: 'unc', values: unc },
      { key: 'out', values: out },
    ],
    width: W,
    height: H,
    maxBarW: 46,
  });
  const cls = ['bf2', 'bf1', 'bf4'],
    lab = [t('rt.ctx.lg_cached'), t('rt.ctx.lg_in'), t('rt.ctx.lg_out')];
  const alt = t('rt.traj.chart_alt', { n: m.length });
  const rects = g.rects.map((r) => {
    const x = svg('rect', { class: cls[r.si], x: r.x, y: r.y, width: r.w, height: r.h });
    titled(x, `${t('rt.traj.step', { n: m[r.i].s.step_id })} · ${lab[r.si]}: ${fmtInt(r.v)}`);
    x.addEventListener('click', () => onPick?.(m[r.i].s.step_id));
    return x;
  });
  const band = g.time.bands;
  const labels = spreadIndices(m.length, 12).map((i) => svg('text', { class: 'tick', x: band[i].cx, y: H - 7, 'text-anchor': 'middle' }, String(m[i].s.step_id)));
  const grid = g.yTicks.flatMap((tk) => [
    svg('line', { class: 'gl', x1: g.plot.x0, x2: g.plot.x1, y1: tk.y, y2: tk.y }),
    svg('text', { class: 'tick', x: g.plot.x0 - 6, y: tk.y + 4, 'text-anchor': 'end' }, fmtTok(tk.v)),
  ]);
  return svg('svg', { class: 'chart rtchart', viewBox: `0 0 ${W} ${H}`, role: 'group', 'aria-label': alt }, svg('title', alt), ...grid, ...rects, ...labels);
}

function trajectoryDetail(scope, nav) {
  const id = nav.get('id');
  const mkBack = () => navLink(nav, { id: null, source: null }, { class: 'btn sm' }, `← ${t('rt.traj.back')}`);
  const body = h('div', { class: 'stack-v' }, skeleton(4));
  let data = null,
    shown = 100;
  const stepHost = h('ol', { class: 'timeline', 'aria-label': t('rt.traj.steps') });
  const focusStep = (n) => {
    const i = data.steps.findIndex((s) => s.step_id === n);
    if (i >= shown) {
      shown = i + 1;
      drawSteps();
    }
    const el = stepHost.querySelector(`[data-step="${n}"]`);
    el?.scrollIntoView?.({ block: 'center' });
    el?.querySelector('h4')?.setAttribute('tabindex', '-1');
    el?.querySelector('h4')?.focus?.();
  };
  function stepCard(s, t0) {
    const tr = data.trajectory;
    const content = contentSection({
      scope,
      compact: true,
      load: () => scope.api.stepContent(tr.id, s.step_id, withSource(nav)),
      render: (c) => renderStepContent(c),
    });
    const mt = s.metrics || {};
    return h(
      'li',
      { class: `tl-item src-${s.source}${s.status === 'error' ? ' err' : ''}`, dataset: { step: s.step_id } },
      h('span', { class: 'tl-dot', 'aria-hidden': 'true' }),
      h(
        'div',
        { class: 'tl-card' },
        h(
          'header',
          { class: 'row gap wrap between' },
          h(
            'div',
            { class: 'row gap wrap' },
            h('h4', t('rt.traj.step', { n: s.step_id })),
            badge(t(`rt.traj.src_${s.source}`) === `rt.traj.src_${s.source}` ? String(s.source) : t(`rt.traj.src_${s.source}`), SRC_KIND[s.source] ?? 'muted'),
            s.model ? h('code', { class: 'small' }, s.model) : null,
            s.status ? statusChip(s.status) : null,
          ),
          h('span', { class: 'muted small num' }, s.ts != null ? `+${((s.ts - t0) / 1).toFixed(1)}s` : '', s.duration_ms != null ? ` · ${fmtDur(s.duration_ms)}` : ''),
        ),
        h(
          'div',
          { class: 'tl-meta small' },
          h('span', t('rt.traj.chars', { message: fmtInt(s.message_chars), reasoning: fmtInt(s.reasoning_chars) })),
          mt.prompt_tokens != null || mt.completion_tokens != null
            ? h('span', { class: 'num' }, t('rt.tokens.triplet', { input: fmtTok(mt.prompt_tokens), output: fmtTok(mt.completion_tokens), cached: fmtTok(mt.cached_tokens) }))
            : null,
          mt.prompt_tokens != null || mt.completion_tokens != null ? tokenBar(mt.prompt_tokens, mt.completion_tokens, mt.cached_tokens) : null,
        ),
        s.tool_calls?.length
          ? h(
              'div',
              { class: 'chips' },
              s.tool_calls.map((c) => h('span', { class: 'chip', title: t('rt.traj.args_chars', { n: fmtInt(c.arguments_chars) }) }, `→ ${c.name}`)),
              s.observations ? h('span', { class: 'chip more' }, t('rt.traj.observations', { n: s.observations })) : null,
            )
          : s.observations
            ? h('span', { class: 'chip more' }, t('rt.traj.observations', { n: s.observations }))
            : null,
        tr.trace_id && s.span_id ? navLink(nav, { trace: tr.trace_id, span: s.span_id, id: null, source: null }, { class: 'small' }, t('rt.traj.open_span'), 'traces') : null,
        content,
      ),
    );
  }
  function drawSteps() {
    const steps = data.steps || [];
    const t0 = steps[0]?.ts ?? 0;
    const more = steps.length - shown;
    swap(
      stepHost,
      steps.slice(0, shown).map((s) => stepCard(s, t0)),
    );
    swap(
      moreHost,
      more > 0
        ? h(
            'div',
            { class: 'row gap' },
            h('span', { class: 'muted small' }, t('rt.traj.more_hint', { n: fmtInt(more) })),
            btn(t('rt.traj.show_more'), { size: 'sm', onClick: () => ((shown += 100), drawSteps()) }),
          )
        : '',
    );
  }
  const moreHost = h('div');
  let chartScope = null;
  function render() {
    const tr = data.trajectory,
      steps = data.steps || [];
    chartScope?.dispose();
    chartScope = scope.child();
    const chart = fitted(chartScope, (w) => stepChart(steps, { onPick: focusStep, width: w }), null, 700);
    swap(
      body,
      h(
        'section',
        { class: 'card' },
        h(
          'div',
          { class: 'card-body' },
          h(
            'div',
            { class: 'row gap wrap between' },
            h(
              'div',
              { class: 'row gap wrap' },
              h('h2', h('code', shortId(tr.id, 12, 6))),
              copyBtn(tr.id, t('rt.sd.copy_id')),
              kindChip(tr.source),
              badge(`${tr.agent?.name ?? '–'}${tr.agent?.version ? ` ${tr.agent.version}` : ''}`, 'muted'),
            ),
            tr.trace_id
              ? navLink(nav, { trace: tr.trace_id, id: null, source: null }, { class: 'btn sm' }, t('rt.traj.open_trace'), 'traces')
              : h('span', { class: 'muted small' }, t('rt.traj.no_trace')),
          ),
          h(
            'div',
            { class: 'trace-facts' },
            kvList([
              [t('rt.f.model'), tr.agent?.model],
              [t('rt.col.session'), tr.session_id ? h('code', { class: 'small wrap-anywhere' }, tr.session_id) : null],
              [t('rt.traj.schema'), tr.schema_version],
              [t('rt.col.file'), h('code', { class: 'small wrap-anywhere' }, tr.file ?? '–')],
            ]),
            kvList([
              [t('rt.col.steps'), fmtInt(tr.steps)],
              [t('rt.col.tools'), fmtInt(tr.tool_calls)],
              [t('rt.sessions.first'), `${fmtStamp(tr.first_ts)} UTC`],
              [t('rt.sessions.last'), `${fmtStamp(tr.last_ts)} UTC`],
            ]),
            h(
              'div',
              { class: 'trace-tokens' },
              h('span', { class: 'kv-k muted' }, t('rt.trace.tokens')),
              tokenBar(tr.prompt_tokens, tr.completion_tokens, tr.cached_tokens),
              h('div', { class: 'num small' }, t('rt.tokens.triplet', { input: fmtTok(tr.prompt_tokens), output: fmtTok(tr.completion_tokens), cached: fmtTok(tr.cached_tokens) })),
            ),
          ),
        ),
      ),
      data.truncated ? h('p', { class: 'note warn', role: 'status' }, t('rt.traj.truncated')) : null,
      card(t('rt.traj.chart_title'), [
        h('p', { class: 'hint' }, t('rt.traj.chart_hint')),
        chart ?? chartEmpty(t('rt.traj.no_metrics')),
        chart
          ? legendList([
              { cls: 'bf2', label: t('rt.ctx.lg_cached') },
              { cls: 'bf1', label: t('rt.ctx.lg_in') },
              { cls: 'bf4', label: t('rt.ctx.lg_out') },
            ])
          : null,
        chart
          ? dataTable(t('chart.data_table'), [t('rt.traj.step_col'), t('rt.sd.tok_in'), t('rt.sd.tok_cached'), t('rt.sd.tok_out')], () =>
              steps
                .filter((s) => s.metrics?.prompt_tokens != null || s.metrics?.completion_tokens != null)
                .map((s) => [s.step_id, fmtInt(s.metrics.prompt_tokens), fmtInt(s.metrics.cached_tokens), fmtInt(s.metrics.completion_tokens)]),
            )
          : null,
      ]),
      card(t('rt.traj.steps_title', { n: fmtInt(steps.length) }), [steps.length ? stepHost : chartEmpty(t('rt.traj.no_steps')), moreHost]),
    );
    drawSteps();
  }
  const live = liveLoad(scope, {
    load: (signal) => scope.api.trajectory(id, withSource(nav), { signal }),
    onData: (d) => {
      data = d;
      render();
    },
    onError: (e) => {
      const problem = trajectoryProblem(e);
      if (problem) swap(body, emptyBox(problem.title, problem.text, mkBack()));
      else
        swap(
          body,
          errorBox(e, () => live.refresh()),
        );
    },
  });
  live.refresh();
  return h('div', { class: 'stack-v' }, h('div', { class: 'row gap wrap' }, mkBack()), body);
}
