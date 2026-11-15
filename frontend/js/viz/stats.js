// Small pure statistics for the wave-3 pages (addendum 9.15): quantiles of a list, medians, bins. No DOM. Unit-tested in
// tests/viz-bars.test.mjs. A value that is not a finite number is never counted (it is "no value", not zero).
const isNum = (n) => typeof n === 'number' && Number.isFinite(n);

/** The finite numbers of a list, sorted ascending. */
export const sortedNums = (xs) => (Array.isArray(xs) ? xs.filter(isNum).sort((a, b) => a - b) : []);

/**
 * Quantile q (0..1) of an ASCENDING list by linear interpolation between the two nearest ranks (the "type 7" rule most tools
 * use). null for an empty list.
 */
export function quantile(sorted, q) {
  const n = sorted.length;
  if (!n) return null;
  if (n === 1) return sorted[0];
  const pos = Math.min(1, Math.max(0, q)) * (n - 1);
  const lo = Math.floor(pos);
  const hi = Math.ceil(pos);
  return sorted[lo] + (sorted[hi] - sorted[lo]) * (pos - lo);
}

/** The median of any list of numbers (non-numbers ignored); null when none. */
export const median = (xs) => quantile(sortedNums(xs), 0.5);

/** The mean of the finite numbers of a list; null when none. */
export function mean(xs) {
  const v = sortedNums(xs);
  return v.length ? v.reduce((a, b) => a + b, 0) / v.length : null;
}

/**
 * `bins` bins over the data: geometric edges when the data is positive and spans more than a factor of `geoRatio` (latencies are
 * long-tailed: equal slots on a log axis), else equal-width edges. Returns {edges: [bins + 1], counts: [bins], geometric} or null
 * for no data. Every value lands in exactly one bin (the last bin includes the maximum).
 */
export function histogramBins(values, { bins = 20, geoRatio = 8 } = {}) {
  const v = sortedNums(values);
  if (!v.length) return null;
  const lo = v[0],
    hi = v[v.length - 1];
  const geometric = lo > 0 && hi / lo > geoRatio;
  let edges;
  if (hi === lo) {
    const pad = Math.abs(lo) * 0.05 || 0.5;
    edges = Array.from({ length: bins + 1 }, (_, i) => lo - pad + ((2 * pad) / bins) * i);
  } else if (geometric) edges = Array.from({ length: bins + 1 }, (_, i) => lo * (hi / lo) ** (i / bins));
  else edges = Array.from({ length: bins + 1 }, (_, i) => lo + ((hi - lo) / bins) * i);
  edges[0] = Math.min(edges[0], lo);
  edges[bins] = Math.max(edges[bins], hi);
  const counts = new Array(bins).fill(0);
  for (const x of v) {
    let b = 0;
    while (b < bins - 1 && x >= edges[b + 1]) b++;
    counts[b]++;
  }
  return { edges, counts, geometric };
}
