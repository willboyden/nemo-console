// Geometry of the policy-decision timeline (addendum 9.16): a lane per category, a column per time slot, allowed and denied stacked in each column on ONE height scale (so lanes compare).
// Pure functions, no DOM; the SVG is built in js/charts/decision-chart.js. Unit-tested in tests/viz-area.test.mjs.
import { clockTicks } from './scale.js';

/**
 * @param model openshell-model.decisionModel (lanes with bins [{a, d}], bins, from, to, binS, maxBin)
 * Returns {width, height, x0, x1, cw, laneH, plotH, laneY(i), cols: [{lane, bin, x, w, a, d, ha, hd, ya, yd}], ticks: [{v, label, x}]}. Allowed sits at the bottom of its column, denied on top of it.
 */
export function decisionLayout({ model, width, labelW = 90, laneH = 46, top = 4, bottom = 22, right = 8, pad = 6 }) {
  const x0 = labelW,
    x1 = Math.max(x0 + 1, width - right);
  const cw = model.bins ? (x1 - x0) / model.bins : 0;
  const plotH = laneH - pad * 2;
  const laneY = (i) => top + i * laneH;
  const cols = [];
  model.lanes.forEach((ln, lane) => {
    const base = laneY(lane) + laneH - pad;
    ln.bins.forEach((b, bin) => {
      if (!b.a && !b.d) return;
      const ha = b.a ? Math.max(2, Math.round((b.a / model.maxBin) * plotH * 10) / 10) : 0;
      const hd = b.d ? Math.max(3, Math.round((b.d / model.maxBin) * plotH * 10) / 10) : 0;
      cols.push({ lane, bin, a: b.a, d: b.d, ha, hd, x: Math.round((x0 + bin * cw) * 10) / 10, w: Math.max(1, Math.round((cw - (cw > 4 ? 1 : 0)) * 10) / 10), ya: base - ha, yd: base - ha - hd });
    });
  });
  const ticks =
    model.from != null && model.to != null && model.to > model.from
      ? clockTicks(model.from, model.to, width < 480 ? 4 : 6).map((tk) => ({ ...tk, x: Math.round((x0 + ((tk.v - model.from) / (model.to - model.from)) * (x1 - x0)) * 10) / 10 }))
      : [];
  return { width, height: top + model.lanes.length * laneH + bottom, x0, x1, cw, laneH, plotH, laneY, cols, ticks };
}
