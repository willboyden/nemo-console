// Line chart card with legend, latest-value caption and a text alternative (collapsible data table).
import { h, setTrustedHtml } from '../dom.js';
import { t } from '../i18n.js';
import { lineChart, legend } from '../charts.js';
import { relativeMinutes } from '../metrics.js';

/**
 * series: [{name, points:[[tsSeconds, value],...]}]. x axis is minutes relative to the newest point.
 * Returns a <section class="card">. `empty` (string) is shown instead of the chart when no series has points.
 */
export function lineChartCard({ title, series, yFormat = String, ariaLabel, empty = t('common.no_data'), valueFmt = yFormat }) {
  const live = series.filter((s) => s.points?.length);
  const body = h('div', { class: 'card-body' });
  if (!live.length) { body.append(h('p', { class: 'hint' }, empty)); return h('section', { class: 'card chart-card' }, h('header', { class: 'card-head' }, h('h3', title)), body); }
  const newest = Math.max(...live.map((s) => s.points[s.points.length - 1][0]));
  const rel = live.map((s) => ({ name: s.name, points: relativeMinutes(s.points, newest) }));
  const summary = t('chart.summary', { title, latest: live.map((s) => `${s.name} ${valueFmt(s.points[s.points.length - 1][1])}`).join(', ') });
  const wrap = h('div', { class: 'chart-wrap' });
  setTrustedHtml(wrap, lineChart(rel, { yFormat, xFormat: (m) => (Math.abs(m) < 0.5 ? t('chart.now') : `${Math.round(m)}m`), ariaLabel: ariaLabel || summary, height: 190, width: 600 }));
  const rows = live.flatMap((s) => {
    const pts = s.points, step = Math.max(1, Math.ceil(pts.length / 8));
    return pts.filter((_, i) => i % step === 0 || i === pts.length - 1).map(([ts, v]) => h('tr', h('td', s.name), h('td', { class: 'num' }, new Date(ts * 1000).toISOString().slice(11, 19)), h('td', { class: 'num al-right' }, valueFmt(v))));
  });
  const legendEl = h('div'); setTrustedHtml(legendEl, legend(rel));
  body.append(wrap, legendEl, h('p', { class: 'sr-only' }, summary),
    h('details', { class: 'chart-data' }, h('summary', t('chart.data_table')),
      h('div', { class: 'tablewrap' }, h('table', { class: 'compact' }, h('thead', h('tr', h('th', { scope: 'col' }, t('chart.series')), h('th', { scope: 'col' }, t('chart.time')), h('th', { scope: 'col', class: 'al-right' }, t('chart.value')))), h('tbody', rows)))));
  return h('section', { class: 'card chart-card' }, h('header', { class: 'card-head' }, h('h3', title)), body);
}
