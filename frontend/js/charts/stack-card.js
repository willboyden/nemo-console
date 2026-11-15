// A card of stacked bars (addendum 9.14): a bar per row for NOW (or for a window mean: Dynamo's latency breakdown, a GPU's memory) and,
// optionally, the same parts stacked over time. Every segment has an aria-label; the legend names the parts; a collapsed table is the
// text alternative. Updated in place, so an open table survives a refresh.
import { h } from '../dom.js';
import { t } from '../i18n.js';
import { stackedRows } from './stacked-rows.js';
import { stackedChart } from './svg-charts.js';
import { legendList, chartEmpty } from './parts.js';
import { finite } from '../series-math.js';

/**
 * @param opts.parts     [{key, name, cls (bf1..bf6)}] the stack order, bottom / left first
 * @param opts.valueFmt  (v) => text for one value;  opts.axisFmt (v) => short y tick text of the time charts;  opts.labelMax the widest row label column in px (default 120; a longer label is cut)
 * Returns {el, update({rows: [{id, label, values: {key: number|null}, totalText?, note?}], time: [{label, ts, bucketS, series: [{key, values}]}]}), fail(msg)}.
 */
export function stackCard(scope, { title, hint, parts, valueFmt, axisFmt, wide = false, rowsTitle, timeTitle, emptyText, labelMax = 120 }) {
  let data = null;
  const noteHost = h('div');
  const rowsHost = h('div', { class: 'chart-wrap-lg' });
  const timeHost = h('div', { class: 'sc-time' });
  const tableBody = h('div', { class: 'tablewrap tall' });
  const details = h('details', { class: 'chart-data' }, h('summary', t('chart.data_table')), tableBody);
  const summary = h('p', { class: 'sr-only' });
  const el = h(
    'section',
    { class: `card chart-card rt-chart stack-card${wide ? ' wide' : ''}`, 'aria-label': title },
    h('header', { class: 'card-head' }, h('h3', title)),
    h(
      'div',
      { class: 'card-body' },
      hint ? h('p', { class: 'hint' }, hint) : null,
      noteHost,
      rowsTitle ? h('h4', { class: 'sc-sub' }, rowsTitle) : null,
      rowsHost,
      legendList(parts.map((p) => ({ cls: p.cls, label: p.name }))),
      timeTitle ? h('h4', { class: 'sc-sub' }, timeTitle) : null,
      timeHost,
      summary,
      details,
    ),
  );
  const nameOf = (key) => parts.find((p) => p.key === key)?.name ?? key;

  function draw() {
    if (!data) return;
    const width = Math.max(260, Math.round(rowsHost.clientWidth || 560));
    const rows = (data.rows || []).map((r) => ({ id: r.id, label: r.label, totalText: r.totalText, parts: parts.map((p) => ({ ...p, value: r.values?.[p.key] ?? null })) }));
    const label = t('chart.summary', {
      title,
      latest: rows.map((r) => `${r.label} ${parts.map((p) => `${p.name} ${r.values?.[p.key] == null ? '–' : valueFmt(r.values[p.key])}`).join(', ')}`).join('; ') || t('common.no_data'),
    });
    const chart = rows.length
      ? stackedRows({ rows, width, ariaLabel: label, valueFmt, labelW: Math.min(labelMax, Math.round(width * 0.42), Math.max(48, Math.max(...rows.map((r) => r.label.length)) * 6.4 + 12)) })
      : null;
    rowsHost.replaceChildren(chart ?? chartEmpty(emptyText ?? t('series.empty')));
    summary.textContent = chart ? label : '';
    const charts = (data.time || []).map((tc) => {
      const series = tc.series.map((s) => ({ key: s.key, label: nameOf(s.key), cls: parts.find((p) => p.key === s.key)?.cls ?? 'bf1', values: s.values }));
      const aria = t('chart.summary', { title: `${title}${tc.label ? `, ${tc.label}` : ''}`, latest: series.map((s) => `${s.label} ${valueFmt(lastValue(s.values))}`).join(', ') });
      const node = stackedChart({ ts: tc.ts, bucketS: tc.bucketS, series, width, height: width < 420 ? 170 : 190, ariaLabel: aria, yFmt: axisFmt, valueFmt });
      return h('div', { class: 'sc-chart' }, tc.label ? h('h4', { class: 'sc-sub' }, tc.label) : null, node ?? chartEmpty(t('series.empty')));
    });
    timeHost.replaceChildren(...charts);
    if (details.open) fillTable();
  }

  function fillTable() {
    const head = [t('chart.series'), ...parts.map((p) => p.name)];
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
          (data?.rows || []).map((r) =>
            h(
              'tr',
              h('td', r.label),
              parts.map((p) => h('td', { class: 'num al-right' }, r.values?.[p.key] == null ? '–' : valueFmt(r.values[p.key]))),
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
      const w = Math.round(rowsHost.clientWidth);
      if (!data || w < 80 || Math.abs(w - lastW) < 6) return;
      lastW = w;
      draw();
    });
    ro.observe(rowsHost);
    scope.own(() => ro.disconnect());
  }
  return {
    el,
    update(d) {
      data = d;
      const notes = (d?.rows || []).filter((r) => r.note).map((r) => r.note);
      noteHost.replaceChildren(...notes.map((n) => h('p', { class: 'note info', role: 'status' }, n)));
      draw();
    },
    fail(msg) {
      noteHost.replaceChildren(h('p', { class: 'note warn', role: 'status' }, msg));
      if (!data) {
        rowsHost.replaceChildren();
        timeHost.replaceChildren();
      }
    },
  };
}

const lastValue = (values) => {
  for (let i = values.length - 1; i >= 0; i--) if (finite(values[i])) return values[i];
  return null;
};
