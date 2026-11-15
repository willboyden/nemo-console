// Geometry of an event swim lane: one lane per GPU (or any key), a mark per event at its time. Marks that would sit on top of each
// other in a lane alternate between two sub-rows, so each stays clickable. Pure functions, no DOM; the SVG is built in
// js/charts/timeline-chart.js. Unit-tested in tests/viz-heat.test.mjs.
import { linear, clockTicks } from './scale.js';

/**
 * @param items  [{id, ts, lane}] (lane = a key from `lanes`)
 * @param lanes  lane keys, top to bottom
 * Returns {width, height, plot: {x0, x1}, laneH, laneY(lane index), marks: [{item, x, y, lane, sub}], ticks: [{v, x, label}]}.
 */
export function timelineLayout({ items, lanes, from, to, width, laneH = 34, labelW = 64, right = 12, top = 4, bottom = 22, near = 16 }) {
  const x0 = labelW,
    x1 = Math.max(x0 + 1, width - right);
  const x = linear(from, to > from ? to : from + 1, x0 + 8, x1 - 8);
  const laneY = (i) => top + i * laneH;
  const marks = [];
  for (let li = 0; li < lanes.length; li++) {
    const mine = items.filter((it) => it.lane === lanes[li]).sort((a, b) => a.ts - b.ts);
    let prev = null;
    for (const it of mine) {
      const px = x(it.ts);
      const sub = prev && px - prev.x < near ? 1 - prev.sub : 0;
      const m = { item: it, x: px, sub, lane: li, y: laneY(li) + laneH / 2 + (sub ? laneH * 0.22 : -laneH * 0.14) };
      marks.push(m);
      prev = m;
    }
  }
  return {
    width,
    height: top + lanes.length * laneH + bottom,
    plot: { x0, x1 },
    laneH,
    laneY,
    marks,
    ticks: clockTicks(from, to, width < 480 ? 4 : 6).map((tk) => ({ ...tk, x: x(tk.v) })),
  };
}
