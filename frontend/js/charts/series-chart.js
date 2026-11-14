// SVG time-series chart of irregular samples (addendum 9.13): several lines with gaps, an optional max band, a time axis.
// Geometry comes from viz/timeseries.js (pure, unit-tested); this only turns it into elements. Lines differ by colour AND dash
// pattern, every mark has a <title>, and the chart is an svg role="group" with an aria-label; the text alternative (a table
// of latest/min/avg/max per line) is built by the card. Class names (ls1.., ln-dash, ts-band, ts-max) are styled in components.css.
import { svg, titled } from '../viz/svg.js';
import { t } from '../i18n.js';
import { clockTicks } from '../viz/scale.js';
import { timeSeriesLayout, relativeTicks } from '../viz/timeseries.js';
import { fmtStamp } from '../viz/fmt.js';
import { lineStyle } from '../series-model.js';

const DASH = ['', ' ln-dash', ' ln-dot'];
/** CSS classes of the i-th line: colour ls1..ls6 and the dash pattern. */
export const lineClass = (i) => {
  const s = lineStyle(i);
  return { color: `ls${s.color}`, dash: DASH[s.dash].trim(), cls: `ln ls${s.color}${DASH[s.dash]}` };
};

/** `now`, `-30s`, `-5m`, `-2h` for a tick that many seconds before the right edge. */
export function fmtOffset(offsetS) {
  if (offsetS <= 0) return t('chart.now');
  if (offsetS < 60) return `-${offsetS}s`;
  if (offsetS < 3600) return `-${Math.round(offsetS / 60)}m`;
  return `-${+(offsetS / 3600).toFixed(1)}h`;
}

/** The x-axis ticks: counted back from now for the short windows, UTC clock / date for the long ones. */
function xTicks(win, t0, t1, x) {
  if (win === '15m' || win === '1h') return relativeTicks(t0, t1, 5).map((tk) => ({ x: x(tk.v), label: fmtOffset(tk.offsetS) }));
  return clockTicks(t0, t1, 6).map((tk) => ({ x: x(tk.v), label: tk.label }));
}

/**
 * @param lines  [{key, name, idx (position for colour/dash), segments}]
 * @param win    '15m'|'1h'|'24h'|'7d' (picks the axis style)
 * @param from,to  the answer's range, epoch seconds
 * Returns an <svg>, or null when no line has a point.
 */
export function seriesChart({ lines, win, from, to, width = 560, height = 210, yFmt, valueFmt = yFmt, showMax = false, ariaLabel }) {
  const g = timeSeriesLayout({ lines, from, to, width, height, showMax });
  if (g.empty) return null;
  const grid = g.yTicks.flatMap((tk) => [
    svg('line', { class: 'gl', x1: g.plot.x0, x2: g.plot.x1, y1: tk.y, y2: tk.y }),
    svg('text', { class: 'tick', x: g.plot.x0 - 6, y: tk.y + 4, 'text-anchor': 'end' }, yFmt(tk.v)),
  ]);
  const ticks = xTicks(win, g.xDomain[0], g.xDomain[1], g.x).map((tk) => svg('text', { class: 'tick', x: tk.x, y: height - 7, 'text-anchor': 'middle' }, tk.label));
  const marks = [];
  g.lines.forEach((ln, li) => {
    const src = lines[li],
      c = lineClass(src.idx);
    for (const seg of ln.segments) {
      if (seg.bandD) marks.push(svg('path', { class: `ts-band ${c.color}`, d: seg.bandD }));
      if (seg.maxD) marks.push(svg('path', { class: `ts-max ${c.color}`, d: seg.maxD }));
      if (seg.d) marks.push(svg('path', { class: c.cls, d: seg.d }));
      if (seg.dot) marks.push(svg('circle', { class: `pt ${c.color}`, cx: seg.pts[0].x, cy: seg.pts[0].y, r: 2.6 }));
    }
    for (const p of ln.hits) {
      const max = showMax && p.max != null && p.max > p.v + 1e-9 ? ` · ${t('chart.max_short')} ${valueFmt(p.max)}` : '';
      marks.push(titled(svg('circle', { class: `ts-hit ${c.color}`, cx: p.x, cy: p.y, r: 5 }), `${src.name}: ${valueFmt(p.v)}${max} · ${fmtStamp(p.ts)} UTC`));
    }
  });
  return svg('svg', { class: 'chart rtchart tschart', viewBox: `0 0 ${width} ${height}`, role: 'group', 'aria-label': ariaLabel }, svg('title', ariaLabel), ...grid, ...marks, ...ticks);
}

/** The swatch of one line in a legend or picker: a short line segment drawn like the chart's own line. */
export function lineSwatch(idx) {
  const c = lineClass(idx);
  return svg('svg', { class: 'sw-line', viewBox: '0 0 28 8', width: 28, height: 8, 'aria-hidden': 'true' }, svg('line', { class: c.cls, x1: 1, x2: 27, y1: 4, y2: 4 }));
}
