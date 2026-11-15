// The XID and diagnostics timeline card of the GPU telemetry page (addendum 9.14): a swim lane per GPU from the `dcgm.xid` and
// `dcgm.diag` events, click a mark for the details (an XID's code, a diagnostic's verdict and tests), and the same events as a table.
// Every value from an event is rendered with textContent (through h()). Nothing here reads a GPU serial: the events carry none (E-8).
import { h } from '../dom.js';
import { t } from '../i18n.js';
import { fmtMs } from '../format.js';
import { fmtStamp } from '../viz/fmt.js';
import { badge, emptyBox } from '../components/ui.js';
import { chartEmpty, swap, windowPhrase } from '../charts/parts.js';
import { timelineChart } from '../charts/timeline-chart.js';
import { timelineModel, diagTests } from '../dcgm-model.js';

const KIND = { ok: 'ok', warn: 'warn', bad: 'bad', muted: 'muted' };
const TEST_KIND = { pass: 'ok', warn: 'warn', fail: 'bad', skip: 'muted' };

/** The details of one mark: what the event says, as a definition list (text only). */
export function eventDetails(it) {
  const d = it.data || {};
  const kv = (k, v) => (v == null || v === '' ? null : h('div', { class: 'kv' }, h('dt', k), h('dd', String(v))));
  const rows = [
    kv(t('gpu.tl.f_time'), `${fmtStamp(it.ts)} UTC`),
    kv(t('gpu.tl.f_gpu'), it.gpu === '?' ? null : it.gpu),
    kv(t('gpu.tl.f_what'), it.kind === 'xid' ? t('gpu.tl.xid') : t('gpu.tl.diag')),
    kv(t('gpu.tl.f_summary'), it.summary),
  ];
  if (it.kind === 'xid') {
    rows.push(kv(t('gpu.tl.f_xid'), d.xid), kv(t('gpu.tl.f_new'), d.new), kv(t('gpu.tl.f_total'), d.xid_total), kv(t('gpu.tl.f_source'), d.source));
  } else {
    rows.push(
      kv(t('gpu.tl.f_level'), d.level),
      kv(t('gpu.tl.f_verdict'), d.verdict),
      kv(t('gpu.tl.f_duration'), typeof d.duration_ms === 'number' ? fmtMs(d.duration_ms) : null),
      kv(t('gpu.tl.f_model'), d.model),
      kv(t('gpu.tl.f_run'), d.run_id),
    );
  }
  const tests = it.kind === 'diag' ? diagTests(d) : [];
  return h(
    'div',
    { class: 'stack-v tl-details', role: 'region', 'aria-label': t('gpu.tl.details') },
    h('dl', { class: 'facts' }, rows),
    tests.length
      ? h(
          'div',
          { class: 'tablewrap' },
          h(
            'table',
            { class: 'compact', 'aria-label': t('gpu.tl.tests') },
            h('thead', h('tr', h('th', { scope: 'col' }, t('gpu.tl.test')), h('th', { scope: 'col' }, t('gpu.tl.result')))),
            h(
              'tbody',
              tests.map((x) => h('tr', h('td', x.name), h('td', badge(x.status, TEST_KIND[x.status.toLowerCase()] ?? 'muted')))),
            ),
          ),
        )
      : null,
  );
}

/**
 * @param opts.win () => the current window id; the card shows the events inside it.
 * Returns {el, update(events, {from, to}), fail(msg)}.
 */
export function timelineCard(scope, { win }) {
  let model = null,
    range = null,
    selected = null,
    items = new Map();
  const chartHost = h('div', { class: 'chart-wrap-lg' });
  const detailHost = h('div', { 'aria-live': 'polite' });
  const noteHost = h('div');
  const tableBody = h('div', { class: 'tablewrap tall' });
  const details = h('details', { class: 'chart-data' }, h('summary', t('chart.data_table')), tableBody);
  const legend = h(
    'ul',
    { class: 'legend' },
    h('li', h('i', { class: 'tl-key diamond bad', 'aria-hidden': 'true' }), h('span', t('gpu.tl.key_xid'))),
    h('li', h('i', { class: 'tl-key circle ok', 'aria-hidden': 'true' }), h('span', t('gpu.tl.key_pass'))),
    h('li', h('i', { class: 'tl-key circle warn', 'aria-hidden': 'true' }), h('span', t('gpu.tl.key_warn'))),
    h('li', h('i', { class: 'tl-key circle bad', 'aria-hidden': 'true' }), h('span', t('gpu.tl.key_fail'))),
  );
  const el = h(
    'section',
    { class: 'card chart-card rt-chart wide', 'aria-label': t('gpu.tl.title') },
    h('header', { class: 'card-head' }, h('h3', t('gpu.tl.title'))),
    h('div', { class: 'card-body' }, h('p', { class: 'hint' }, t('gpu.tl.hint')), noteHost, chartHost, legend, detailHost, details),
  );

  function pick(it) {
    selected = selected === it.id ? null : it.id;
    swap(detailHost, selected ? eventDetails(it) : '');
    draw();
  }

  function draw() {
    if (!model || !range) return;
    const width = Math.max(300, Math.round(chartHost.clientWidth || 640));
    swap(noteHost, model.outside ? h('p', { class: 'muted small' }, t('gpu.tl.outside', { count: model.outside })) : null);
    if (!model.items.length) {
      chartHost.replaceChildren(emptyBox(t('gpu.tl.empty'), t('gpu.tl.empty_hint', { window: windowPhrase(win()) })));
      legend.hidden = true;
      swap(detailHost);
      return;
    }
    legend.hidden = false;
    const aria = t('gpu.tl.alt', { count: model.items.length, lanes: model.lanes.length, window: windowPhrase(win()) });
    chartHost.replaceChildren(timelineChart(model, { from: range.from, to: range.to, width, selected, onPick: pick, ariaLabel: aria }) ?? chartEmpty());
    if (details.open) fillTable();
  }

  function fillTable() {
    const rows = [...model.items].reverse();
    tableBody.replaceChildren(
      h(
        'table',
        { class: 'compact', 'aria-label': t('gpu.tl.title') },
        h(
          'thead',
          h(
            'tr',
            h('th', { scope: 'col' }, t('gpu.tl.f_time')),
            h('th', { scope: 'col' }, t('gpu.tl.f_gpu')),
            h('th', { scope: 'col' }, t('gpu.tl.f_what')),
            h('th', { scope: 'col' }, t('gpu.tl.f_summary')),
          ),
        ),
        h(
          'tbody',
          rows.map((r) =>
            h(
              'tr',
              h('td', { class: 'num' }, `${fmtStamp(r.ts)}`),
              h('td', r.gpu),
              h('td', badge(r.kind === 'xid' ? t('gpu.tl.xid') : t('gpu.tl.diag'), KIND[r.severity])),
              h('td', { class: 'wrap-anywhere' }, r.summary),
            ),
          ),
        ),
      ),
    );
  }
  details.addEventListener('toggle', () => details.open && fillTable());

  let lastW = 0;
  if (typeof ResizeObserver !== 'undefined') {
    const ro = new ResizeObserver(() => {
      const w = Math.round(chartHost.clientWidth);
      if (!model || w < 80 || Math.abs(w - lastW) < 6) return;
      lastW = w;
      draw();
    });
    ro.observe(chartHost);
    scope.own(() => ro.disconnect());
  }
  return {
    el,
    update(events, r) {
      range = r;
      model = timelineModel(events, r);
      items = new Map(model.items.map((i) => [i.id, i]));
      // the selection survives a refresh while its event is still in the window (ids carry time, kind and GPU)
      if (selected && !items.has(selected)) {
        selected = null;
        swap(detailHost);
      }
      draw();
    },
    fail(msg) {
      swap(noteHost, h('p', { class: 'note warn', role: 'status' }, msg));
    },
  };
}
