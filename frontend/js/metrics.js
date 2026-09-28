// Helpers over MetricsSnapshot / history payloads (ARCHITECTURE.md sections 4-5).
/** Find the point for `key` whose labels contain all `labels`. Returns the MetricPoint or undefined. */
export function findPoint(snapshot, key, labels = {}) {
  return (snapshot?.points || []).find((p) => p.key === key && Object.entries(labels).every(([k, v]) => p.labels?.[k] === v));
}
export const valueOf = (snapshot, key, labels) => findPoint(snapshot, key, labels)?.value;
/** Sum of all points of `key` (e.g. tokens across kinds). undefined when none. */
export function sumOf(snapshot, key) {
  const ps = (snapshot?.points || []).filter((p) => p.key === key);
  return ps.length ? ps.reduce((s, p) => s + p.value, 0) : undefined;
}
/** All points of `key` as {label: value} keyed by the value of label `by`. */
export function groupBy(snapshot, key, by) {
  const out = {};
  for (const p of snapshot?.points || []) if (p.key === key && p.labels?.[by] != null) out[p.labels[by]] = p.value;
  return out;
}
/** Counter history -> per-second rate points; negative deltas (counter reset) are dropped. */
export function counterRate(points) {
  const out = [];
  for (let i = 1; i < (points || []).length; i++) {
    const [t0, v0] = points[i - 1], [t1, v1] = points[i], dt = t1 - t0;
    if (dt > 0 && v1 >= v0) out.push([t1, (v1 - v0) / dt]);
  }
  return out;
}
/** Canonical history key with an optional label selector, e.g. keyWith('dyn.ttft_ms', {q:'p95'}) -> 'dyn.ttft_ms{q=p95}'. */
export function keyWith(key, labels) {
  const e = Object.entries(labels || {});
  return e.length ? `${key}{${e.map(([k, v]) => `${k}=${v}`).join(',')}}` : key;
}
/** Convert ts seconds to minutes-ago so chart x axes read "-15m .. now". */
export function relativeMinutes(points, nowSec) {
  const now = nowSec ?? (points.length ? points[points.length - 1][0] : 0);
  return points.map(([t, v]) => [(t - now) / 60, v]);
}
