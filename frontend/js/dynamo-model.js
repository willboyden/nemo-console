// Pure shaping of the Dynamo Status and Events tabs (addendum 9.14): the latency breakdown (queue -> prefill -> decode), output tokens
// per second, the capacity panel, spec-decode acceptance by position and the filters of the log events. No DOM, no i18n. A phase or
// figure the worker does not report is null, never 0. Unit-tested in tests/dynamo-model.test.mjs.
import { meanOf, lastOf, finite, gridSize, alignToGrid, mergeFactor, rebin, rebinTimes, baseKey, seriesLabels } from './series-math.js';

/** The keys of the status tab's one series request (at most 12, addendum 9.13). */
export const DYN_KEYS = [
  'dyn.ttft_window_ms',
  'dyn.itl_window_ms',
  'dyn.ttft_ms',
  'dyn.itl_ms',
  'dyn.phase_ms',
  'dyn.requests_running',
  'dyn.requests_waiting',
  'dyn.preemptions_total',
  'dyn.kv_cache_usage',
  'dyn.tokens{kind=completion}',
  'dyn.prefix_cache_hit_ratio',
];
export const DYN_WINDOWS = ['15m', '1h', '24h', '7d'];
export const PHASES = ['queue', 'prefill', 'decode'];
export const PHASE_QUANTILES = ['p50', 'p95'];

export const seriesOfKey = (series, key) => (series || []).filter((s) => baseKey(s.key) === baseKey(key));

/**
 * The latency breakdown from `dyn.phase_ms{phase, q}`: for each quantile, the window mean of each phase (ms) and `total` (the sum of
 * the phases that are there; `partial` when one is missing, so the stack is a lower bound). Quantiles of different phases do not add
 * up exactly (the p95 of a sum is not the sum of the p95s): the view says so. `inference` (the worker's own total) is kept apart as
 * `inference`, for comparison. Returns {p50: {queue, prefill, decode, inference, total, partial, any}, p95: {...}}.
 */
export function phaseSummary(series) {
  const lines = seriesOfKey(series, 'dyn.phase_ms');
  const out = {};
  for (const qn of PHASE_QUANTILES) {
    const get = (phase) => {
      const s = lines.find((x) => x.label?.phase === phase && x.label?.q === qn);
      return s ? meanOf(s.points) : null;
    };
    const v = Object.fromEntries([...PHASES, 'inference'].map((p) => [p, get(p)]));
    const present = PHASES.map((p) => v[p]).filter(finite);
    out[qn] = { ...v, total: present.length ? present.reduce((a, b) => a + b, 0) : null, partial: present.length > 0 && present.length < PHASES.length, any: present.length > 0 };
  }
  return out;
}

/**
 * The phases of one quantile over time for the stacked chart: [{key: 'queue', values}] on one grid (null where the worker reported
 * nothing: no request finished in that poll), merged to at most `maxCols` columns.
 */
export function phaseSeries(series, qn, { from, to, stepS }, maxCols = 120) {
  const n = gridSize(from, to, stepS);
  const factor = mergeFactor(n, maxCols);
  const lines = seriesOfKey(series, 'dyn.phase_ms');
  const out = PHASES.map((phase) => {
    const s = lines.find((x) => x.label?.phase === phase && x.label?.q === qn);
    return { key: phase, present: !!s, values: s ? rebin(alignToGrid(s.points, { from, stepS, n }), factor) : null };
  });
  return { ts: rebinTimes(from, stepS, n, factor), bucketS: stepS * factor, series: out.filter((p) => p.present).map((p) => ({ key: p.key, values: p.values })), any: out.some((p) => p.present) };
}

/**
 * The output-token counter. The adapter's key is `dyn.tokens{kind=completion}` (the one the page requests); 9.14's text calls it `output`,
 * so a series of that kind wins when an answer has one. Either way it is the counter of generated tokens. Returns {kind, series: [..one series..]} (kind null when neither).
 */
export function outputTokens(series) {
  const toks = seriesOfKey(series, 'dyn.tokens');
  for (const kind of ['output', 'completion']) {
    const hit = toks.filter((s) => seriesLabels(s).kind === kind && s.points.length);
    if (hit.length) return { kind, series: hit };
  }
  return { kind: null, series: [] };
}

export const CAPACITY_KINDS = ['context_length', 'kv_blocks', 'max_num_seqs', 'max_batched_tokens'];
/**
 * The capacity panel: `dyn.capacity{kind}` as reported (gauges) in the contract's order, plus how much of it is used now when the
 * snapshot has the matching figure (running requests against max_num_seqs, KV usage against kv_blocks). Returns
 * {rows: [{kind, value, used, usedRatio}], any}.
 */
export function capacityModel(points) {
  const val = (key, labels) => {
    const p = (points || []).find((x) => x?.key === key && Object.entries(labels || {}).every(([k, v]) => x.labels?.[k] === v));
    return p && finite(p.value) ? p.value : null;
  };
  const running = val('dyn.requests_running'),
    kv = val('dyn.kv_cache_usage');
  const rows = CAPACITY_KINDS.map((kind) => {
    const value = val('dyn.capacity', { kind });
    let used = null;
    if (value != null && value > 0 && kind === 'max_num_seqs' && running != null) used = running;
    if (value != null && value > 0 && kind === 'kv_blocks' && kv != null) used = kv * value;
    return { kind, value, used, usedRatio: used != null ? Math.min(1, used / value) : null };
  }).filter((r) => r.value != null);
  return { rows, any: rows.length > 0 };
}

/** `dyn.spec_accept_pos{pos}` of the snapshot as [{pos, ratio}] by position (0-based as reported); empty when the worker does not report it. */
export function specAcceptModel(points) {
  const rows = (points || [])
    .filter((p) => p?.key === 'dyn.spec_accept_pos' && finite(p.value) && p.labels?.pos != null && Number.isFinite(Number(p.labels.pos)))
    .map((p) => ({ pos: Number(p.labels.pos), ratio: p.value }))
    .sort((a, b) => a.pos - b.pos);
  const overall = (points || []).find((p) => p?.key === 'dyn.spec_accept_ratio' && finite(p.value));
  return { rows, overall: overall ? overall.value : null, any: rows.length > 0 };
}

/** The newest and the window mean of one series, for the tiles. */
export const tileOf = (s) => (s ? { latest: lastOf(s.points), mean: meanOf(s.points) } : { latest: null, mean: null });

// ---------------------------------------------------------------------------------------------- the Events tab (dynamo.log)

export const LOG_LEVELS = ['ERROR', 'WARN', 'INFO', 'DEBUG', 'TRACE'];
/** A log level as one of LOG_LEVELS (case-insensitive; WARNING is WARN; anything else is INFO). */
export function normLevel(v) {
  const s = String(v ?? '')
    .trim()
    .toUpperCase();
  if (s === 'WARNING') return 'WARN';
  return LOG_LEVELS.includes(s) ? s : 'INFO';
}

/** The `dynamo.log` events as rows {key, ts, level, component, text} (text = the redacted summary, shown as text only), newest first. */
export function logRows(events) {
  return (events || [])
    .filter((e) => e && e.kind === 'dynamo.log' && finite(e.ts))
    .map((e) => ({ key: `${e.ts}|${e.summary}`, ts: e.ts, level: normLevel(e.data?.level), component: String(e.data?.component ?? '').trim() || '–', text: String(e.summary ?? '') }))
    .sort((a, b) => b.ts - a.ts);
}

/** Level and component filters, and a plain-text search (case-insensitive substring, no pattern syntax). `levels` is a Set or null (all). */
export function filterLogs(rows, { levels = null, component = '', text = '' } = {}) {
  const needle = String(text ?? '')
    .trim()
    .toLowerCase();
  return (rows || []).filter((r) => (!levels || levels.has(r.level)) && (!component || r.component === component) && (!needle || r.text.toLowerCase().includes(needle)));
}

/** How many rows each level has (all levels present, 0 allowed: a count is a count) and the components seen with their counts, busiest first. */
export function logFacets(rows) {
  const levels = Object.fromEntries(LOG_LEVELS.map((l) => [l, 0]));
  const comps = new Map();
  for (const r of rows || []) {
    levels[r.level]++;
    comps.set(r.component, (comps.get(r.component) ?? 0) + 1);
  }
  return { levels, components: [...comps.entries()].map(([name, count]) => ({ name, count })).sort((a, b) => b.count - a.count || a.name.localeCompare(b.name)) };
}

const LSEP = '|';
/** The level filter for the URL: `ERROR|WARN`; '' means all. */
export const encodeLevels = (set) => (set && set.size && set.size < LOG_LEVELS.length ? LOG_LEVELS.filter((l) => set.has(l)).join(LSEP) : '');
/** The inverse: a Set of known levels, or null (all) when empty or nothing valid. */
export function decodeLevels(s) {
  const got = String(s ?? '')
    .split(LSEP)
    .map((x) => x.trim().toUpperCase())
    .filter((x) => LOG_LEVELS.includes(x));
  return got.length ? new Set(got) : null;
}
