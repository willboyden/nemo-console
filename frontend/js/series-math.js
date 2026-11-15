// Pure helpers over NORMALIZED series (series-model.normalizeAnswer: [{key, label, id, points: [[ts, avg, max]]}]) for the wave-2
// cards (addendum 9.14): the increase of a counter over a window, means, peaks, alignment of points onto one time grid and
// rebinning a grid into fewer columns. No DOM. A value that is not there is null, never 0: every helper returns null (or leaves a
// null in the grid) when there is nothing to compute from. Unit-tested in tests/series-math.test.mjs.
export const finite = (v) => typeof v === 'number' && Number.isFinite(v);

/** `dyn.tokens{kind=output}` -> `dyn.tokens`. The series route answers with the key AS REQUESTED (a labelled request keeps its selector). */
export const baseKey = (key) => String(key ?? '').split('{')[0];
/** The labels of a series: the selector of a labelled request key, overridden by the label the route parsed (split=1). */
export function seriesLabels(s) {
  const out = {};
  const m = /\{(.*)\}$/.exec(String(s?.key ?? ''));
  if (m) for (const pair of m[1].split(',')) if (pair.includes('=')) out[pair.slice(0, pair.indexOf('='))] = pair.slice(pair.indexOf('=') + 1);
  return { ...out, ...(s?.label || {}) };
}

const clean = (points) =>
  (points || [])
    .filter((p) => Array.isArray(p) && finite(p[0]) && finite(p[1]))
    .slice()
    .sort((a, b) => a[0] - b[0]);

/**
 * How much a counter grew over its points, Prometheus-style: the sum of the positive steps; a step DOWN is a reset (the counter
 * restarted from zero), and counts as the new value (what it accumulated since the restart). The time between the last sample before
 * a reset and the reset itself is not seen, so a window with a reset is a lower bound: `resets` says how many there were. A gap in
 * the samples loses nothing (the counter is cumulative). Fewer than two points: null (not 0).
 * Returns {value, resets, n, from, to, seconds} | null.
 */
export function counterIncrease(points) {
  const p = clean(points);
  if (p.length < 2) return null;
  let value = 0,
    resets = 0;
  for (let i = 1; i < p.length; i++) {
    const d = p[i][1] - p[i - 1][1];
    if (d < 0) {
      resets++;
      value += p[i][1];
    } else value += d;
  }
  return { value, resets, n: p.length, from: p[0][0], to: p[p.length - 1][0], seconds: p[p.length - 1][0] - p[0][0] };
}

/** The increase of several counters added up (null when none of them had two points). `resets` adds up too. */
export function sumIncrease(seriesList) {
  let value = 0,
    resets = 0,
    seen = 0,
    seconds = 0;
  for (const s of seriesList || []) {
    const inc = counterIncrease(s.points);
    if (!inc) continue;
    seen++;
    value += inc.value;
    resets += inc.resets;
    seconds = Math.max(seconds, inc.seconds);
  }
  return seen ? { value, resets, seconds, lines: seen } : null;
}

/** Mean of the average column over the points; null when there are none. */
export function meanOf(points) {
  const p = clean(points);
  return p.length ? p.reduce((a, q) => a + q[1], 0) / p.length : null;
}
/** The newest value; null when there are none. */
export function lastOf(points) {
  const p = clean(points);
  return p.length ? p[p.length - 1][1] : null;
}
/** The highest value seen: the max column where a point has one, else its average; null when there are none. */
export function peakOf(points) {
  const p = clean(points);
  return p.length ? Math.max(...p.map((q) => (finite(q[2]) ? Math.max(q[2], q[1]) : q[1]))) : null;
}

/** Group series by the value of one label; a series without that label is left out. Insertion order. */
export function groupByLabel(series, name) {
  const m = new Map();
  for (const s of series || []) {
    const v = s.label?.[name];
    if (v == null) continue;
    const k = String(v);
    if (!m.has(k)) m.set(k, []);
    m.get(k).push(s);
  }
  return m;
}

/** Number of grid slots for a window: first bucket start `from` to last `to`, one slot per `stepS`. */
export const gridSize = (from, to, stepS) => (finite(from) && finite(to) && finite(stepS) && stepS > 0 && to >= from ? Math.floor((to - from) / stepS + 1e-6) + 1 : 0);

/**
 * Put points onto a regular grid: slot i is the bucket starting at from + i * stepS. A point lands in its nearest slot (a point
 * outside the grid is dropped, two in one slot: the later wins); every other slot stays null. Returns an array of n values.
 */
export function alignToGrid(points, { from, stepS, n }) {
  const out = new Array(Math.max(0, n)).fill(null);
  if (!(stepS > 0)) return out;
  for (const p of clean(points)) {
    const i = Math.round((p[0] - from) / stepS);
    if (i >= 0 && i < n) out[i] = p[1];
  }
  return out;
}

/** The merge factor that brings n columns to at most maxCols. */
export const mergeFactor = (n, maxCols) => (maxCols > 0 && n > maxCols ? Math.ceil(n / maxCols) : 1);

/**
 * Merge every `factor` neighbouring slots into one: the mean of the values present (how 'mean') or their maximum ('max'); a
 * merged slot with no value at all stays null (a gap is never turned into 0).
 */
export function rebin(values, factor, how = 'mean') {
  if (factor <= 1) return values.slice();
  const out = [];
  for (let i = 0; i < values.length; i += factor) {
    const part = values.slice(i, i + factor).filter(finite);
    out.push(part.length ? (how === 'max' ? Math.max(...part) : part.reduce((a, b) => a + b, 0) / part.length) : null);
  }
  return out;
}

/** Start times of the merged slots: from, from + step * factor, ... */
export const rebinTimes = (from, stepS, n, factor) => Array.from({ length: Math.ceil(n / Math.max(1, factor)) }, (_, i) => from + i * stepS * Math.max(1, factor));

/** {n, latest, min, avg, max} over a plain array that may hold nulls; every field null when nothing is present. */
export function arrayStats(values) {
  const v = (values || []).filter(finite);
  if (!v.length) return { n: 0, latest: null, min: null, avg: null, max: null };
  let latest = null;
  for (let i = values.length - 1; i >= 0; i--)
    if (finite(values[i])) {
      latest = values[i];
      break;
    }
  return { n: v.length, latest, min: Math.min(...v), avg: v.reduce((a, b) => a + b, 0) / v.length, max: Math.max(...v) };
}

/**
 * A copy of a series answer that holds only the series of `keys` (one card out of a request that fetched several). `where` may
 * narrow further: (series) => boolean. The answer's window, step and range stay, so a card cannot tell it from its own request.
 */
export function subAnswer(answer, keys, where = null) {
  const want = new Set([].concat(keys).map(baseKey));
  const series = (answer?.series || []).filter((s) => want.has(baseKey(s.key)) && (!where || where(s)));
  return { ...(answer || {}), series };
}

/** Seconds each series window spans (addendum 9.13). */
export const WINDOW_SPAN_S = { '15m': 900, '1h': 3600, '24h': 86400, '7d': 604800 };
