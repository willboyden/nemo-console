// SVG trend of one number across runs (addendum 9.13): x = the run's position (oldest to newest), y = the number, one line and
// one colour+dash pattern per group, a point per run that HAS the number (a failed run without it is not a point and never a
// zero). Points are focusable buttons; the selected runs carry an A / B ring. Geometry: viz/timeseries.js trendLayout.
import { svg, titled } from '../viz/svg.js';
import { trendLayout } from '../viz/timeseries.js';
import { lineClass } from './series-chart.js';

/**
 * @param groups  [{key, count, points: [{i, v, id, ts}]}] from runs.trendGroups (the order decides the style)
 * @param n       number of runs on the axis
 * @param opts    yFmt(v) axis text, valueFmt(v) tooltip text, dateOf(i) tick text, labelOf(id) run label, a / b selected ids,
 *                onPick(id, {compare: boolean}) called by a click (compare = shift) or Enter / Space
 * Returns an <svg> or null when no run has the number.
 */
export function trendChart({ groups, n, width = 560, height = 220, yFmt, valueFmt, dateOf, labelOf, a = null, b = null, onPick, ariaLabel, xTickCount = 6 }) {
  const g = trendLayout({ groups, n, width, height, xTickCount });
  if (g.empty) return null;
  const grid = g.yTicks.flatMap((tk) => [
    svg('line', { class: 'gl', x1: g.plot.x0, x2: g.plot.x1, y1: tk.y, y2: tk.y }),
    svg('text', { class: 'tick', x: g.plot.x0 - 6, y: tk.y + 4, 'text-anchor': 'end' }, yFmt(tk.v)),
  ]);
  // Two neighbouring ticks that would print the same date show it once (the first and the last tick always stay).
  const tickLabels = [];
  g.xTicks.forEach((tk, k) => {
    const label = dateOf(tk.i);
    const prev = tickLabels.at(-1);
    if (prev && prev.label === label) {
      if (k === g.xTicks.length - 1 && tickLabels.length > 1) tickLabels[tickLabels.length - 1] = { tk, label }; // the last tick wins over a twin
      return;
    }
    tickLabels.push({ tk, label });
  });
  const xs = tickLabels.map(({ tk, label }) => svg('text', { class: 'tick', x: tk.x, y: height - 7, 'text-anchor': tk.i === 0 ? 'start' : tk.i === n - 1 ? 'end' : 'middle' }, label));
  const marks = [];
  const rings = [];
  g.groups.forEach((grp, gi) => {
    const c = lineClass(gi);
    if (grp.d) marks.push(svg('path', { class: c.cls, d: grp.d }));
    for (const p of grp.pts) {
      const text = `${labelOf(p.id)}: ${valueFmt(p.v)}`;
      const dot = svg(
        'g',
        { class: 'rh-pt', role: 'button', tabindex: 0, 'aria-label': text },
        svg('circle', { class: `pt ${c.color}`, cx: p.x, cy: p.y, r: 4 }),
        svg('circle', { class: 'rh-hit', cx: p.x, cy: p.y, r: 9 }),
      );
      titled(dot, text);
      dot.addEventListener('click', (e) => onPick?.(p.id, { compare: e.shiftKey }));
      dot.addEventListener('keydown', (e) => {
        if (e.key === 'Enter' || e.key === ' ') {
          e.preventDefault();
          onPick?.(p.id, { compare: e.shiftKey });
        }
      });
      marks.push(dot);
      for (const [sel, tag] of [
        [a, 'a'],
        [b, 'b'],
      ]) {
        if (sel !== p.id) continue;
        rings.push(
          svg('circle', { class: `rh-ring rh-ring-${tag}`, cx: p.x, cy: p.y, r: 8 }),
          svg('text', { class: `rh-ring-tag rh-ring-${tag}`, x: p.x, y: p.y - 12, 'text-anchor': 'middle' }, tag.toUpperCase()),
        );
      }
    }
  });
  return svg('svg', { class: 'chart rtchart rhchart', viewBox: `0 0 ${width} ${height}`, role: 'group', 'aria-label': ariaLabel }, svg('title', ariaLabel), ...grid, ...rings, ...marks, ...xs);
}
