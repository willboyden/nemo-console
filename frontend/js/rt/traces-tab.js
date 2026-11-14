// Traces tab: the index strip, window/anchor and filters, a sortable paginated table of trace summaries, and (with
// `&trace=<id>`) the trace detail (trace-detail.js). Filters live in the URL, so a view can be shared and Back works.
import { h } from '../dom.js';
import { t } from '../i18n.js';
import { fmtInt } from '../format.js';
import { describeError } from '../errors.js';
import { emptyBox, errorBox, skeleton, btn } from '../components/ui.js';
import { fmtStamp, fmtDur } from '../viz/fmt.js';
import { createNav, navLink } from './nav.js';
import { liveLoad } from './live.js';
import { indexStrip, indexGate } from './index-strip.js';
import { rangeControls, facetSelect, searchBox, numberBox, resetButton } from './controls.js';
import { tripletShort, refusedBox, statusChip, kindChip, routingChip, modelChips, tokenBar, durBar, sortTh, pager, swap } from './ui.js';
import { traceDetail } from './trace-detail.js';

const FILTERS = ['client', 'kind', 'model', 'status', 'q', 'min_ms', 'session'];
const SORTS = ['start', 'duration', 'tokens', 'llm_calls'];

export function tracesTab(scope, ctx) {
  const nav = createNav('traces', ctx);
  const host = h('div', { class: 'stack-v rt-tab' });
  let sub = null,
    mode = null;
  const apply = () => {
    const key = nav.get('trace') ? `d:${nav.get('trace')}` : 'list';
    if (key === mode) return;
    mode = key;
    sub?.dispose();
    sub = scope.child();
    swap(host, nav.get('trace') ? traceDetail(sub, nav, ctx) : traceList(sub, nav));
    if (key !== 'list') window.scrollTo?.({ top: 0 });
  };
  nav.onChange(apply);
  apply();
  return host;
}

export function listParams(nav) {
  const q = {
    window: nav.get('window'),
    anchor: nav.get('anchor'),
    sort: SORTS.includes(nav.get('sort')) ? nav.get('sort') : 'start',
    order: nav.get('order') === 'asc' ? 'asc' : 'desc',
    limit: [25, 50, 100, 200].includes(Number(nav.get('limit'))) ? Number(nav.get('limit')) : 50,
    offset: Math.max(0, parseInt(nav.get('offset'), 10) || 0),
  };
  for (const k of FILTERS) if (nav.get(k)) q[k] = nav.get(k);
  return q;
}

function traceList(scope, nav) {
  const root = h('div', { class: 'stack-v' });
  const tableHost = h('div', { class: 'tbl-host' }, skeleton(5));
  const noteHost = h('div');
  let gate = null,
    controls = null;
  const refreshAll = () => {
    live.forget();
    live.refresh();
  };
  const facetEls = {
    client: facetSelect({ name: 'client', label: t('rt.f.client'), nav, onChange: refreshAll }),
    kind: facetSelect({ name: 'kind', label: t('rt.f.kind'), nav, onChange: refreshAll }),
    model: facetSelect({ name: 'model', label: t('rt.f.model'), nav, onChange: refreshAll }),
    status: facetSelect({ name: 'status', label: t('rt.f.status'), nav, onChange: refreshAll }),
  };
  const search = searchBox({ name: 'q', label: t('rt.f.search'), nav, onChange: refreshAll, placeholder: t('rt.f.search_ph') });
  const minMs = numberBox({ name: 'min_ms', label: t('rt.f.min_ms'), nav, onChange: refreshAll, step: 100, placeholder: '0' });
  const reset = resetButton(() => {
    nav.set(Object.fromEntries([...FILTERS, 'offset'].map((k) => [k, null])));
    search.input.value = '';
    refreshAll();
    for (const el of Object.values(facetEls)) el.update(el.facet);
    minMs.querySelector('input').value = '';
  });
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
  const state = () => ({ sort: listParams(nav).sort, order: listParams(nav).order });
  const onSort = (sort, order) => {
    nav.set({ sort, order, offset: null });
    refreshAll();
  };

  function render(data) {
    for (const [name, el] of Object.entries(facetEls)) {
      el.facet = data.facets?.[name];
      el.update(el.facet);
    }
    const items = data.items || [];
    if (!items.length) {
      swap(tableHost, emptyBox(t('rt.traces.empty'), t('rt.traces.empty_hint'), btn(t('rt.filter.reset'), { size: 'sm', onClick: () => reset.click() })));
      return;
    }
    const maxDur = Math.max(1, ...items.map((i) => i.duration_ms ?? 0));
    const st = state();
    const head = h(
      'tr',
      h('th', { scope: 'col' }, t('rt.col.status')),
      sortTh(t('rt.col.start'), 'start', st, onSort),
      h('th', { scope: 'col' }, t('rt.col.client')),
      h('th', { scope: 'col' }, t('rt.col.root')),
      h('th', { scope: 'col' }, t('rt.col.models')),
      sortTh(t('rt.col.duration'), 'duration', st, onSort),
      h('th', { scope: 'col', class: 'al-right col-ttfc', title: t('rt.col.ttfc_tip') }, t('rt.col.ttfc')),
      sortTh(t('rt.col.tokens'), 'tokens', st, onSort, '', t('rt.col.tokens_sub')),
      sortTh(t('rt.col.calls'), 'llm_calls', st, onSort, 'al-right', t('rt.col.calls_sub')),
      h('th', { scope: 'col' }, t('rt.col.routing')),
    );
    const rows = items.map((r) => {
      const tr = h(
        'tr',
        { class: `trace-row st-${r.status}`, dataset: { id: r.trace_id } },
        h('td', { class: 'nowrap' }, statusChip(r.status), r.error ? h('div', { class: 'muted small err-type', title: t('rt.error_phase', { phase: r.error.phase ?? '–' }) }, r.error.type) : null),
        h('td', { class: 'num nowrap', title: new Date(r.start_ts * 1000).toISOString() }, fmtStamp(r.start_ts)),
        h('td', h('div', r.client ?? h('span', { class: 'muted' }, t('rt.unknown_client'))), h('div', kindChip(r.kind))),
        h(
          'td',
          h('div', { class: 'nowrap' }, navLink(nav, { trace: r.trace_id, span: null, view: null }, { class: 'row-link', title: t('rt.traces.open') }, r.root_name ?? '–')),
          h('code', { class: 'muted small' }, r.trace_id.slice(0, 8)),
        ),
        h('td', modelChips(r.models)),
        h(
          'td',
          h('div', { class: 'num' }, r.status === 'open' ? t('rt.dur_open') : fmtDur(r.duration_ms)),
          r.duration_ms != null ? durBar(r.duration_ms, maxDur, t('rt.col.duration_bar', { d: fmtDur(r.duration_ms) })) : null,
        ),
        h('td', { class: 'num al-right nowrap col-ttfc' }, fmtDur(r.ttfc_ms)),
        h(
          'td',
          { class: 'nowrap' },
          h('div', { class: 'num small tok-text' }, tripletShort(r.input_tokens, r.output_tokens, r.cached_tokens)),
          r.input_tokens != null || r.output_tokens != null ? tokenBar(r.input_tokens, r.output_tokens, r.cached_tokens) : null,
        ),
        h(
          'td',
          { class: 'num al-right nowrap', title: t('rt.col.calls_tip', { llm: r.llm_calls, tools: r.tool_calls }) },
          `${fmtInt(r.llm_calls)} · ${fmtInt(r.tool_calls)}`,
          r.tool_errors ? h('span', { class: 'bad-text small', title: t('rt.col.tool_errors', { n: r.tool_errors }) }, ` ✕${r.tool_errors}`) : null,
        ),
        h('td', { class: 'nowrap' }, routingChip(r.routing)),
      );
      tr.addEventListener('click', (e) => {
        if (e.target.closest('a,button')) return;
        nav.go({ trace: r.trace_id, span: null, view: null });
      });
      return tr;
    });
    const p = listParams(nav);
    swap(
      tableHost,
      h('div', { class: 'tablewrap' }, h('table', { class: 'compact trace-table', 'aria-label': t('rt.traces.table') }, h('thead', head), h('tbody', rows))),
      pager({
        offset: data.offset ?? p.offset,
        limit: data.limit ?? p.limit,
        total: data.total ?? items.length,
        onPage: (off) => {
          nav.set({ offset: off || null });
          refreshAll();
          tableHost.scrollIntoView?.({ block: 'nearest' });
        },
      }),
      h(
        'div',
        { class: 'row gap small muted' },
        h('label', { for: 'page-size' }, t('rt.pager.size')),
        (() => {
          const s = h(
            'select',
            { id: 'page-size', class: 'narrow-select' },
            [25, 50, 100, 200].map((n) => h('option', { value: n, selected: n === p.limit }, String(n))),
          );
          s.addEventListener('change', () => {
            nav.set({ limit: s.value === '50' ? null : s.value, offset: null });
            refreshAll();
          });
          return s;
        })(),
      ),
    );
  }

  const live = liveLoad(scope, {
    load: (signal) => scope.api.traces(listParams(nav), { signal }),
    ms: 10000,
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

  controls = h(
    'div',
    { class: 'stack-v' },
    h('div', { class: 'rt-toolbar' }, rangeControls(nav, refreshAll)),
    h('div', { class: 'rt-filters' }, facetEls.client, facetEls.kind, facetEls.model, facetEls.status, search, minMs, h('div', { class: 'field filter end' }, reset)),
  );
  root.append(strip.el, controls, noteHost, tableHost);
  return root;
}
