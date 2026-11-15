// The policy-decision timeline (addendum 9.16): a lane per category, a column per time slot, allowed (a solid bar) and denied (a hatched bar with a cross, on top) stacked in the column, all lanes on
// one height scale. Denied is told apart by its hatching, its glyph and the legend as well as its colour. Each lane is one element with an aria-label (its allowed and denied counts); each
// column has a <title> with its time span and counts. The allowed / denied totals and the denied targets are in the card's table. Geometry: viz/decisions.js (pure, unit-tested).
import { svg, titled } from '../viz/svg.js';
import { decisionLayout } from '../viz/decisions.js';
import { fmtStamp } from '../viz/fmt.js';
import { t } from '../i18n.js';

let uid = 0;
const clipText = (s, n) => (String(s).length > n ? `${String(s).slice(0, n - 1)}…` : String(s));

/** Returns an <svg>, or null when the model has no lane. */
export function decisionChart({ model, width = 640, ariaLabel }) {
  if (!model.lanes.length) return null;
  const longest = Math.max(...model.lanes.map((l) => l.category.length));
  const labelW = Math.min(width < 480 ? 74 : 120, Math.max(60, longest * 6.6 + 14));
  const g = decisionLayout({ model, width, labelW });
  const pid = `dc-hatch-${++uid}`;
  const nodes = [
    svg(
      'defs',
      svg(
        'pattern',
        { id: pid, width: 5, height: 5, patternUnits: 'userSpaceOnUse', patternTransform: 'rotate(45)' },
        svg('rect', { class: 'dc-hatch-bg', width: 5, height: 5 }),
        svg('line', { class: 'dc-hatch-line', x1: 0, y1: 0, x2: 0, y2: 5 }),
      ),
    ),
  ];
  for (const tk of g.ticks)
    nodes.push(svg('line', { class: 'gl v', x1: tk.x, x2: tk.x, y1: g.laneY(0), y2: g.height - 20 }), svg('text', { class: 'tick', x: tk.x, y: g.height - 6, 'text-anchor': 'middle' }, tk.label));
  const byLane = new Map();
  for (const c of g.cols) (byLane.get(c.lane) ?? byLane.set(c.lane, []).get(c.lane)).push(c);
  model.lanes.forEach((ln, i) => {
    const y = g.laneY(i);
    const aria = t('osv.dec_lane', { category: ln.category, allowed: ln.allowed, denied: ln.denied });
    const grp = svg('g', { class: 'dc-lane', role: 'img', 'aria-label': aria });
    titled(grp, aria);
    grp.append(
      svg('rect', { class: 'tl-lane', x: g.x0, y: y + 2, width: g.x1 - g.x0, height: g.laneH - 4, rx: 3 }),
      svg('text', { class: 'tick dc-name', x: g.x0 - 8, y: y + g.laneH / 2 + 4, 'text-anchor': 'end' }, clipText(ln.category, Math.floor((labelW - 12) / 6.4))),
    );
    for (const c of byLane.get(i) || []) {
      const from = model.from + c.bin * model.binS;
      const tip = t('osv.dec_bin', { from: fmtStamp(from), to: fmtStamp(from + model.binS).slice(6), allowed: c.a, denied: c.d });
      const col = svg('g', { class: 'dc-col', 'aria-hidden': 'true' });
      titled(col, tip);
      if (c.a) col.append(svg('rect', { class: 'dc-allowed', x: c.x, y: c.ya, width: c.w, height: c.ha }));
      if (c.d) {
        col.append(svg('rect', { class: 'dc-denied', x: c.x, y: c.yd, width: c.w, height: c.hd, fill: `url(#${pid})` }));
        col.append(svg('text', { class: 'dc-cross', x: c.x + c.w / 2, y: Math.max(y + 10, c.yd - 2), 'text-anchor': 'middle' }, '✕'));
      }
      grp.append(col);
    }
    nodes.push(grp);
  });
  return svg('svg', { class: 'chart rtchart dcchart', viewBox: `0 0 ${width} ${g.height}`, role: 'group', 'aria-label': ariaLabel }, svg('title', ariaLabel), ...nodes);
}
