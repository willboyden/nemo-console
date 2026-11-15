// Pure shaping of the Switchyard flow tab (addendum 9.14): the decision flow (route -> decision source -> target), the per-model
// latency / error / cache / context rows, the call-rate heatmap matrix, fallbacks, routing overhead and client outcomes, all from
// ONE series answer (split=1) plus the metrics snapshot. No DOM, no i18n. A figure the answer cannot give is null, never 0 (an error
// rate with no call counter is unknown, not 0%). Unit-tested in tests/switchyard-model.test.mjs.
import { rateSegments } from './series-model.js';
import { counterIncrease, sumIncrease, meanOf, lastOf, peakOf, finite, gridSize, alignToGrid, mergeFactor, rebin, rebinTimes, arrayStats, baseKey } from './series-math.js';

/** The metric keys of the flow tab (one request: addendum 9.13 allows 12). */
export const FLOW_KEYS = [
  'sy.decisions_total',
  'sy.model.latency_ms',
  'sy.model.errors_total',
  'sy.model.calls',
  'sy.model.cache_hit_ratio',
  'sy.model.max_context_tokens',
  'sy.fallbacks_total',
  'sy.routing_overhead_ms',
  'sy.client_responses_total',
];
export const FLOW_WINDOWS = ['15m', '1h', '24h', '7d'];

/** The series of one key. */
export const seriesOfKey = (series, key) => (series || []).filter((s) => baseKey(s.key) === baseKey(key));

// Node ids carry their column, so a route and a target that share a name stay two nodes.
const ID = { route: 'r:', source: 's:', target: 't:' };
export const nodeId = (layer, name) => `${ID[layer]}${name}`;
export const nodeName = (id) => String(id).slice(2);
export const nodeLayer = (id) => ({ 'r:': 'route', 's:': 'source', 't:': 'target' })[String(id).slice(0, 2)] ?? 'target';

/** `sy.route.calls{route, window}` of the metrics snapshot as [{route, value, window}]: the calls among the newest routing records (not a counter). */
export function routeCallsFromSnapshot(points) {
  const m = new Map();
  for (const p of points || []) {
    if (p?.key !== 'sy.route.calls' || !finite(p.value) || !p.labels?.route) continue;
    const r = String(p.labels.route);
    const have = m.get(r);
    m.set(r, { route: r, value: (have?.value ?? 0) + p.value, window: String(p.labels.window ?? have?.window ?? '') });
  }
  return [...m.values()].sort((a, b) => b.value - a.value || a.route.localeCompare(b.route));
}

/**
 * The decision flow over the window. `decisions` are the series of `sy.decisions_total` (labels source, target and, when the server
 * gives it, route); each one's increase over the window is a flow. WITH a route label on every series the flow has three columns
 * (route -> source -> target); without it only source -> target is known, because the counters do not say which route made which
 * decision (`mode: 'two'`), and the routes are shown as the separate share list `routeShares`.
 * Filters: `route` keeps the decisions of one route (three-column flow only; `routeApplied` says whether it could), `model` keeps the
 * decisions that target that name.
 * Returns {mode: 'three'|'two'|'none', flows: [{source, target, value}], total, resets, routeApplied, routeShares}.
 */
export function decisionFlow({ decisions, routeCalls }, { route = '', model = '' } = {}) {
  const lines = (decisions || []).filter((s) => s.label?.source != null && s.label?.target != null);
  const three = lines.length > 0 && lines.every((s) => s.label.route != null && s.label.route !== '');
  let resets = 0;
  const items = [];
  for (const s of lines) {
    const inc = counterIncrease(s.points);
    if (!inc) continue;
    resets += inc.resets;
    if (inc.value <= 0) continue;
    items.push({ route: three ? String(s.label.route) : null, source: String(s.label.source), target: String(s.label.target), value: inc.value });
  }
  const kept = items.filter((i) => (!route || !three || i.route === route) && (!model || i.target === model));
  const sum = new Map();
  const add = (a, b, v) => sum.set(`${a}\u0000${b}`, { source: a, target: b, value: (sum.get(`${a}\u0000${b}`)?.value ?? 0) + v });
  for (const i of kept) {
    if (three) add(nodeId('route', i.route), nodeId('source', i.source), i.value);
    add(nodeId('source', i.source), nodeId('target', i.target), i.value);
  }
  const flows = [...sum.values()];
  const total = kept.reduce((a, i) => a + i.value, 0);
  const routeShares = (routeCalls || []).filter((r) => (!route || r.route === route) && r.value >= 0);
  return { mode: !items.length ? 'none' : three ? 'three' : 'two', flows, total, resets, routeApplied: three, routeShares };
}

/** The distinct names a filter can offer: routes (from the snapshot and the flow's route labels) and models (from the call series). */
export function filterOptions({ routeCalls, decisions, modelSeries, summary }) {
  const routes = new Set((routeCalls || []).map((r) => r.route));
  for (const s of decisions || []) if (s.label?.route) routes.add(String(s.label.route));
  for (const k of Object.keys(summary?.by_route || {})) routes.add(k);
  const models = new Set();
  for (const s of modelSeries || []) if (s.label?.model) models.add(String(s.label.model));
  for (const k of Object.keys(summary?.by_model || {})) models.add(k);
  const sort = (set) => [...set].filter(Boolean).sort((a, b) => a.localeCompare(b));
  return { routes: sort(routes), models: sort(models) };
}

/** Keep only `name` of a {name: n} object ('' keeps all): the route / model filter on the share cards. */
export function filterCounts(counts, name) {
  if (!name || !counts || typeof counts !== 'object') return counts;
  return Object.prototype.hasOwnProperty.call(counts, name) ? { [name]: counts[name] } : {};
}

/**
 * One row per model: p50 and p99 latency (mean of the window's samples, ms), calls and errors over the window, the error rate (errors
 * / calls; null without both counters or with no calls), the newest cache-hit ratio and the largest context seen (tokens).
 * `latency` series carry labels {model, q}. Rows are ordered by calls (busiest first, unknown last), then name.
 */
export function modelRows({ latency, errors, calls, cache, context }, { model = '' } = {}) {
  const names = new Set();
  for (const list of [latency, errors, calls, cache, context]) for (const s of list || []) if (s.label?.model) names.add(String(s.label.model));
  const by = (list, name) => (list || []).filter((s) => String(s.label?.model) === name);
  const q = (name, qn) => {
    const s = by(latency, name).find((x) => x.label?.q === qn);
    return s ? meanOf(s.points) : null;
  };
  const rows = [...names]
    .filter((n) => !model || n === model)
    .map((name) => {
      const callInc = sumIncrease(by(calls, name)),
        errInc = sumIncrease(by(errors, name));
      const c = by(cache, name)[0],
        x = by(context, name)[0];
      return {
        model: name,
        p50: q(name, 'p50'),
        p99: q(name, 'p99'),
        calls: callInc ? callInc.value : null,
        errors: errInc ? errInc.value : null,
        errorRate: callInc && errInc && callInc.value > 0 ? Math.min(1, errInc.value / callInc.value) : null,
        cache: c ? lastOf(c.points) : null,
        maxContext: x ? peakOf(x.points) : null,
        resets: (callInc?.resets ?? 0) + (errInc?.resets ?? 0),
      };
    });
  rows.sort((a, b) => (b.calls ?? -1) - (a.calls ?? -1) || a.model.localeCompare(b.model));
  return rows;
}

/**
 * The call-rate heatmap: one row per model, one column per time slot, the call RATE (calls per second, or per minute when the
 * busiest slot is below 0.1/s). A stats reset (the counter drops) leaves a gap, as do missing samples; an idle slot is a real 0.
 * Merged to at most `maxCols` columns (mean). Returns {ts, bucketS, unit: 's'|'min', max, rows: [{model, values, total, peak}]}.
 */
export function rateMatrix(callSeries, { from, to, stepS }, { maxCols = 140, model = '' } = {}) {
  const n = gridSize(from, to, stepS);
  const factor = mergeFactor(n, maxCols);
  const rows = (callSeries || [])
    .filter((s) => s.label?.model && (!model || String(s.label.model) === model))
    .map((s) => {
      const pts = rateSegments(s.points, stepS).flat();
      const inc = counterIncrease(s.points);
      return { model: String(s.label.model), raw: alignToGrid(pts, { from, stepS, n }), total: inc ? inc.value : null };
    });
  const peak = Math.max(0, ...rows.flatMap((r) => r.raw.filter(finite)));
  const unit = peak > 0 && peak < 0.1 ? 'min' : 's';
  const k = unit === 'min' ? 60 : 1;
  const out = rows.map((r) => {
    const scaled = r.raw.map((v) => (v == null ? null : v * k));
    return { model: r.model, values: rebin(scaled, factor), total: r.total, peak: arrayStats(scaled).max };
  });
  out.sort((a, b) => (b.total ?? -1) - (a.total ?? -1) || a.model.localeCompare(b.model));
  return { ts: rebinTimes(from, stepS, n, factor), bucketS: stepS * factor, unit, max: peak * k, rows: out };
}

/** Fallbacks over the window: the increase per reason (largest first) and in total; null when no reason has two samples. */
export function fallbackModel(series) {
  const rows = (series || [])
    .map((s) => {
      const inc = counterIncrease(s.points);
      return inc ? { reason: String(s.label?.reason ?? ''), value: inc.value, resets: inc.resets, series: s } : null;
    })
    .filter(Boolean)
    .sort((a, b) => b.value - a.value || a.reason.localeCompare(b.reason));
  return { rows, total: rows.length ? rows.reduce((a, r) => a + r.value, 0) : null, any: rows.length > 0 };
}

/** The routing overhead per quantile ({q}): newest and window mean, ms. */
export function overheadModel(series) {
  const out = {};
  for (const s of series || []) {
    const qn = s.label?.q;
    if (qn) out[qn] = { latest: lastOf(s.points), mean: meanOf(s.points), series: s };
  }
  return out;
}

/** What clients got, by outcome: the increase over the window, largest first; each outcome's share of the total. */
export function outcomeRows(series) {
  const rows = (series || [])
    .map((s) => {
      const inc = counterIncrease(s.points);
      return inc ? { outcome: String(s.label?.outcome ?? ''), value: inc.value } : null;
    })
    .filter(Boolean)
    .sort((a, b) => b.value - a.value || a.outcome.localeCompare(b.outcome));
  const total = rows.reduce((a, r) => a + r.value, 0);
  return rows.map((r) => ({ ...r, pct: total > 0 ? (r.value / total) * 100 : 0 }));
}
