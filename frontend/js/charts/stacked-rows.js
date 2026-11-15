// Horizontal stacked bars, one per row (addendum 9.14): Dynamo's latency breakdown (queue -> prefill -> decode, one bar for p50, one for
// p95), a GPU's memory now (used / reserved / free). Every segment is an element with an aria-label and a <title>, so the chart is not
// colour-only; the segment values are also in the card's table. All bars share one scale, so a longer bar is a larger total.
import { svg, titled } from '../viz/svg.js';
import { finite } from '../series-math.js';

/**
 * @param opts.rows   [{id, label, parts: [{key, name, value|null, cls (bf1..bf6)}], totalText?}]
 * @param opts.valueFmt  (v) => text for a segment's tooltip
 * Returns an <svg>, or null when no row has a positive part.
 */
export function stackedRows({ rows, width = 560, ariaLabel, valueFmt, labelW = 56, rowH = 30, tail = 96 }) {
  const sum = (r) => r.parts.reduce((a, p) => a + (finite(p.value) && p.value > 0 ? p.value : 0), 0);
  const max = Math.max(0, ...rows.map(sum));
  if (!(max > 0)) return null;
  const plotW = Math.max(60, width - labelW - tail);
  const nodes = [];
  rows.forEach((r, i) => {
    const y = 4 + i * rowH;
    let x = labelW;
    nodes.push(svg('text', { class: 'tick', x: labelW - 8, y: y + rowH / 2 + 3, 'text-anchor': 'end' }, r.label));
    for (const p of r.parts) {
      if (!finite(p.value) || p.value <= 0) continue;
      const w = Math.max(1.5, (p.value / max) * plotW);
      const text = `${r.label} · ${p.name}: ${valueFmt(p.value)}`;
      nodes.push(titled(svg('rect', { class: `${p.cls} sr-seg`, x: x.toFixed(1), y: y + 4, width: w.toFixed(1), height: rowH - 10, rx: 2, role: 'img', 'aria-label': text }), text));
      x += w;
    }
    nodes.push(svg('text', { class: 'tick val', x: (x + 6).toFixed(1), y: y + rowH / 2 + 3 }, r.totalText ?? valueFmt(sum(r))));
  });
  return svg('svg', { class: 'chart rtchart srchart', viewBox: `0 0 ${width} ${rows.length * rowH + 8}`, role: 'group', 'aria-label': ariaLabel }, svg('title', ariaLabel), ...nodes);
}
