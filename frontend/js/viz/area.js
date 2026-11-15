// Geometry of a stepped stacked area over equal time buckets (addendum 9.16: the OpenShell phase counts over time). Each bucket is a flat step, so a count that changes between two
// samples is not drawn as a slope that was never measured. A bucket with no sample in ANY series is a gap (the area breaks there, it is not drawn down to zero); a series with no
// value in a bucket that others have adds nothing to the stack there. Pure functions, no DOM; the SVG is built in js/charts/area-chart.js. Unit-tested in tests/viz-area.test.mjs.
import { plotBox, timeBands, MARGIN } from './series.js';
import { linear, niceScale, isNum } from './scale.js';

const r1 = (v) => Math.round(v * 10) / 10;

/**
 * @param ts       bucket start times (epoch s, equal steps);  @param bucketS the step
 * @param series   [{key, values: [n numbers | null]}], stacked bottom to top in this order
 * Returns {plot, y, yTicks, max, areas: [{si, key, d, any}], bands: [{i, x, w, ts, total, present}], totals, time}. `d` is a closed path of the series' band (one subpath per run of
 * present buckets); `any` is false for a series that never rises above 0 (nothing to draw).
 */
export function stackedAreaLayout({ ts, bucketS, series, width, height, margin = MARGIN, yTickCount = 4 }) {
  const plot = plotBox(width, height, margin);
  const tb = timeBands(ts, bucketS, plot);
  const n = ts.length;
  const val = (s, i) => (isNum(s.values[i]) ? s.values[i] : null);
  const present = Array.from({ length: n }, (_, i) => series.some((s) => val(s, i) != null));
  const totals = Array.from({ length: n }, (_, i) => series.reduce((a, s) => a + (val(s, i) ?? 0), 0));
  const axis = niceScale(0, Math.max(...totals, 0) || 1, yTickCount);
  const y = linear(axis.min, axis.max, plot.y0, plot.y1);
  // runs of present buckets
  const runs = [];
  for (let i = 0; i < n; i++) {
    if (!present[i]) continue;
    const last = runs[runs.length - 1];
    if (last && last[1] === i) last[1] = i + 1;
    else runs.push([i, i + 1]);
  }
  const areas = series.map((s, si) => {
    const base = (i) => series.slice(0, si).reduce((a, x) => a + (val(x, i) ?? 0), 0);
    let d = '';
    let any = false;
    for (const [i0, i1] of runs) {
      const up = [],
        lo = [];
      for (let i = i0; i < i1; i++) {
        const b = tb.bands[i];
        const v = val(s, i) ?? 0;
        if (v > 0) any = true;
        const yTop = r1(y(base(i) + v)),
          yBot = r1(y(base(i)));
        up.push([r1(b.x), r1(b.x + b.w), yTop]);
        lo.push([r1(b.x), r1(b.x + b.w), yBot]);
      }
      let sub = `M${up[0][0]} ${up[0][2]}`;
      for (const [x0, x1, yy] of up) sub += `L${x0} ${yy}L${x1} ${yy}`;
      for (let k = lo.length - 1; k >= 0; k--) sub += `L${lo[k][1]} ${lo[k][2]}L${lo[k][0]} ${lo[k][2]}`;
      d += `${sub}Z`;
    }
    return { si, key: s.key, d, any };
  });
  const bands = tb.bands.map((b) => ({ i: b.i, x: r1(b.x), w: Math.max(1, r1(b.w)), ts: b.ts, total: totals[b.i], present: present[b.i] }));
  return { plot, y, yTicks: axis.ticks.map((v) => ({ v, y: r1(y(v)) })), max: axis.max, areas, bands, totals, time: tb };
}
