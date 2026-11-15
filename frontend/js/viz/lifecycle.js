// Geometry of the sandbox lifecycle swim lanes (addendum 9.16): a lane per sandbox, a bar per phase from one event to the next, on one time axis. Pure functions, no DOM; the SVG is
// built in js/charts/lifecycle-chart.js. Unit-tested in tests/viz-area.test.mjs.
import { linear, clockTicks } from './scale.js';

/**
 * @param lanes [{name, segments: [{from, to, ...}], marks: [{ts, action}]}] (openshell-model.lifecycleLanes);  @param from,to the window (epoch s)
 * Returns {width, height, x0, x1, x(ts), laneH, laneY(i), segs: [{lane, k, x, w, seg}], marks: [{lane, k, x, mark}], ticks: [{v, label, x}]}. A bar is at least `minW` px wide so an instant phase still shows.
 */
export function lifecycleLayout({ lanes, from, to, width, labelW = 120, laneH = 30, top = 4, bottom = 22, right = 12, minW = 3 }) {
  const x0 = labelW,
    x1 = Math.max(x0 + 1, width - right);
  const x = linear(from, to > from ? to : from + 1, x0, x1);
  const laneY = (i) => top + i * laneH;
  const segs = [],
    marks = [];
  lanes.forEach((ln, lane) => {
    ln.segments.forEach((seg, k) => {
      const a = x(seg.from),
        b = x(seg.to);
      segs.push({ lane, k, seg, x: Math.round(a * 10) / 10, w: Math.max(minW, Math.round((b - a) * 10) / 10) });
    });
    ln.marks.forEach((mark, k) => marks.push({ lane, k, mark, x: Math.round(x(mark.ts) * 10) / 10 }));
  });
  const ticks = clockTicks(from, to, width < 480 ? 4 : 6).map((tk) => ({ ...tk, x: Math.round(x(tk.v) * 10) / 10 }));
  return { width, height: top + lanes.length * laneH + bottom, x0, x1, x, laneH, laneY, segs, marks, ticks };
}
