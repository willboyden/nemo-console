// Geometry for time series sampled at irregular times, drawn as one or more lines with gaps (addendum 9.13): a line is a
// list of SEGMENTS (a gap or a counter reset starts a new one), each segment a list of [ts, value, max|null] points.
// Pure functions, no DOM; the SVG is built in js/charts/series-chart.js. Unit-tested in tests/viz-timeseries.test.mjs.
import { linear, niceScale, isNum } from './scale.js';
import { plotBox } from './series.js';

export const TS_MARGIN = { l: 54, r: 12, t: 10, b: 26 };
const r1 = (v) => Math.round(v * 10) / 10;

/** `M x y L x y ...` through points [{x,y}]. */
export function segmentPath(pts) {
  return pts.map((p, i) => `${i ? 'L' : 'M'}${r1(p.x)} ${r1(p.y)}`).join('');
}

/** Closed polygon between two polylines of equal length: along `upper` left to right, back along `lower`. */
export function bandPath(upper, lower) {
  if (upper.length < 2 || upper.length !== lower.length) return '';
  const fwd = upper.map((p, i) => `${i ? 'L' : 'M'}${r1(p.x)} ${r1(p.y)}`).join('');
  const back = [...lower]
    .reverse()
    .map((p) => `L${r1(p.x)} ${r1(p.y)}`)
    .join('');
  return `${fwd}${back}Z`;
}

/** n indices spread evenly over [0, count) (first and last included), for thinning hover targets. */
export function thinIndices(count, n) {
  if (count <= 0 || n <= 0) return [];
  if (count <= n) return Array.from({ length: count }, (_, i) => i);
  const out = new Set([0, count - 1]);
  for (let k = 1; k < n - 1; k++) out.add(Math.round((k * (count - 1)) / (n - 1)));
  return [...out].sort((a, b) => a - b);
}

const REL_STEPS_S = [30, 60, 120, 300, 600, 900, 1800, 3600, 7200, 21600];
/** Ticks counted back from the right edge: [{v (epoch s), offsetS (0 = now)}], at most `target`, newest last. */
export function relativeTicks(from, to, target = 5) {
  const span = to - from;
  if (!(span > 0)) return [{ v: to, offsetS: 0 }];
  const step = REL_STEPS_S.find((s) => span / s <= target) ?? REL_STEPS_S.at(-1);
  const out = [];
  for (let k = 0; k * step <= span && out.length < 40; k++) out.push({ v: to - k * step, offsetS: k * step });
  return out.reverse();
}

/**
 * @param lines   [{key, segments: [[[ts, v, max|null], ...], ...]}]
 * @param from,to the x domain (epoch seconds); when omitted, the data extent
 * @param showMax include each point's max in the y range and return the max polyline + band
 * Returns {plot, x, y, yTicks:[{v,y}], xDomain:[from,to], lines:[{key, segments:[{pts, d, dot, maxD, bandD, spread}], hits, empty}], max, min, empty}.
 * Non-finite values are dropped (a null is a gap, never a zero); a segment of one point is a dot (d is '').
 */
export function timeSeriesLayout({ lines, from, to, width, height, margin = TS_MARGIN, yTickCount = 4, yMin, yMax, showMax = false, maxHits = 48 }) {
  const plot = plotBox(width, height, margin);
  const clean = lines.map((ln) => ({
    key: ln.key,
    segments: (ln.segments || []).map((seg) => seg.filter((p) => isNum(p?.[0]) && isNum(p?.[1]))).filter((seg) => seg.length),
  }));
  const all = clean.flatMap((ln) => ln.segments.flat());
  if (!all.length)
    return {
      plot,
      x: () => plot.x0,
      y: () => plot.y0,
      yTicks: [],
      xDomain: [from ?? 0, to ?? 1],
      lines: clean.map((ln) => ({ key: ln.key, segments: [], hits: [], empty: true })),
      max: 0,
      min: 0,
      empty: true,
    };
  let t0 = isNum(from) ? from : Math.min(...all.map((p) => p[0])),
    t1 = isNum(to) ? to : Math.max(...all.map((p) => p[0]));
  if (!(t1 > t0)) t1 = t0 + 1;
  const ys = all.flatMap((p) => (showMax && isNum(p[2]) ? [p[1], p[2]] : [p[1]]));
  const dMin = Math.min(...ys),
    dMax = Math.max(...ys);
  const axis = niceScale(yMin ?? Math.min(0, dMin), yMax ?? dMax, yTickCount);
  const x = linear(t0, t1, plot.x0, plot.x1),
    y = linear(axis.min, axis.max, plot.y0, plot.y1);
  const outLines = clean.map((ln) => {
    const segments = ln.segments.map((seg) => {
      const pts = seg.map((p) => ({ x: r1(x(p[0])), y: r1(y(p[1])), ts: p[0], v: p[1], max: isNum(p[2]) ? p[2] : null }));
      const spread = pts.some((p) => p.max != null && p.max > p.v + 1e-9);
      const maxPts = pts.map((p) => ({ x: p.x, y: r1(y(p.max != null ? Math.max(p.max, p.v) : p.v)) }));
      return {
        pts,
        dot: pts.length === 1,
        d: pts.length > 1 ? segmentPath(pts) : '',
        spread,
        maxD: showMax && spread && pts.length > 1 ? segmentPath(maxPts) : '',
        bandD: showMax && spread ? bandPath(maxPts, pts) : '',
      };
    });
    const flat = segments.flatMap((s) => s.pts);
    const hits = thinIndices(flat.length, maxHits).map((i) => flat[i]);
    return { key: ln.key, segments, hits, empty: false };
  });
  return { plot, x, y, yTicks: axis.ticks.map((v) => ({ v, y: r1(y(v)) })), xDomain: [t0, t1], lines: outLines, max: axis.max, min: axis.min, empty: false };
}

/** Drop interior ticks closer than `gap` px to a neighbour kept (the first and the last tick always stay), so labels never touch. */
export function spaceTicks(ticks, gap) {
  if (ticks.length <= 2) return ticks;
  const out = [ticks[0]];
  const last = ticks.at(-1);
  for (const tk of ticks.slice(1, -1)) if (tk.x - out.at(-1).x >= gap && last.x - tk.x >= gap) out.push(tk);
  out.push(last);
  return out;
}

/**
 * Layout of a trend across runs: x = the run's position (0..n-1, evenly spaced; runs are far apart in time and a time
 * axis would bunch them), y = the chosen number. Points with a null value are not drawn (a failed run has no number).
 * groups: [{key, points: [{i, v, id}]}] (points of one group in run order).
 * Returns {plot, x, y, yTicks, xTicks:[{i, x}], groups:[{key, pts:[{x,y,i,v,id}], d}], empty}.
 */
export function trendLayout({ groups, n, width, height, margin = { l: 54, r: 14, t: 12, b: 26 }, yTickCount = 4, xTickCount = 6 }) {
  const plot = plotBox(width, height, margin);
  const vals = groups.flatMap((g) => g.points.map((p) => p.v)).filter(isNum);
  if (!vals.length || n < 1) return { plot, x: () => plot.x0, y: () => plot.y0, yTicks: [], xTicks: [], groups: [], empty: true };
  const lo = Math.min(...vals),
    hi = Math.max(...vals);
  // A trend of a quantity that never goes below zero starts at zero unless the values sit far above it (then the axis
  // would be mostly empty and the differences invisible): keep zero when min <= 60% of max.
  const lower = lo < 0 ? lo : lo <= 0.6 * hi ? 0 : lo - (hi - lo) * 0.15;
  const axis = niceScale(lower, hi, yTickCount);
  const x = n === 1 ? () => (plot.x0 + plot.x1) / 2 : linear(0, n - 1, plot.x0 + 6, plot.x1 - 6);
  const y = linear(axis.min, axis.max, plot.y0, plot.y1);
  const xs = new Set([0, n - 1]);
  for (let k = 1; k < xTickCount - 1; k++) xs.add(Math.round((k * (n - 1)) / (xTickCount - 1)));
  const out = groups.map((g) => {
    const pts = g.points.filter((p) => isNum(p.v)).map((p) => ({ x: r1(x(p.i)), y: r1(y(p.v)), i: p.i, v: p.v, id: p.id }));
    return { key: g.key, pts, d: pts.length > 1 ? segmentPath(pts) : '' };
  });
  return {
    plot,
    x,
    y,
    yTicks: axis.ticks.map((v) => ({ v, y: r1(y(v)) })),
    xTicks: spaceTicks(
      [...xs].sort((a, b) => a - b).map((i) => ({ i, x: r1(x(i)) })),
      46,
    ),
    groups: out,
    empty: false,
  };
}
