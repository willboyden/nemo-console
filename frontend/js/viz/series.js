// Time-series geometry: plot boxes, stacked bars, multi-line charts with gaps, swim-lane packing. Pure functions.
import { linear, niceScale, isNum, clockTicks } from './scale.js';

export const MARGIN = { l: 50, r: 12, t: 10, b: 26 };
/** The plot rectangle inside a width x height chart. y0 is the bottom edge, y1 the top (screen coordinates). */
export function plotBox(width, height, m = MARGIN) {
  return { x0: m.l, x1: width - m.r, y0: height - m.b, y1: m.t, w: width - m.l - m.r, h: height - m.t - m.b, width, height };
}
const r1 = (v) => Math.round(v * 10) / 10;

/** Time scale over equal buckets: bucket i covers [ts[i], ts[i]+bucketS]. Returns x(ts), the bands and clock ticks. */
export function timeBands(ts, bucketS, plot) {
  const n = ts.length;
  if (!n) return { x: () => plot.x0, bands: [], ticks: [], n: 0 };
  const t0 = ts[0],
    t1 = ts[0] + Math.max(1, n * bucketS);
  const x = linear(t0, t1, plot.x0, plot.x1);
  const bands = ts.map((t, i) => {
    const a = x(t),
      b = x(t + bucketS);
    return { i, x: a, w: b - a, cx: (a + b) / 2, ts: t };
  });
  return { x, bands, n, t0, t1, ticks: clockTicks(t0, t1, 6).map((tk) => ({ ...tk, x: x(tk.v) })) };
}

/**
 * Stacked bars. series = [{key, values:[..n]}] (null counts as 0). Returns {plot, y, yTicks, max, rects:[{si,key,i,x,y,w,h,v}], totals}.
 * The y axis is "nice": its top is at or above the tallest stack.
 */
export function stackedBars({ ts, bucketS, series, width, height, margin = MARGIN, yTickCount = 4, maxBarW = Infinity }) {
  const plot = plotBox(width, height, margin);
  const tb = timeBands(ts, bucketS, plot);
  const n = ts.length;
  const totals = Array.from({ length: n }, (_, i) => series.reduce((a, s) => a + (isNum(s.values[i]) ? s.values[i] : 0), 0));
  const axis = niceScale(0, Math.max(...totals, 0) || 1, yTickCount);
  const y = linear(axis.min, axis.max, plot.y0, plot.y1);
  const rects = [];
  const gap = tb.bands[0] && tb.bands[0].w > 4 ? 1 : 0;
  for (const band of tb.bands) {
    let acc = 0;
    series.forEach((s, si) => {
      const v = isNum(s.values[band.i]) ? s.values[band.i] : 0;
      if (v <= 0) return;
      const top = y(acc + v),
        bottom = y(acc);
      const bw = Math.min(maxBarW, Math.max(0.5, band.w - gap));
      rects.push({ si, key: s.key, i: band.i, x: r1(band.cx - bw / 2), y: r1(top), w: Math.max(0.5, r1(bw)), h: Math.max(0.5, r1(bottom - top)), v });
      acc += v;
    });
  }
  return { plot, y, yTicks: axis.ticks.map((v) => ({ v, y: r1(y(v)) })), max: axis.max, rects, totals, time: tb };
}

/** Path data through points, lifting the pen at null values. points: [{x,y}|null]. */
export function pathWithGaps(points) {
  let d = '',
    pen = false;
  for (const p of points) {
    if (!p) {
      pen = false;
      continue;
    }
    d += `${pen ? 'L' : 'M'}${r1(p.x)} ${r1(p.y)}`;
    pen = true;
  }
  return d;
}

/**
 * Lines over time buckets. series = [{key, values}] (null = no data, a gap). Returns {plot, yTicks, lines:[{key, d, points}]}.
 * points carry {x,y,i,v}. A one-point run is returned as a dot (d is empty) so it still shows.
 */
export function lineSeries({ ts, bucketS, series, width, height, margin = MARGIN, yTickCount = 4, yMax }) {
  const plot = plotBox(width, height, margin);
  const tb = timeBands(ts, bucketS, plot);
  const vals = series.flatMap((s) => s.values.filter(isNum));
  const axis = niceScale(0, yMax ?? (Math.max(...vals, 0) || 1), yTickCount);
  const y = linear(axis.min, axis.max, plot.y0, plot.y1);
  const lines = series.map((s) => {
    const points = tb.bands.map((b) => (isNum(s.values[b.i]) ? { x: b.cx, y: y(s.values[b.i]), i: b.i, v: s.values[b.i] } : null));
    return { key: s.key, d: pathWithGaps(points), points: points.filter(Boolean) };
  });
  return { plot, y, yTicks: axis.ticks.map((v) => ({ v, y: r1(y(v)) })), max: axis.max, lines, time: tb };
}

/**
 * Pack intervals into the fewest lanes (swim lanes): items sorted by start, each into the first lane whose last item
 * ended before it began (+ `pad` seconds). items: [{start, end}] -> same objects with `lane`; also returns the lane count.
 * `maxLanes` caps the lanes: once they are all busy, an item goes into the lane that frees up first and overlaps it;
 * `overlapped` counts those items (0 when nothing had to overlap).
 */
export function packLanes(items, pad = 0, maxLanes = Infinity) {
  const sorted = items.map((it, i) => ({ it, i })).sort((a, b) => a.it.start - b.it.start || a.i - b.i);
  const ends = [];
  const out = new Array(items.length);
  let overlapped = 0;
  for (const { it, i } of sorted) {
    let lane = ends.findIndex((e) => e + pad <= it.start);
    if (lane < 0 && ends.length >= maxLanes) {
      lane = ends.reduce((best, e, j) => (e < ends[best] ? j : best), 0);
      overlapped++;
      ends[lane] = Math.max(ends[lane], it.end, it.start);
    } else if (lane < 0) {
      lane = ends.length;
      ends.push(it.end);
    } else ends[lane] = Math.max(it.end, it.start);
    out[i] = { ...it, lane };
  }
  return { items: out, lanes: ends.length, overlapped };
}

/**
 * Context growth for a session: one bar per LLM call. mode 'time' places calls by timestamp, 'index' evenly by number.
 * Each bar: uncached input (bottom share excluded: cached sits at the bottom), cached input, output tokens (own panel).
 * Returns {top: {rects, yTicks, ...}, bottom: {...}, xs}. calls: [{ts, input_tokens, cached_tokens, output_tokens, reasoning_tokens}].
 */
export function contextGrowth({ calls, mode = 'time', width, height, margin = { l: 56, r: 12, t: 24, b: 26 }, split = 0.68 }) {
  const n = calls.length;
  const total = height - margin.t - margin.b;
  const topH = Math.round(total * split) - 6;
  const topBox = { x0: margin.l, x1: width - margin.r, y1: margin.t, y0: margin.t + topH, w: width - margin.l - margin.r, h: topH };
  const botBox = { x0: margin.l, x1: width - margin.r, y1: margin.t + topH + 30, y0: height - margin.b, w: topBox.w, h: height - margin.b - (margin.t + topH + 30) };
  const times = calls.map((c) => c.ts);
  const t0 = n ? Math.min(...times) : 0,
    t1 = n ? Math.max(...times) : 1;
  const xOf = mode === 'index' ? (c, i) => topBox.x0 + ((i + 0.5) / Math.max(1, n)) * topBox.w : (c) => linear(t0, t1 === t0 ? t0 + 1 : t1, topBox.x0 + 6, topBox.x1 - 6)(c.ts);
  const bw = Math.max(1.5, Math.min(14, (topBox.w / Math.max(1, n)) * 0.8));
  const inMax = niceScale(0, Math.max(...calls.map((c) => c.input_tokens ?? 0), 1), 3);
  const outMax = niceScale(0, Math.max(...calls.map((c) => c.output_tokens ?? 0), 1), 2);
  const yT = linear(inMax.min, inMax.max, topBox.y0, topBox.y1),
    yB = linear(outMax.min, outMax.max, botBox.y0, botBox.y1);
  const bars = calls.map((c, i) => {
    const cx = xOf(c, i),
      inp = Math.max(0, c.input_tokens ?? 0),
      ca = Math.min(inp, Math.max(0, c.cached_tokens ?? 0));
    return {
      i,
      call: c,
      x: r1(cx - bw / 2),
      w: r1(bw),
      cx,
      cached: { y: r1(yT(ca)), h: r1(yT(0) - yT(ca)) },
      uncached: { y: r1(yT(inp)), h: r1(yT(ca) - yT(inp)) },
      out: { y: r1(yB(Math.max(0, c.output_tokens ?? 0))), h: r1(yB(0) - yB(Math.max(0, c.output_tokens ?? 0))) },
    };
  });
  const xTicks = mode === 'index' ? [] : clockTicks(t0, t1, 6).map((tk) => ({ ...tk, x: xOf({ ts: tk.v }, 0) }));
  return {
    top: { box: topBox, yTicks: inMax.ticks.map((v) => ({ v, y: r1(yT(v)) })), max: inMax.max },
    bottom: { box: botBox, yTicks: outMax.ticks.map((v) => ({ v, y: r1(yB(v)) })), max: outMax.max },
    bars,
    xTicks,
    yT,
  };
}
