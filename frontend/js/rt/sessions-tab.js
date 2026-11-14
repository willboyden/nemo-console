// Sessions tab: a table of sessions (the key shortened, with a copy button) and, with `&session=<key>`, one session:
// its traces on a single swim-lane timeline and a context growth chart built from its LLM calls.
import { h } from '../dom.js';
import { t } from '../i18n.js';
import { fmtInt } from '../format.js';
import { describeError } from '../errors.js';
import { emptyBox, errorBox, skeleton, btn, card, badge } from '../components/ui.js';
import { fmtStamp, fmtDur, fmtTok, fmtPercent, shortId } from '../viz/fmt.js';
import { createNav, navLink } from './nav.js';
import { liveLoad } from './live.js';
import { indexStrip, indexGate } from './index-strip.js';
import { rangeControls, facetSelect, searchBox, resetButton } from './controls.js';
import { colTh, tripletShort, refusedBox, kindChip, modelChips, tokenBar, sortTh, pager, copyBtn, kvList, statusChip, legendList, dataTable, chartEmpty, swap, dash } from './ui.js';
import { swimLane, contextChart } from './session-charts.js';
import { fitted } from './fit.js';

const SORTS = ['last', 'traces', 'tokens'];

export function sessionsTab(scope, ctx) {
  const nav = createNav('sessions', ctx);
  const host = h('div', { class: 'stack-v rt-tab' });
  let sub = null,
    mode = null;
  const apply = () => {
    const key = nav.get('session') ? `d:${nav.get('session')}` : 'list';
    if (key === mode) return;
    mode = key;
    sub?.dispose();
    sub = scope.child();
    swap(host, nav.get('session') ? sessionDetail(sub, nav) : sessionList(sub, nav));
    if (key !== 'list') window.scrollTo?.({ top: 0 });
  };
  nav.onChange(apply);
  apply();
  return host;
}

const params = (nav) => ({
  window: nav.get('window'),
  anchor: nav.get('anchor'),
  sort: SORTS.includes(nav.get('sort')) ? nav.get('sort') : 'last',
  order: nav.get('order') === 'asc' ? 'asc' : 'desc',
  limit: [25, 50, 100, 200].includes(Number(nav.get('limit'))) ? Number(nav.get('limit')) : 50,
  offset: Math.max(0, parseInt(nav.get('offset'), 10) || 0),
  ...Object.fromEntries(['client', 'kind', 'q'].filter((k) => nav.get(k)).map((k) => [k, nav.get(k)])),
});

function sessionList(scope, nav) {
  const tableHost = h('div', { class: 'tbl-host' }, skeleton(4));
  const noteHost = h('div');
  let gate = null,
    controls = null;
  const refreshAll = () => {
    live.forget();
    live.refresh();
  };
  const fClient = facetSelect({ name: 'client', label: t('rt.f.client'), nav, onChange: refreshAll });
  const fKind = facetSelect({ name: 'kind', label: t('rt.f.kind'), nav, onChange: refreshAll });
  const search = searchBox({ name: 'q', label: t('rt.f.session_search'), nav, onChange: refreshAll, placeholder: t('rt.f.session_search_ph') });
  const reset = resetButton(() => {
    nav.set({ client: null, kind: null, q: null, offset: null });
    search.input.value = '';
    fClient.update(fClient.facet);
    fKind.update(fKind.facet);
    refreshAll();
  });
  // The sessions answer carries no facets: take the client and kind options from the traces facets of the same window.
  const loadFacets = () =>
    scope.api
      .traces({ window: nav.get('window'), anchor: nav.get('anchor'), limit: 1 })
      .then((d) => {
        if (!scope.alive) return;
        fClient.facet = d.facets?.client;
        fKind.facet = d.facets?.kind;
        fClient.update(fClient.facet);
        fKind.update(fKind.facet);
      })
      .catch(() => {});
  const strip = indexStrip(scope, nav, {
    onIndex: (ix, meta) => {
      const g = indexGate(ix, nav);
      const was = gate;
      gate = g;
      if (controls) controls.hidden = !!g;
      if (!!g !== !!was || (g && meta.changed)) {
        if (g) swap(tableHost, g);
        else {
          // the gate box is a `.state`: replace it so the next answer always renders, changed or not
          swap(tableHost, skeleton(4));
          live.refresh();
        }
      }
    },
  });
  const st = () => ({ sort: params(nav).sort, order: params(nav).order });
  const onSort = (sort, order) => {
    nav.set({ sort, order, offset: null });
    refreshAll();
  };
  function render(data) {
    const items = data.items || [];
    if (!items.length) {
      swap(tableHost, emptyBox(t('rt.sessions.empty'), t('rt.sessions.empty_hint'), btn(t('rt.filter.reset'), { size: 'sm', onClick: () => reset.click() })));
      return;
    }
    const s = st();
    const head = h(
      'tr',
      h('th', { scope: 'col' }, t('rt.col.session')),
      h('th', { scope: 'col' }, t('rt.col.client')),
      sortTh(t('rt.col.traces'), 'traces', s, onSort, 'al-right'),
      h('th', { scope: 'col', class: 'al-right' }, t('rt.col.calls')),
      sortTh(t('rt.col.tokens'), 'tokens', s, onSort, '', t('rt.col.tokens_sub')),
      h('th', { scope: 'col', class: 'al-right' }, t('rt.col.errors')),
      h('th', { scope: 'col' }, t('rt.col.models')),
      h('th', { scope: 'col' }, t('rt.col.first')),
      sortTh(t('rt.col.last'), 'last', s, onSort),
      h('th', { scope: 'col', class: 'al-right' }, t('rt.col.span')),
    );
    const rows = items.map((r) => {
      const tr = h(
        'tr',
        { class: 'trace-row' },
        h(
          'td',
          h(
            'span',
            { class: 'row gap nowrap' },
            navLink(nav, { session: r.session_key }, { class: 'row-link', title: r.session_key }, h('code', shortId(r.session_key))),
            copyBtn(r.session_key, t('rt.sessions.copy_key')),
          ),
        ),
        h('td', h('div', r.client ?? t('rt.unknown_client')), h('div', kindChip(r.kind))),
        h('td', { class: 'num al-right' }, fmtInt(r.traces)),
        h('td', { class: 'num al-right nowrap', title: t('rt.sessions.calls', { llm: fmtInt(r.llm_calls), tools: fmtInt(r.tool_calls) }) }, `${fmtInt(r.llm_calls)} · ${fmtInt(r.tool_calls)}`),
        h(
          'td',
          { class: 'nowrap' },
          h('div', { class: 'num small tok-text' }, tripletShort(r.input_tokens, r.output_tokens, r.cached_tokens)),
          tokenBar(r.input_tokens, r.output_tokens, r.cached_tokens),
        ),
        h('td', { class: 'num al-right' }, r.errors ? h('span', { class: 'bad-text' }, `✕ ${fmtInt(r.errors)}`) : '0'),
        h('td', modelChips(r.models)),
        h('td', { class: 'num nowrap' }, fmtStamp(r.first_ts)),
        h('td', { class: 'num nowrap' }, fmtStamp(r.last_ts)),
        h('td', { class: 'num al-right nowrap' }, fmtDur(r.span_ms)),
      );
      tr.addEventListener('click', (e) => !e.target.closest('a,button') && nav.go({ session: r.session_key }));
      return tr;
    });
    const p = params(nav);
    swap(
      tableHost,
      h('div', { class: 'tablewrap' }, h('table', { class: 'compact trace-table', 'aria-label': t('rt.sessions.table') }, h('thead', head), h('tbody', rows))),
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
    load: (signal) => scope.api.sessions(params(nav), { signal }),
    ms: 12000,
    enabled: () => !gate,
    onData: (data, { changed }) => {
      if (gate) return;
      swap(noteHost);
      if (changed || !tableHost.querySelector('table,.state')) render(data);
    },
    onError: (e, { quiet }) => {
      if (quiet && tableHost.querySelector('table')) swap(noteHost, h('p', { class: 'note warn', role: 'status' }, t('rt.refresh_failed', { why: describeError(e) })));
      else if (e?.status === 422)
        swap(
          tableHost,
          refusedBox(e, () => reset.click()),
        );
      else swap(tableHost, errorBox(e, refreshAll));
    },
  });
  live.refresh();
  loadFacets();
  controls = h(
    'div',
    { class: 'stack-v' },
    h(
      'div',
      { class: 'rt-toolbar' },
      rangeControls(nav, () => {
        loadFacets();
        refreshAll();
      }),
    ),
    h('div', { class: 'rt-filters' }, fClient, fKind, search, h('div', { class: 'field filter end' }, reset)),
  );
  return h('div', { class: 'stack-v' }, strip.el, controls, noteHost, tableHost);
}

function sessionDetail(scope, nav) {
  const key = nav.get('session');
  const mkBack = () => navLink(nav, { session: null }, { class: 'btn sm' }, `← ${t('rt.sessions.back')}`);
  const body = h('div', { class: 'stack-v' }, skeleton(4));
  let mode = nav.get('ctx') === 'index' ? 'index' : 'time';
  let data = null;
  const chartHost = h('div', { class: 'chart-wrap-lg' });
  const laneHost = h('div', { class: 'chart-wrap-lg' });
  let ctxScope = null,
    laneScope = null;
  const openCall = (c) => nav.go({ trace: c.trace_id, span: c.span_id, view: null, session: null }, 'traces');
  const openTrace = (tr) => nav.go({ trace: tr.trace_id, span: null, view: null, session: null }, 'traces');

  function drawCtx() {
    const calls = (data.calls || []).filter((c) => c.ts != null);
    if (!calls.length) return swap(chartHost, chartEmpty(t('rt.ctx.empty')));
    ctxScope?.dispose();
    ctxScope = scope.child();
    swap(
      chartHost,
      fitted(ctxScope, (w) => contextChart(calls, { mode, onOpen: openCall, maxInput: data.session.max_input_tokens, width: w }), chartEmpty(t('rt.ctx.empty')), 760),
    );
  }
  function render() {
    const s = data.session;
    const calls = data.calls || [];
    const first = calls.find((c) => c.input_tokens != null),
      last = [...calls].reverse().find((c) => c.input_tokens != null);
    const hit = (c) => (c?.input_tokens ? (c.cached_tokens ?? 0) / c.input_tokens : null);
    const modeSw = h('div', { class: 'seg-control', role: 'group', 'aria-label': t('rt.ctx.mode') });
    const drawMode = () =>
      swap(
        modeSw,
        ...['time', 'index'].map((v) => {
          const b = h('button', { type: 'button', 'aria-pressed': String(mode === v) }, t(`rt.ctx.mode_${v}`));
          b.addEventListener('click', () => {
            mode = v;
            nav.set({ ctx: v === 'time' ? null : v });
            drawMode();
            drawCtx();
          });
          return b;
        }),
      );
    drawMode();
    const traces = data.traces || [];
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
              h('h2', { class: 'wrap-anywhere' }, h('code', s.session_key)),
              copyBtn(s.session_key, t('rt.sessions.copy_key')),
              badge(s.client ?? t('rt.unknown_client'), 'muted'),
              kindChip(s.kind),
            ),
            h('div'),
          ),
          h(
            'div',
            { class: 'trace-facts' },
            kvList([
              [t('rt.sessions.first'), `${fmtStamp(s.first_ts)} UTC`],
              [t('rt.sessions.last'), `${fmtStamp(s.last_ts)} UTC`],
              [t('rt.col.span'), fmtDur(s.span_ms)],
              [t('rt.col.models'), modelChips(s.models, 8)],
            ]),
            kvList([
              [t('rt.col.traces'), fmtInt(s.traces)],
              [t('rt.sessions.llm_calls'), fmtInt(s.llm_calls)],
              [t('rt.sessions.tool_calls'), fmtInt(s.tool_calls)],
              [t('rt.col.errors'), s.errors ? fmtInt(s.errors) : '0'],
            ]),
            kvList([
              [t('rt.sessions.max_input'), fmtTok(s.max_input_tokens)],
              [t('rt.sessions.context_growth'), first && last && first !== last ? t('rt.sessions.growth', { from: fmtTok(first.input_tokens), to: fmtTok(last.input_tokens) }) : null],
              [t('rt.sessions.cache_hit'), first && last && hit(first) != null ? t('rt.sessions.hit_range', { from: fmtPercent(hit(first)), to: fmtPercent(hit(last)) }) : null],
              [t('rt.col.tokens'), t('rt.tokens.triplet', { input: fmtTok(s.input_tokens), output: fmtTok(s.output_tokens), cached: fmtTok(s.cached_tokens) })],
            ]),
          ),
        ),
      ),
      card(t('rt.lane.title'), [
        h('p', { class: 'hint' }, t('rt.lane.hint')),
        traces.length ? laneHost : chartEmpty(),
        legendList([
          { cls: 'lane-ok', label: t('rt.status.ok') },
          { cls: 'lane-error', label: t('rt.status.error') },
          { cls: 'lane-open', label: t('rt.status.open') },
        ]),
      ]),
      card(t('rt.ctx.title'), [
        h('div', { class: 'row gap wrap between' }, h('p', { class: 'hint grow' }, t('rt.ctx.hint')), modeSw),
        chartHost,
        legendList([
          { cls: 'ctx-sw-cached', label: t('rt.ctx.lg_cached') },
          { cls: 'ctx-sw-in', label: t('rt.ctx.lg_in') },
          { cls: 'ctx-sw-out', label: t('rt.ctx.lg_out') },
          { cls: 'ctx-sw-max ln-dash', label: t('rt.ctx.lg_max'), line: 'dash' },
        ]),
        dataTable(t('rt.ctx.table'), [t('rt.ctx.c_no'), t('rt.ctx.c_when'), t('rt.f.model'), t('rt.sd.tok_in'), t('rt.sd.tok_cached'), t('rt.sd.tok_out'), t('rt.col.status')], () =>
          calls.slice(0, 300).map((c, i) => [i + 1, fmtStamp(c.ts), c.model, fmtInt(c.input_tokens), fmtInt(c.cached_tokens), fmtInt(c.output_tokens), c.status]),
        ),
      ]),
      card(t('rt.sessions.traces_title', { n: fmtInt(traces.length) }), [
        h(
          'div',
          { class: 'tablewrap tall' },
          h(
            'table',
            { class: 'compact', 'aria-label': t('rt.sessions.traces_table') },
            h(
              'thead',
              h(
                'tr',
                ['rt.col.start', 'rt.col.root', 'rt.col.duration', 'rt.col.tokens', 'rt.col.llm', 'rt.col.status'].map((k, i) => colTh(k, i === 4 ? 'al-right' : '')),
              ),
            ),
            h(
              'tbody',
              traces.map((tr) =>
                h(
                  'tr',
                  h('td', { class: 'num nowrap' }, fmtStamp(tr.start_ts)),
                  h(
                    'td',
                    navLink(nav, { trace: tr.trace_id, session: null, span: null, view: null }, { class: 'row-link' }, tr.root_name ?? '–', 'traces'),
                    ' ',
                    h('code', { class: 'muted small' }, tr.trace_id.slice(0, 8)),
                  ),
                  h('td', { class: 'num' }, tr.status === 'open' ? t('rt.dur_open') : fmtDur(tr.duration_ms)),
                  h('td', { class: 'num small' }, tripletShort(tr.input_tokens, tr.output_tokens, tr.cached_tokens)),
                  h('td', { class: 'num al-right' }, fmtInt(tr.llm_calls)),
                  h('td', statusChip(tr.status)),
                ),
              ),
            ),
          ),
        ),
      ]),
    );
    if (traces.length) {
      laneScope?.dispose();
      laneScope = scope.child();
      swap(
        laneHost,
        fitted(laneScope, (w) => swimLane(traces, { onOpen: openTrace, width: w }), chartEmpty(), 900),
      );
    }
    drawCtx();
  }
  const live = liveLoad(scope, {
    load: (signal) => scope.api.session(key, { signal }),
    ms: 15000,
    onData: (d, { changed }) => {
      if (!data || changed) {
        data = d;
        render();
      }
    },
    onError: (e, { quiet }) => {
      if (data && quiet) return;
      if (e?.status === 404 || e?.status === 422)
        swap(body, emptyBox(e.status === 404 ? t('rt.sessions.not_found') : t('rt.sessions.bad_key'), e.status === 404 ? t('rt.sessions.not_found_hint') : describeError(e), mkBack()));
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
