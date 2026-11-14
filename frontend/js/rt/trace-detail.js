// One trace: a summary header, three synchronised views of the same span tree (waterfall, icicle, span table) and a
// details panel for the selected span. Selection, view and span live in the URL (`&trace=&span=&view=`).
import { h } from '../dom.js';
import { t } from '../i18n.js';
import { fmtInt } from '../format.js';
import { describeError } from '../errors.js';
import { btn, badge, card, errorBox, skeleton, emptyBox } from '../components/ui.js';
import { buildTree, timeline, marksByParent, spanStats } from '../viz/tree.js';
import { fmtStamp, fmtDur, fmtTok, fmtPercent, shortId } from '../viz/fmt.js';
import { navLink } from './nav.js';
import { liveLoad } from './live.js';
import { statusChip, kindChip, modelChips, tokenBar, kvList, copyBtn, dash, swap } from './ui.js';
import { createWaterfall } from './waterfall.js';
import { createIcicle } from './icicle.js';
import { createSpanTable } from './span-table.js';
import { createDetails } from './span-details.js';
import { segPicker } from './controls.js';

const VIEWS = ['waterfall', 'icicle', 'table'];

export function buildModel(data) {
  const tree = buildTree(data.spans);
  const tl = timeline(data.trace, data.spans, data.marks);
  return { trace: data.trace, spans: data.spans, marks: data.marks, truncated: !!data.truncated, tree, tl, marksMap: marksByParent(data.marks, tree), stats: spanStats(data.spans) };
}

function routingFlow(r) {
  if (!r) return null;
  const step = (label, v) => h('div', { class: 'rflow-step' }, h('span', { class: 'rflow-l muted small' }, label), h('strong', { class: 'wrap-anywhere' }, v ?? '–'));
  const arrow = () => h('span', { class: 'rflow-arrow', 'aria-hidden': 'true' }, '→');
  return h(
    'div',
    { class: 'rflow', role: 'group', 'aria-label': t('rt.routing.flow_alt', { route: r.route ?? '–', decision: r.decision ?? '–', model: r.served_model ?? '–' }) },
    step(t('rt.routing.route'), r.route),
    arrow(),
    step(t('rt.routing.decision'), r.decision),
    arrow(),
    step(t('rt.routing.served'), r.served_model),
    h(
      'div',
      { class: 'rflow-meta muted small' },
      r.algorithm ? h('span', t('rt.routing.algorithm', { name: r.algorithm })) : null,
      h('span', t('rt.routing.overhead', { ms: fmtDur(r.overhead_ms) })),
      r.errors ? badge(t('rt.routing.errors', { count: r.errors }), 'warn') : null,
    ),
  );
}

function summaryCard(m, nav, back) {
  const tr = m.trace;
  const links = [
    tr.session_key ? navLink(nav, { session: tr.session_key, trace: null, span: null, view: null }, { class: 'btn sm' }, t('rt.trace.open_session'), 'sessions') : null,
    tr.has_trajectory ? navLink(nav, { trace: tr.trace_id, id: null, session: null, span: null, view: null }, { class: 'btn sm' }, t('rt.trace.open_trajectory'), 'trajectories') : null,
  ].filter(Boolean);
  return h(
    'section',
    { class: 'card trace-summary', 'aria-label': t('rt.trace.summary') },
    h(
      'div',
      { class: 'card-body' },
      h(
        'div',
        { class: 'row gap wrap between' },
        h(
          'div',
          { class: 'row gap wrap' },
          back,
          h('h2', { class: 'wrap-anywhere' }, tr.root_name ?? t('rt.trace.untitled')),
          statusChip(tr.status),
          kindChip(tr.kind),
          tr.client ? badge(tr.client, 'muted') : null,
        ),
        h('div', { class: 'row gap wrap' }, ...links),
      ),
      tr.error
        ? h(
            'div',
            { class: 'note bad', role: 'alert' },
            h('strong', t('rt.trace.failed')),
            ' ',
            h('code', tr.error.type ?? '–'),
            tr.error.phase ? ` · ${t('rt.error_phase', { phase: tr.error.phase })}` : '',
          )
        : null,
      routingFlow(tr.routing),
      h(
        'div',
        { class: 'trace-facts' },
        kvList([
          [t('rt.trace.id'), h('span', { class: 'row gap nowrap' }, h('code', { class: 'small', title: tr.trace_id }, shortId(tr.trace_id, 8, 6)), copyBtn(tr.trace_id, t('rt.sd.copy_id')))],
          [t('rt.trace.start'), `${fmtStamp(tr.start_ts)} UTC`],
          [t('rt.trace.duration'), tr.status === 'open' ? t('rt.dur_open') : fmtDur(tr.duration_ms)],
          [t('rt.trace.ttfc'), tr.ttfc_ms != null ? fmtDur(tr.ttfc_ms) : null],
          [t('rt.trace.models'), tr.models?.length ? modelChips(tr.models, 8) : null],
        ]),
        kvList([
          [t('rt.trace.spans'), t('rt.trace.span_counts', { spans: fmtInt(tr.spans), llm: fmtInt(tr.llm_calls), tools: fmtInt(tr.tool_calls) })],
          [t('rt.trace.tool_errors'), tr.tool_errors ? fmtInt(tr.tool_errors) : null],
          [t('rt.trace.subagents'), tr.subagents ? fmtInt(tr.subagents) : null],
          [t('rt.trace.marks'), fmtInt(tr.marks)],
          [t('rt.trace.max_input'), tr.max_input_tokens != null ? fmtTok(tr.max_input_tokens) : null],
          [t('rt.trace.cache_hit'), tr.input_tokens ? fmtPercent((tr.cached_tokens ?? 0) / tr.input_tokens) : null],
        ]),
        h(
          'div',
          { class: 'trace-tokens' },
          h('span', { class: 'kv-k muted' }, t('rt.trace.tokens')),
          tr.input_tokens != null || tr.output_tokens != null
            ? [
                tokenBar(tr.input_tokens, tr.output_tokens, tr.cached_tokens),
                h(
                  'div',
                  { class: 'num small' },
                  t('rt.tokens.full', { input: fmtTok(tr.input_tokens), cached: fmtTok(tr.cached_tokens), output: fmtTok(tr.output_tokens), reasoning: fmtTok(tr.reasoning_tokens) }),
                ),
                h(
                  'div',
                  { class: 'tok-legend small muted' },
                  h('i', { class: 'tk-in sw' }),
                  t('rt.tokens.lg_in'),
                  h('i', { class: 'tk-cached sw' }),
                  t('rt.tokens.lg_cached'),
                  h('i', { class: 'tk-out sw' }),
                  t('rt.tokens.lg_out'),
                ),
              ]
            : dash(),
        ),
      ),
    ),
  );
}

export function traceDetail(scope, nav, ctx) {
  const id = nav.get('trace');
  const root = h('div', { class: 'stack-v trace-detail' });
  const mkBack = () => navLink(nav, { trace: null, span: null, view: null }, { class: 'btn sm' }, `← ${t('rt.trace.back')}`);
  const body = h('div', { class: 'stack-v' }, skeleton(4));
  root.append(body);
  let m = null,
    view = VIEWS.includes(nav.get('view')) ? nav.get('view') : 'waterfall',
    selected = nav.get('span') || null,
    views = {},
    details = null,
    gridEl = null,
    viewHost = null,
    summaryHost = null,
    pickerEl = null,
    wide = false,
    truncNote = null;

  const spanOf = (sid) => (sid ? m.tree.nodes.get(sid)?.span : null);
  function select(sid, opts = {}) {
    selected = sid;
    nav.set({ span: sid }, { push: false });
    for (const v of Object.values(views)) v.setSelected?.(sid, { reveal: opts.reveal });
    const s = spanOf(sid);
    details.show(s);
    gridEl.classList.toggle('has-panel', !!s);
  }
  function build() {
    const vm = m;
    summaryHost = h('div');
    views = {
      waterfall: createWaterfall({ model: vm, selected, scope, onSelect: (sid, o) => select(sid, o) }),
      icicle: createIcicle({ model: vm, selected, onSelect: (sid) => select(sid, {}) }),
      table: createSpanTable({ model: vm, selected, onSelect: (sid) => select(sid, {}) }),
    };
    details = createDetails({
      scope,
      api: scope.api,
      traceId: id,
      model: () => m,
      onClose: () => select(null),
      onZoom: (sid) => {
        if (view !== 'waterfall') pick('waterfall');
        views.waterfall.zoomToSpan(sid);
      },
    });
    pickerEl = segPicker({
      values: VIEWS,
      current: () => view,
      labelOf: (v) => t(`rt.view.${v}`),
      label: t('rt.view.label'),
      onPick: (v) => pick(v),
    });
    viewHost = h('div', { class: 'tv-views' });
    gridEl = h(
      'div',
      { class: `trace-grid${selected ? ' has-panel' : ''}` },
      h('div', { class: 'tv-main' }, h('div', { class: 'row gap wrap between tv-switch' }, pickerEl, h('span', { class: 'muted small' }, t('rt.view.hint'))), viewHost),
      h('div', { class: 'tv-side' }, details.el),
    );
    truncNote = h('p', { class: 'note warn', hidden: !m.truncated, role: 'status' }, t('rt.trace.truncated'));
    swap(body, summaryHost, truncNote, gridEl);
    swap(summaryHost, summaryCard(m, nav, mkBack()));
    showView();
    if (selected && spanOf(selected)) {
      details.show(spanOf(selected));
      for (const v of Object.values(views)) v.setSelected?.(selected, { reveal: true });
    } else if (selected) {
      selected = null;
      nav.set({ span: null });
    }
  }
  function showView() {
    swap(viewHost, views[view].el);
    pickerEl.redraw();
    if (selected) views[view].setSelected?.(selected, { reveal: true });
  }
  function pick(v) {
    view = v;
    nav.set({ view: v === 'waterfall' ? null : v });
    showView();
  }

  const live = liveLoad(scope, {
    load: (signal) => scope.api.trace(id, { signal }),
    ms: 4000,
    enabled: () => !m || m.trace.status === 'open',
    onData: (data, { changed }) => {
      if (!m) {
        m = buildModel(data);
        build();
        return;
      }
      if (!changed) return;
      m = buildModel(data);
      swap(summaryHost, summaryCard(m, nav, mkBack()));
      truncNote.hidden = !m.truncated;
      for (const v of Object.values(views)) v.update(m);
      if (selected && !spanOf(selected)) select(null);
    },
    onError: (e, { quiet }) => {
      if (m && quiet) return;
      if (e?.status === 404 || e?.status === 422)
        swap(body, emptyBox(e.status === 404 ? t('rt.trace.not_found') : t('rt.trace.bad_id'), e.status === 404 ? t('rt.trace.not_found_hint') : describeError(e), mkBack()));
      else
        swap(
          body,
          errorBox(e, () => live.refresh()),
        );
    },
  });
  live.refresh();
  return root;
}
