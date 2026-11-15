// SVG chart of a percentile curve and of a scatter of runs (addendum 9.15). The curve has one line per run (A solid, B dashed: colour AND dash tell them
// apart) over evenly spaced percentiles (p1 p5 ... p99); a percentile a run did not record is a gap in its line, never a drop to zero. The scatter has one
// marker per run or result: the colour group is also a SHAPE (circle, square, diamond, triangle ...), so groups are not told apart by colour alone. Geometry:
// viz/bars.js curveLayout / pointsLayout (pure, unit-tested). Every mark has a <title>; the chart is an svg role="group" with an aria-label.
import { svg, titled } from '../viz/svg.js';
import { curveLayout, pointsLayout } from '../viz/bars.js';
import { lineClass } from './series-chart.js';

/** The look of run A (a solid line) and run B (a dashed line in another colour) in every A / B overlay: colour AND dash, so B is never told apart by colour alone. */
export const RUN_STYLE = { a: { cls: 'ln ls1', color: 'ls1' }, b: { cls: 'ln ls2 ln-dash', color: 'ls2' } };
/** The legend swatch of a RUN_STYLE entry (a short line segment drawn like the chart's own line). */
export const styleSwatch = (style) =>
  svg('svg', { class: 'sw-line', viewBox: '0 0 28 8', width: 28, height: 8, 'aria-hidden': 'true' }, svg('line', { class: style.cls, x1: 1, x2: 27, y1: 4, y2: 4 }));

const frame = (w, h, label, nodes, cls) => svg('svg', { class: `chart rtchart ${cls}`, viewBox: `0 0 ${w} ${h}`, role: 'group', 'aria-label': label }, svg('title', label), ...nodes);

/**
 * @param cats   the x labels (p1 ... p99);  @param lines [{key, name, idx (colour / dash slot) or style ({cls, color}: see RUN_STYLE), values: [number|null]}]
 * @param opts   width, height, yFmt (axis), valueFmt (tooltip), xTitle, ariaLabel
 * Returns an <svg>, or null when no line has a value.
 */
export function curveChart({ cats, lines, width = 360, height = 190, yFmt, valueFmt = yFmt, xTitle = null, ariaLabel }) {
  const g = curveLayout({ cats, lines, width, height });
  if (g.empty) return null;
  const nodes = g.yTicks.flatMap((tk) => [
    svg('line', { class: 'gl', x1: g.plot.x0, x2: g.plot.x1, y1: tk.y, y2: tk.y }),
    svg('text', { class: 'tick', x: g.plot.x0 - 6, y: tk.y + 4, 'text-anchor': 'end' }, yFmt(tk.v)),
  ]);
  // narrow charts name every other percentile
  const every = width < 300 ? 2 : 1;
  cats.forEach((c, i) => {
    if (i % every === 0 || i === cats.length - 1) nodes.push(svg('text', { class: 'tick', x: g.xs[i], y: height - 20, 'text-anchor': 'middle' }, c));
  });
  if (xTitle) nodes.push(svg('text', { class: 'axis-title', x: (g.plot.x0 + g.plot.x1) / 2, y: height - 4, 'text-anchor': 'middle' }, xTitle));
  g.lines.forEach((ln, li) => {
    const src = lines[li];
    const c = src.style ?? lineClass(src.idx ?? li);
    if (ln.d) nodes.push(svg('path', { class: c.cls, d: ln.d }));
    for (const p of ln.pts) nodes.push(titled(svg('circle', { class: `pt ${c.color}`, cx: p.x, cy: p.y, r: 3.5 }), `${src.name} · ${cats[p.i]}: ${valueFmt(p.v)}`));
  });
  return frame(width, height, ariaLabel, nodes, 'curvechart');
}

const MARKS = ['circle', 'square', 'diamond', 'triangle', 'cross', 'ring'];
/** The marker of group g as an SVG element at (cx, cy) with the class `sc<g+1>` (the colour) — and a different shape per group. */
export function marker(g, cx, cy, r = 4.2) {
  const cls = `sc-dot sc${(g % 6) + 1}`;
  switch (MARKS[g % MARKS.length]) {
    case 'square':
      return svg('rect', { class: cls, x: cx - r, y: cy - r, width: 2 * r, height: 2 * r, rx: 1 });
    case 'diamond':
      return svg('path', { class: cls, d: `M${cx} ${cy - r - 1}L${cx + r + 1} ${cy}L${cx} ${cy + r + 1}L${cx - r - 1} ${cy}Z` });
    case 'triangle':
      return svg('path', { class: cls, d: `M${cx} ${cy - r - 1}L${cx + r + 1} ${cy + r}L${cx - r - 1} ${cy + r}Z` });
    case 'cross':
      return svg('path', { class: `${cls} sc-cross`, d: `M${cx - r} ${cy}H${cx + r}M${cx} ${cy - r}V${cy + r}` });
    case 'ring':
      return svg('circle', { class: `${cls} sc-ring`, cx, cy, r: r - 0.5 });
    default:
      return svg('circle', { class: cls, cx, cy, r });
  }
}
/** The legend key of group g: the same marker, small (an <svg> for a legend list item). */
export function markerKey(g) {
  return svg('svg', { class: 'sw-mark', viewBox: '0 0 14 14', width: 14, height: 14, 'aria-hidden': 'true' }, marker(g, 7, 7, 4));
}

/**
 * @param points  [{id, x, y, g, text}] (text = the tooltip and the aria-label of the mark)
 * @param opts    width, height, logX, xFmt, yFmt, xTitle, yTitle, yDomain, onPick(id, {compare}), a / b (selected ids, ringed A / B), ariaLabel
 * Returns {svg, skipped} where svg is null when no point has both coordinates.
 */
export function pointsChart({ points, width = 560, height = 280, logX = false, xFmt, yFmt, xTitle, yTitle, yDomain = null, onPick, a = null, b = null, ariaLabel }) {
  const g = pointsLayout({ points, width, height, logX, yDomain });
  if (g.empty) return { svg: null, skipped: g.skipped };
  const nodes = [
    ...g.yTicks.flatMap((tk) => [
      svg('line', { class: 'gl', x1: g.plot.x0, x2: g.plot.x1, y1: tk.y, y2: tk.y }),
      svg('text', { class: 'tick', x: g.plot.x0 - 6, y: tk.y + 4, 'text-anchor': 'end' }, yFmt(tk.v)),
    ]),
    ...g.xTicks.flatMap((tk) => [
      svg('line', { class: 'gl v', x1: tk.x, x2: tk.x, y1: g.plot.y1, y2: g.plot.y0 }),
      svg('text', { class: 'tick', x: tk.x, y: height - 20, 'text-anchor': 'middle' }, xFmt(tk.v)),
    ]),
    svg('text', { class: 'axis-title', x: (g.plot.x0 + g.plot.x1) / 2, y: height - 4, 'text-anchor': 'middle' }, xTitle),
    svg('text', { class: 'axis-title', x: 10, y: (g.plot.y0 + g.plot.y1) / 2, 'text-anchor': 'middle', transform: `rotate(-90 10 ${(g.plot.y0 + g.plot.y1) / 2})` }, yTitle),
  ];
  for (const d of g.dots) {
    const text = d.text ?? '';
    const grp = svg(
      'g',
      { class: `sc-pt${onPick ? ' sc-pick' : ''}`, role: onPick ? 'button' : 'img', 'aria-label': text, ...(onPick ? { tabindex: 0 } : {}) },
      marker(d.g, d.cx, d.cy),
      svg('circle', { class: 'sc-hit', cx: d.cx, cy: d.cy, r: 9 }),
    );
    titled(grp, text);
    if (onPick) {
      grp.addEventListener('click', (e) => onPick(d.id, { compare: e.shiftKey }));
      grp.addEventListener('keydown', (e) => {
        if (e.key === 'Enter' || e.key === ' ') {
          e.preventDefault();
          onPick(d.id, { compare: e.shiftKey });
        }
      });
    }
    nodes.push(grp);
    for (const [sel, tag] of [
      [a, 'a'],
      [b, 'b'],
    ]) {
      if (sel === d.id)
        nodes.push(
          svg('circle', { class: `rh-ring rh-ring-${tag}`, cx: d.cx, cy: d.cy, r: 9 }),
          svg('text', { class: `rh-ring-tag rh-ring-${tag}`, x: d.cx, y: d.cy - 12, 'text-anchor': 'middle' }, tag.toUpperCase()),
        );
    }
  }
  return { svg: frame(width, height, ariaLabel, nodes, 'scatterchart pointschart'), skipped: g.skipped };
}
