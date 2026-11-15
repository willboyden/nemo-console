// Pure scale and tick helpers for the trace explorer's charts (no DOM, unit-tested in tests/viz-scale.test.mjs).
export const clamp = (v, lo, hi) => Math.min(hi, Math.max(lo, v));
const isNum = (n) => typeof n === 'number' && Number.isFinite(n);
export { isNum };

/** Linear map domain [d0,d1] -> range [r0,r1]; a zero-width domain maps to the middle. `.invert` maps back. */
export function linear(d0, d1, r0, r1) {
  const k = d1 === d0 ? 0 : (r1 - r0) / (d1 - d0);
  const f = (v) => (d1 === d0 ? (r0 + r1) / 2 : r0 + (v - d0) * k);
  f.invert = (p) => (k === 0 ? d0 : d0 + (p - r0) / k);
  return f;
}

/** Log10 map; values <= 0 are clamped to the domain's low end so a zero is drawn at the floor, never NaN. */
export function logScale(d0, d1, r0, r1) {
  const lo = Math.log10(Math.max(d0, 1e-9)),
    hi = Math.log10(Math.max(d1, d0 * 1.0001, 1e-9));
  const lin = linear(lo, hi, r0, r1);
  const f = (v) => lin(Math.log10(Math.max(v, d0, 1e-9)));
  f.invert = (p) => 10 ** lin.invert(p);
  return f;
}

/** The 1-2-5 step at or above span/count. */
export function niceStep(span, count = 5) {
  const raw = span / Math.max(1, count);
  if (!(raw > 0) || !Number.isFinite(raw)) return 1;
  const mag = 10 ** Math.floor(Math.log10(raw));
  return ([1, 2, 5, 10].map((m) => m * mag).find((s) => s >= raw * 0.999999) ?? 10 * mag) || 1;
}

/** Nice axis over [min,max]: the bounds are extended to multiples of the step, so every datum fits inside the axis. */
export function niceScale(min, max, count = 4) {
  if (!isNum(min) || !isNum(max)) return { min: 0, max: 1, step: 1, ticks: [0, 1] };
  if (max <= min) max = min + (min === 0 ? 1 : Math.abs(min) * 0.1 || 1);
  const step = niceStep(max - min, count);
  const lo = Math.floor(min / step + 1e-9) * step,
    hi = Math.ceil(max / step - 1e-9) * step;
  const ticks = [];
  for (let v = lo, i = 0; v <= hi + step * 1e-9 && i < 100; v += step, i++) ticks.push(+v.toFixed(10));
  return { min: lo, max: hi, step, ticks };
}

/** Decade ticks for a log axis over [min,max] (min > 0): 1, 10, 100 ... covering the data. */
export function logTicks(min, max) {
  const lo = Math.floor(Math.log10(Math.max(min, 1e-9))),
    hi = Math.ceil(Math.log10(Math.max(max, min * 1.0001, 1e-9)));
  const out = [];
  for (let e = lo; e <= hi && out.length < 24; e++) out.push(10 ** e);
  return out.length > 1 ? out : [10 ** lo, 10 ** (lo + 1)];
}

const OFFSET_STEPS_MS = [1, 2, 5, 10, 20, 50, 100, 200, 500, 1000, 2000, 5000, 10000, 15000, 30000, 60000, 120000, 300000, 600000, 900000, 1800000, 3600000, 7200000, 21600000, 43200000, 86400000];
/** Format a millisecond offset or duration: 0, 250ms, 1.5s, 2m03s, 1h05m. Compact, for axes. */
export function fmtOffsetMs(ms) {
  if (!isNum(ms)) return '–';
  const a = Math.abs(ms),
    sign = ms < 0 ? '-' : '';
  if (a < 1) return a === 0 ? '0' : `${sign}${a.toFixed(2)}ms`;
  if (a < 1000) return `${sign}${+a.toFixed(a < 10 ? 1 : 0)}ms`;
  if (a < 60000) return `${sign}${+(a / 1000).toFixed(a < 10000 ? 2 : 1)}s`;
  const s = Math.round(a / 1000);
  if (s < 3600) return `${sign}${Math.floor(s / 60)}m${String(s % 60).padStart(2, '0')}s`;
  return `${sign}${Math.floor(s / 3600)}h${String(Math.floor((s % 3600) / 60)).padStart(2, '0')}m`;
}

/** Relative-time ticks over [fromMs,toMs]: the smallest 1-2-5 style step that gives at most `target` ticks. */
export function offsetTicks(fromMs, toMs, target = 6) {
  const span = toMs - fromMs;
  if (!(span > 0)) return [{ v: fromMs, label: fmtOffsetMs(fromMs) }];
  const step = OFFSET_STEPS_MS.find((s) => span / s <= target) ?? 86400000 * Math.ceil(span / 86400000 / target);
  const out = [];
  for (let v = Math.ceil(fromMs / step) * step; v <= toMs + 1e-9 && out.length < 40; v += step) out.push({ v, label: fmtOffsetMs(v) });
  return out;
}

const CLOCK_STEPS_S = [60, 300, 900, 1800, 3600, 10800, 21600, 43200, 86400, 172800, 604800, 2592000];
const p2 = (n) => String(n).padStart(2, '0');
/** UTC calendar day `MM-DD` and clock `HH:MM` of an epoch-second timestamp. */
export const fmtDay = (s) => (isNum(s) && Math.abs(s) <= 8.64e12 ? new Date(s * 1000).toISOString().slice(5, 10) : '–');
export const fmtHM = (s) => (isNum(s) && Math.abs(s) <= 8.64e12 ? new Date(s * 1000).toISOString().slice(11, 16) : '–');
/** Epoch-second ticks over [t0,t1] aligned to UTC: at most `target`, labelled HH:MM, or MM-DD from a day up. */
export function clockTicks(t0, t1, target = 6) {
  if (!isNum(t0) || !isNum(t1) || t1 <= t0) return isNum(t0) ? [{ v: t0, label: fmtHM(t0), major: true }] : [];
  const span = t1 - t0;
  const step = CLOCK_STEPS_S.find((s) => span / s <= target) ?? 2592000;
  const out = [];
  for (let v = Math.ceil(t0 / step) * step; v <= t1 + 1e-9 && out.length < 60; v += step) {
    const d = new Date(v * 1000);
    const midnight = d.getUTCHours() === 0 && d.getUTCMinutes() === 0;
    out.push({ v, label: step >= 86400 ? fmtDay(v) : midnight && span > 86400 ? fmtDay(v) : `${p2(d.getUTCHours())}:${p2(d.getUTCMinutes())}`, major: midnight });
  }
  return out;
}

/** n evenly spread indices out of `count` (always includes 0 and count-1 when n >= 2), for thinning labels. */
export function spreadIndices(count, n) {
  if (count <= 0) return [];
  if (count <= n) return Array.from({ length: count }, (_, i) => i);
  const out = new Set([0, count - 1]);
  for (let k = 1; k < n - 1; k++) out.add(Math.round((k * (count - 1)) / (n - 1)));
  return [...out].sort((a, b) => a - b);
}

/** Fractional bin position of value v among histogram edges (linear inside a bin; clamped to 0..n). */
export function binPosition(edges, v) {
  if (!Array.isArray(edges) || edges.length < 2 || !isNum(v)) return null;
  if (v <= edges[0]) return 0;
  const n = edges.length - 1;
  if (v >= edges[n]) return n;
  for (let i = 0; i < n; i++) {
    if (v < edges[i + 1]) return i + (edges[i + 1] === edges[i] ? 0 : (v - edges[i]) / (edges[i + 1] - edges[i]));
  }
  return n;
}
