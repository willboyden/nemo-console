// A stepped stacked area over time (addendum 9.16): the OpenShell sandboxes by phase. Each part is a filled band with its own class (bf1.. colours) AND a place in the stack and a legend
// entry, so the parts are told apart by order and name as well as colour. A bucket with no sample is a gap, not a drop to zero. Every bucket has a transparent hit area whose <title> lists
// the time and every part's value (hover), and the whole chart an aria-label; the card around it offers a data table. Geometry: viz/area.js (pure, unit-tested).
import { svg, titled } from '../viz/svg.js';
import { stackedAreaLayout } from '../viz/area.js';
import { fmtStamp } from '../viz/fmt.js';
import { compactNum } from './svg-charts.js';

/**
 * @param opts.series  [{key, label, cls (bf1..), values}], stacked bottom to top
 * Returns an <svg>, or null when nothing was sampled or every part is 0 (nothing to draw: the card says so in words).
 */
export function areaChart({ ts, bucketS, series, width = 560, height = 210, ariaLabel, yFmt = compactNum, valueFmt = (v) => String(v) }) {
  if (!ts?.length || !series.some((s) => s.values.some((v) => typeof v === 'number' && v > 0))) return null;
  const g = stackedAreaLayout({ ts, bucketS, series, width, height });
  const nodes = [];
  for (const tk of g.yTicks)
    nodes.push(svg('line', { class: 'gl', x1: g.plot.x0, x2: g.plot.x1, y1: tk.y, y2: tk.y }), svg('text', { class: 'tick', x: g.plot.x0 - 6, y: tk.y + 4, 'text-anchor': 'end' }, yFmt(tk.v)));
  for (const a of g.areas) if (a.any) nodes.push(svg('path', { class: `ar-fill ${series[a.si].cls}`, d: a.d }));
  for (const tk of g.time.ticks) nodes.push(svg('text', { class: 'tick', x: tk.x, y: height - 7, 'text-anchor': 'middle' }, tk.label));
  for (const b of g.bands) {
    if (!b.present) continue;
    const lines = series.map((s) => `${s.label}: ${typeof s.values[b.i] === 'number' ? valueFmt(s.values[b.i]) : '–'}`).join(' · ');
    nodes.push(titled(svg('rect', { class: 'ar-hit', x: b.x, y: g.plot.y1, width: b.w, height: g.plot.y0 - g.plot.y1 }), `${fmtStamp(b.ts)} UTC · ${lines}`));
  }
  return svg('svg', { class: 'chart rtchart areachart', viewBox: `0 0 ${width} ${height}`, role: 'group', 'aria-label': ariaLabel }, svg('title', ariaLabel), ...nodes);
}
