// A chart card of one or more stacked line panels that share a time axis (addendum 9.14): power against its limit, the really-busy lines
// of a GPU, Dynamo's load (running, waiting, KV usage, preemptions each on its own scale). The caller shapes the lines (a name and
// segments, from series-model); this draws them with the same SVG as the series cards (colour AND dash pattern, a <title> on every point,
// an aria-label per chart), a legend per panel and a text alternative (a table of latest / min / avg / max per line). The card is
// updated in place, so an open data table survives a refresh.
import { h } from '../dom.js';
import { t } from '../i18n.js';
import { seriesChart, lineSwatch } from './series-chart.js';
import { chartEmpty, swap } from './parts.js';
import { lineStats } from '../series-model.js';

/**
 * @param opts.title    card title
 * @param opts.hint     optional one-line explanation under the title
 * @param opts.panels   [{id, title?, yFmt, valueFmt, height?}] one chart each, top to bottom
 * @param opts.win      () => '15m'|'1h'|'24h'|'7d' (picks the time axis style)
 * @param opts.wide     span the whole grid row
 * Returns {el, update({from, to, panels: [{lines: [{name, segments}], title?, yFmt?, valueFmt?}], note?, caption?}), fail(msg)}.
 */
export function linesCard(scope, { title, hint, panels, win, wide = false }) {
  let data = null;
  const noteHost = h('div');
  const caption = h('span', { class: 'muted small' });
  const body = h('div', { class: 'lc-panels' });
  const tableBody = h('div', { class: 'tablewrap tall' });
  const details = h('details', { class: 'chart-data' }, h('summary', t('chart.data_table')), tableBody);
  const summary = h('p', { class: 'sr-only' });
  const el = h(
    'section',
    { class: `card chart-card rt-chart lines-card${wide ? ' wide' : ''}`, 'aria-label': title },
    h('header', { class: 'card-head' }, h('h3', title), caption),
    h('div', { class: 'card-body' }, hint ? h('p', { class: 'hint' }, hint) : null, noteHost, body, summary, details),
  );

  // An update may override a panel's title and formatters (a rate shown per minute when it is small): the scale is the data's to choose.
  const drawn = () =>
    panels.map((p, i) => {
      const d = data?.panels?.[i] || {};
      const lines = (d.lines || []).map((l, k) => ({ key: `${p.id}.${k}`, name: l.name, idx: l.idx ?? k, segments: l.segments || [] }));
      return { spec: { ...p, title: d.title ?? p.title, yFmt: d.yFmt ?? p.yFmt, valueFmt: d.valueFmt ?? p.valueFmt }, lines };
    });

  function draw() {
    if (!data) return;
    const width = Math.max(260, Math.round(body.clientWidth || 560));
    const all = drawn();
    const anyData = all.some((p) => p.lines.some((l) => l.segments.some((s) => s.length)));
    const sum = [];
    const nodes = all.map(({ spec, lines }) => {
      const live = lines.filter((l) => l.segments.some((s) => s.length));
      const label = t('chart.summary', {
        title: `${spec.title || title}, ${t(`window.${win()}`)}`,
        latest: live.map((l) => `${l.name} ${spec.valueFmt(lineStats(l.segments).latest)}`).join(', ') || t('common.no_data'),
      });
      const chart = live.length
        ? seriesChart({ lines: live, win: win(), from: data.from, to: data.to, width, height: spec.height ?? (width < 420 ? 170 : 190), yFmt: spec.yFmt, valueFmt: spec.valueFmt, ariaLabel: label })
        : null;
      if (chart) sum.push(label);
      return h(
        'div',
        { class: 'lc-panel' },
        spec.title || live.length
          ? h(
              'div',
              { class: 'lc-head' },
              spec.title ? h('h4', spec.title) : null,
              live.length
                ? h(
                    'ul',
                    { class: 'legend lc-legend' },
                    live.map((l) => h('li', lineSwatch(l.idx), h('span', l.name))),
                  )
                : null,
            )
          : null,
        chart ?? chartEmpty(t('series.empty')),
      );
    });
    body.replaceChildren(...(anyData || all.length > 1 ? nodes : [chartEmpty(t('series.empty'))]));
    summary.textContent = sum.join(' ');
    if (details.open) fillTable();
  }

  function fillTable() {
    const head = [t('chart.series'), t('series.latest'), t('series.min'), t('series.avg'), t('series.max')];
    const rows = drawn().flatMap(({ spec, lines }) =>
      lines
        .filter((l) => l.segments.some((s) => s.length))
        .map((l) => {
          const s = lineStats(l.segments),
            f = spec.valueFmt;
          return [`${spec.title ? `${spec.title}: ` : ''}${l.name}`, f(s.latest), f(s.min), f(s.avg), f(s.max)];
        }),
    );
    tableBody.replaceChildren(
      h(
        'table',
        { class: 'compact', 'aria-label': t('chart.data_table') },
        h(
          'thead',
          h(
            'tr',
            head.map((c, i) => h('th', { scope: 'col', class: i ? 'al-right' : '' }, c)),
          ),
        ),
        h(
          'tbody',
          rows.map((r) =>
            h(
              'tr',
              r.map((c, i) => h('td', { class: i ? 'num al-right' : '' }, c)),
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
      const w = Math.round(body.clientWidth);
      if (!data || w < 80 || Math.abs(w - lastW) < 6) return;
      lastW = w;
      draw();
    });
    ro.observe(body);
    scope.own(() => ro.disconnect());
  }
  return {
    el,
    update(d) {
      data = d;
      caption.textContent = d?.caption ?? '';
      swap(noteHost, d?.note ? h('p', { class: 'note info', role: 'status' }, d.note) : null);
      draw();
    },
    fail(msg) {
      swap(noteHost, h('p', { class: 'note warn', role: 'status' }, msg));
      if (!data) body.replaceChildren();
    },
  };
}
