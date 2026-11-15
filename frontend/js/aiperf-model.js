// Pure shaping for the AIPerf run detail (addendum 9.15): percentile curves from `GET /systems/aiperf/runs/{id}/summary`, the HTTP-phase
// bar, and the per-request view (latency histogram, swim lanes by worker, error codes) from `.../runs/{id}/requests`. No DOM, no i18n:
// words are the views' job. A statistic the run did not record is null and shows as an en dash, never as 0. Unit-tested in
// tests/aiperf-model.test.mjs.
import { quantile, sortedNums, histogramBins } from './viz/stats.js';

const isNum = (n) => typeof n === 'number' && Number.isFinite(n);

/** The percentiles the summary route carries, in order; `PCT_X` are the numbers behind the names. */
export const PCT_KEYS = ['p1', 'p5', 'p10', 'p25', 'p50', 'p75', 'p90', 'p95', 'p99'];
export const PCT_X = PCT_KEYS.map((k) => Number(k.slice(1)));

/** The three latency metrics of the curves: our id, AIPerf's own metric name (profile_export_aiperf.json) and the names we also accept. */
export const CURVE_METRICS = [
  { id: 'ttft', name: 'time_to_first_token', alt: ['ttft', 'time_to_first_output_token'] },
  { id: 'itl', name: 'inter_token_latency', alt: ['itl'] },
  { id: 'latency', name: 'request_latency', alt: ['latency', 'request_latency_ms'] },
];

/** The multiplier that turns a value of this unit into milliseconds (the metrics are ms in AIPerf; an odd unit is converted, an unknown one is read as ms). */
export function msFactor(unit) {
  switch (String(unit ?? 'ms').toLowerCase()) {
    case 's':
    case 'sec':
    case 'seconds':
      return 1000;
    case 'us':
    case 'µs':
      return 0.001;
    case 'ns':
      return 1e-6;
    default:
      return 1;
  }
}

const metricOf = (summary, def) => {
  const m = summary?.metrics;
  if (!m || typeof m !== 'object' || Array.isArray(m)) return null;
  for (const k of [def.name, ...def.alt]) if (m[k] && typeof m[k] === 'object') return m[k];
  return null;
};

/**
 * The nine percentiles of one metric of one summary, in ms: {values: [number|null x 9], unit, n} or null when the summary has no
 * such metric or none of its percentiles.
 */
export function curveOf(summary, def) {
  const m = metricOf(summary, def);
  if (!m) return null;
  const f = msFactor(m.unit);
  const values = PCT_KEYS.map((k) => (isNum(m[k]) ? m[k] * f : null));
  const n = values.filter(isNum).length;
  return n ? { values, unit: 'ms', n, avg: isNum(m.avg) ? m.avg * f : null } : null;
}

/**
 * The curves to draw for run A and (optionally) run B: [{id, name, a: curve|null, b: curve|null}] for the metrics at least one of the
 * runs has. A run with no curve for a metric simply has no line (the card says so), it is never drawn flat at zero.
 */
export function percentileCurves(summaryA, summaryB) {
  return CURVE_METRICS.map((def) => ({ id: def.id, name: def.name, a: curveOf(summaryA, def), b: curveOf(summaryB, def) })).filter((c) => c.a || c.b);
}

/** The five HTTP phases in the order a request goes through them. */
export const HTTP_PHASES = ['dns', 'connecting', 'sending', 'waiting', 'receiving'];

/** `http_phases_ms` as {dns, ..., receiving} with null for a phase not reported; null when the run has none at all. */
export function httpPhases(summary) {
  const p = summary?.http_phases_ms;
  if (!p || typeof p !== 'object' || Array.isArray(p)) return null;
  const out = {};
  for (const k of HTTP_PHASES) out[k] = isNum(p[k]) && p[k] >= 0 ? p[k] : null;
  return HTTP_PHASES.some((k) => out[k] != null) ? out : null;
}

/** The facts of a summary the detail shows as a line: {requests, concurrency, isl, osl, duration_s, preset, model, verdict}; numbers or null. */
export function summaryFacts(summary) {
  const num = (v) => (isNum(v) ? v : null);
  const str = (v) => (typeof v === 'string' && v ? v : null);
  return {
    requests: num(summary?.requests),
    concurrency: num(summary?.concurrency),
    isl: num(summary?.isl),
    osl: num(summary?.osl),
    duration_s: num(summary?.duration_s),
    preset: str(summary?.preset),
    model: str(summary?.model),
    verdict: str(summary?.verdict),
  };
}

// ------------------------------------------------------------------------------------------------ per-request view

/** The measures of the per-request histogram: row field -> id. */
export const REQUEST_MEASURES = [
  { id: 'latency', field: 'latency_ms' },
  { id: 'ttft', field: 'ttft_ms' },
  { id: 'itl', field: 'itl_avg_ms' },
];

/**
 * The route's rows as clean rows: only the documented fields, numbers where a number belongs, `error_code` an integer or null, `worker` a
 * non-negative integer. A row with no start or end is dropped (it cannot be placed). Returns {rows, total, returned, truncated}.
 */
export function cleanRequests(answer) {
  const src = Array.isArray(answer?.rows) ? answer.rows : [];
  const num = (v) => (isNum(v) ? v : null);
  const rows = [];
  for (const r of src) {
    if (!r || typeof r !== 'object' || !isNum(r.start_ms) || !isNum(r.end_ms)) continue;
    const worker = Number.isInteger(r.worker) && r.worker >= 0 ? r.worker : 0;
    rows.push({
      i: isNum(r.i) ? r.i : rows.length,
      start_ms: r.start_ms,
      end_ms: Math.max(r.end_ms, r.start_ms),
      latency_ms: num(r.latency_ms),
      ttft_ms: num(r.ttft_ms),
      itl_avg_ms: num(r.itl_avg_ms),
      input_tokens: num(r.input_tokens),
      output_tokens: num(r.output_tokens),
      worker,
      error_code: Number.isInteger(r.error_code) ? r.error_code : null, // null = the request succeeded; 0 = it failed without a usable HTTP code
    });
  }
  const total = isNum(answer?.total) ? answer.total : rows.length;
  return { rows, total, returned: isNum(answer?.returned) ? answer.returned : rows.length, truncated: answer?.truncated === true };
}

/**
 * The histogram of one measure over the requests that SUCCEEDED (a request that failed has no meaningful latency to speak of; its
 * error code is on the error bars). Returns {edges, counts, geometric, n, p50, p95, missing, failed} or null when no request has the
 * measure. `missing` counts successful requests without the measure.
 */
export function requestHistogram(rows, field = 'latency_ms', { bins = 20 } = {}) {
  const ok = rows.filter((r) => r.error_code == null);
  const vals = ok.map((r) => r[field]).filter(isNum);
  const hist = histogramBins(vals, { bins });
  if (!hist) return null;
  const sorted = sortedNums(vals);
  return { ...hist, n: vals.length, p50: quantile(sorted, 0.5), p95: quantile(sorted, 0.95), missing: ok.length - vals.length, failed: rows.length - ok.length };
}

/**
 * The swim lanes by worker: one lane per worker (the busiest `maxLanes`, in worker order), and at most `maxBars` bars in all (every failed
 * request is kept; the others are sampled evenly), so a 5000-row answer stays a few thousand elements.
 * Returns {lanes: [{id, label (worker index), worker, n, errors}], items: [{i, lane, start, end, error, latency, code}], workers, shown, sampled}.
 */
export function swimLanes(rows, { maxLanes = 12, maxBars = 1500 } = {}) {
  const by = new Map();
  for (const r of rows) {
    const e = by.get(r.worker) || { worker: r.worker, n: 0, errors: 0 };
    e.n++;
    if (r.error_code != null) e.errors++;
    by.set(r.worker, e);
  }
  const workers = [...by.values()];
  const kept = [...workers]
    .sort((a, b) => b.n - a.n || a.worker - b.worker)
    .slice(0, maxLanes)
    .sort((a, b) => a.worker - b.worker);
  const laneOf = new Map(kept.map((w, i) => [w.worker, i]));
  const inLane = rows.filter((r) => laneOf.has(r.worker));
  const bad = inLane.filter((r) => r.error_code != null);
  const good = inLane.filter((r) => r.error_code == null);
  const room = Math.max(0, maxBars - bad.length);
  const step = good.length > room ? good.length / Math.max(1, room) : 1;
  const sampled = step > 1 ? Array.from({ length: Math.min(room, good.length) }, (_, k) => good[Math.min(good.length - 1, Math.floor(k * step))]) : good;
  const items = [...bad, ...sampled]
    .sort((a, b) => a.start_ms - b.start_ms || a.i - b.i)
    .map((r) => ({ i: r.i, lane: laneOf.get(r.worker), start: r.start_ms, end: r.end_ms, error: r.error_code != null, latency: r.latency_ms, code: r.error_code }));
  return {
    lanes: kept.map((w) => ({ id: `w${w.worker}`, label: `#${w.worker}`, worker: w.worker, n: w.n, errors: w.errors })),
    items,
    workers: workers.length,
    shown: items.length,
    sampled: items.length < inLane.length,
  };
}

/** The error codes as counts: {errors, total, rate, codes: [{code, count, share}]} most frequent first (ties by code). rate is null for no rows. Code 0 is a failure with no usable HTTP code. */
export function errorCodes(rows) {
  const counts = new Map();
  let errors = 0;
  for (const r of rows) {
    if (r.error_code == null) continue;
    errors++;
    counts.set(r.error_code, (counts.get(r.error_code) || 0) + 1);
  }
  const codes = [...counts.entries()].map(([code, count]) => ({ code, count, share: errors ? count / errors : 0 })).sort((a, b) => b.count - a.count || a.code - b.code);
  return { errors, total: rows.length, rate: rows.length ? errors / rows.length : null, codes };
}

// ------------------------------------------------------------------------------------------------ over runs

/** The measures a throughput-vs-latency scatter can put on x (event field -> id); the y axis is the output throughput. */
export const SCATTER_X = [
  { id: 'ttft_p50_ms', field: 'ttft_p50_ms' },
  { id: 'ttft_p99_ms', field: 'ttft_p99_ms' },
  { id: 'itl_p50_ms', field: 'itl_p50_ms' },
];

/**
 * One point per run that has both numbers: {id, x, y, g (a colour/shape slot), concurrency}, grouped by concurrency (the lowest value
 * first; the slots run out at `maxGroups`, later values share the last one). `rows` come from runs.shapeRuns. Returns {points, groups: [{value, g, count}], skipped}.
 */
export function throughputPoints(rows, xField, { yField = 'output_tps', maxGroups = 6 } = {}) {
  const num = (v) => (isNum(v) ? v : null);
  const kept = [];
  let skipped = 0;
  for (const r of rows) {
    const x = num(r.fields?.[xField]),
      y = num(r.fields?.[yField]);
    if (x == null || y == null) {
      skipped++;
      continue;
    }
    kept.push({ id: r.id, x, y, c: num(r.fields?.concurrency) });
  }
  const values = [...new Set(kept.map((p) => p.c))].sort((a, b) => (a == null ? 1 : b == null ? -1 : a - b));
  const slot = new Map(values.map((v, i) => [v, Math.min(i, maxGroups - 1)]));
  const groups = values.map((v) => ({ value: v, g: slot.get(v), count: kept.filter((p) => p.c === v).length }));
  return { points: kept.map((p) => ({ id: p.id, x: p.x, y: p.y, g: slot.get(p.c), concurrency: p.c })), groups, skipped };
}
