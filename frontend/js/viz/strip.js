// Geometry of an up / down strip (addendum 9.16): equal cells across a plot, the height of a cell says its state as well as its colour (down = full, partial = two thirds, up = a low bar,
// no data = an outlined, hatched full cell), so the strip reads in greyscale. Pure functions, no DOM; the SVG is built in js/charts/strip-chart.js. Unit-tested in tests/viz-area.test.mjs.
import { clockTicks } from './scale.js';

export const STRIP_HEIGHT_SHARE = { down: 1, part: 0.66, up: 0.34, none: 1 };

/**
 * @param cells [{ts, binS, state}], oldest first;  @param width the chart width;  @param opts {height, labelW, right, axis (room for the tick row)}
 * Returns {width, height, x0, x1, cw, bars: [{i, x, y, w, h, state}], ticks: [{v, label, x}]}.
 */
export function stripLayout({ cells, width, height = 30, labelW = 0, right = 6, axis = 18 }) {
  const x0 = labelW,
    x1 = Math.max(x0 + 1, width - right);
  const n = cells.length;
  const cw = n ? (x1 - x0) / n : 0;
  const bars = cells.map((c, i) => {
    const h = Math.max(3, Math.round(height * (STRIP_HEIGHT_SHARE[c.state] ?? 1)));
    return { i, x: Math.round((x0 + i * cw) * 10) / 10, y: height - h, w: Math.max(1, Math.round((cw - (cw > 4 ? 1 : 0)) * 10) / 10), h, state: c.state };
  });
  const t0 = cells[0]?.ts,
    t1 = n ? cells[n - 1].ts + cells[n - 1].binS : undefined;
  const ticks = n ? clockTicks(t0, t1, width < 480 ? 4 : 6).map((tk) => ({ ...tk, x: Math.round((x0 + ((tk.v - t0) / Math.max(1, t1 - t0)) * (x1 - x0)) * 10) / 10 })) : [];
  return { width, height: height + axis, x0, x1, cw, bars, ticks };
}
