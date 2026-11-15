// MOCK of GET /api/v1/systems/{id}/metrics/series (docs/ARCHITECTURE.md addendum 9.13). MOCK-ONLY: the numbers are invented
// so the charts can be built and screenshotted; the route's SHAPE and rules follow the contract: keys (1..12, each matching
// KEY_RE, a bare key also matches its label variants), window 15m|1h|24h|7d, split 0|1, buckets (15m raw points at the poll
// interval with max = value; 1h and 24h = 1-minute buckets; 7d = 10-minute buckets), timestamps = bucket starts, points in
// time order, a key with no data is a series with `points: []`, at most 32 series per key (the rest dropped, truncated: true),
// 422 invalid_request for a bad window, > 12 keys, an empty or malformed key or split not 0/1.
// Values are a deterministic function of (key, labels, timestamp), so two requests agree and a chart does not jump on refresh.

import { dcgmCounterRate, SY_COUNTER_RATE, DYN_COUNTER_RATE } from './mock-wave2.mjs';

export const WINDOW_SPAN_S = { '15m': 900, '1h': 3600, '24h': 86400, '7d': 604800 };
export const MAX_KEYS = 12;
export const MAX_SERIES_PER_KEY = 32;
// The backend's metrics_keys.KEY_RE (api/observe.rs).
export const KEY_RE = /^[a-z][a-z0-9_.]{0,63}(?:\{[A-Za-z0-9_.:/\-=,]{1,200}\})?$/;

const hash32 = (s) => {
  let h = 2166136261;
  for (let i = 0; i < s.length; i++) h = Math.imul(h ^ s.charCodeAt(i), 16777619);
  return h >>> 0;
};
const unit = (s) => (hash32(s) % 10007) / 10007; // 0..1, stable per string
const labelId = (labels) =>
  Object.entries(labels || {})
    .sort(([a], [b]) => (a < b ? -1 : 1))
    .map(([k, v]) => `${k}=${v}`)
    .join(',');
// The snapshot carries a little random jitter; the first value seen for a series is kept as its nominal level, so two requests
// (and two refreshes of a chart) agree.
const NOMINAL = new Map();
const nominal = (key, value) => {
  if (!NOMINAL.has(key)) NOMINAL.set(key, value);
  return NOMINAL.get(key);
};

/** A request's query -> {keys, window, split} or {error}. */
export function parseSeriesQuery(sp) {
  const raw = sp.get('keys');
  if (raw == null || raw === '') return { error: 'keys: 1 to 12 comma-separated metric keys are required' };
  const keys = raw.split(',').map((k) => k.trim());
  // A key may carry a label selector with commas inside braces: re-join the pieces of `key{a=b,c=d}`.
  const joined = [];
  for (const part of keys) {
    if (joined.length && /\{[^}]*$/.test(joined[joined.length - 1])) joined[joined.length - 1] += `,${part}`;
    else joined.push(part);
  }
  if (joined.length > MAX_KEYS) return { error: `keys: at most ${MAX_KEYS} keys, got ${joined.length}` };
  for (const k of joined) if (!KEY_RE.test(k)) return { error: `keys: ${JSON.stringify(k)} is not a canonical metric key` };
  const window = sp.get('window') ?? '1h';
  if (!(window in WINDOW_SPAN_S)) return { error: `window: must be one of ${Object.keys(WINDOW_SPAN_S).join(', ')}` };
  const split = sp.get('split') ?? '0';
  if (split !== '0' && split !== '1') return { error: 'split: must be 0 or 1' };
  return { keys: joined, window, split: split === '1' };
}

/** `key{a=b,c=d}` -> {base, sel: {a: 'b', c: 'd'}} */
export function parseKey(key) {
  const m = /^([^{]+)(?:\{(.*)\})?$/.exec(key);
  const sel = {};
  if (m?.[2]) for (const pair of m[2].split(',')) sel[pair.slice(0, pair.indexOf('='))] = pair.slice(pair.indexOf('=') + 1);
  return { base: m[1], sel };
}

// Counters whose key does not end in _total (the adapters report them as lifetime counts), so they are drawn as rates too.
const COUNTER_KEYS = new Set(['sy.model.calls', 'dyn.tokens']);
const isCounter = (base) => /_total$/.test(base) || COUNTER_KEYS.has(base);
// Gauges that do not wobble: a capacity, a limit, the largest context a model was given.
const STEADY = /\.(capacity|max_context_tokens|power_limit_w)$/;
// Per-second rate of each counter (the mock's own picture of a busy lab); anything else starts at a small default.
const COUNTER_RATE = {
  'flare.generated_total': 42,
  'flare.prefill_total': 210,
  'flare.requests_completed_total': 0.04, // slow: shown per minute
  'flare.requests_failed_total': 0.002,
  'flare.cache_reused_total': 95,
  'sy.requests_total': 0.4,
  'sy.errors_total': 0.003,
  'dyn.requests_total': 0.2,
  'dcgm.pcie_replay_total': 0,
  'dcgm.xid_total': 0,
  // addendum 9.14: a rate may depend on the series' labels (a function of them)
  'dcgm.energy_mj_total': (l) => dcgmCounterRate('dcgm.energy_mj_total', l),
  'dcgm.violation_us_total': (l) => dcgmCounterRate('dcgm.violation_us_total', l),
  ...SY_COUNTER_RATE,
  ...DYN_COUNTER_RATE,
};
/** The console / engine restarted at these offsets before "now": a counter starts again from zero there. */
const RESETS_S = [40 * 60, 30 * 3600];

function counterAt(base, id, ts, now, labels = {}) {
  const r = COUNTER_RATE[base];
  const rate0 = (typeof r === 'function' ? r(labels || {}) : r) ?? 0.05;
  const phi = unit(`${base}|${id}|c`) * 6.283;
  const F = (u) => rate0 * (u - 300 * Math.cos(u / 600 + phi)); // integral of rate0 * (1 + 0.5 sin(u / 600 + phi))
  let last = now - 9 * 86400;
  for (const r of RESETS_S) if (now - r <= ts && now - r > last) last = now - r;
  return F(ts) - F(last);
}

function gaugeAt(base, id, ts, cur, unitName) {
  if (cur === 0 || /(^|\.)(up|gateway_up|worker_up)$/.test(base) || /_up$/.test(base) || STEADY.test(base)) return cur;
  const phi = unit(`${base}|${id}|g`) * 6.283;
  const amp = unitName === 'ratio' ? 0.2 : unitName === 'B' ? 0.03 : 0.12;
  // A slow daily swing, a medium wave of a few hours and a small fast ripple: readable in every window, never aliased in the 7d one.
  const day = 86400 * (0.85 + unit(`${base}|d`) * 0.3);
  const mid = 3 * 3600 * (1 + unit(`${base}|m`) * 1.5);
  const fast = 1500 + unit(`${base}|f`) * 1500;
  let v = cur * (1 + amp * (0.6 * Math.sin(ts / (day / 6.283) + phi) + 0.3 * Math.sin(ts / (mid / 6.283) + 2 * phi) + 0.1 * Math.sin(ts / (fast / 6.283) + 3 * phi)));
  v = Math.max(0, v);
  return unitName === 'ratio' ? Math.min(1, v) : v;
}

/** Holes in the data (nothing was sampled): they show as gaps in the 24h and 7d charts, never in the last hour. */
const holeAt = (ts, now) => (ts < now - 4.5 * 3600 && ts >= now - 5 * 3600) || (ts < now - 49 * 3600 && ts >= now - 50 * 3600);

/**
 * @param points    the system's current snapshot points ({key, value, unit, labels}); they say which label variants exist
 * @param now       epoch seconds;  @param pollS the poll interval (the 15m step)
 * Returns the route's answer.
 */
export function buildSeries({ keys, window, split, points, now, pollS = 2 }) {
  const span = WINDOW_SPAN_S[window];
  const step = window === '15m' ? pollS : window === '7d' ? 600 : 60;
  const to = Math.floor(now / step) * step;
  const from = to - span + step;
  const grid = [];
  for (let ts = from; ts <= to; ts += step) grid.push(ts);
  let truncated = false;
  const series = [];
  for (const key of keys) {
    const { base, sel } = parseKey(key);
    const variants = points.filter((p) => p.key === base && Object.entries(sel).every(([k, v]) => p.labels?.[k] === v));
    const sample = (p) =>
      grid
        .filter((ts) => !holeAt(ts, now))
        .map((ts) => {
          const id = labelId(p.labels);
          const avg = isCounter(base) ? counterAt(base, id, ts, now, p.labels) : gaugeAt(base, id, ts, nominal(`${base}|${id}`, p.value), p.unit);
          const spread = window === '15m' || isCounter(base) ? 0 : 0.05 + 0.1 * Math.abs(Math.sin(ts * 1.7 + unit(`${base}|${id}`) * 6.28));
          let max = avg * (1 + spread);
          if (p.unit === 'ratio') max = Math.min(1, max);
          return [ts, +avg.toFixed(4), +max.toFixed(4)];
        });
    if (!variants.length) {
      series.push({ key, label: null, points: [] });
    } else if (!split) {
      const per = variants.map(sample);
      series.push({ key, label: null, points: per[0].map((p, i) => [p[0], +per.reduce((a, v) => a + v[i][1], 0).toFixed(4), +per.reduce((a, v) => a + v[i][2], 0).toFixed(4)]) });
    } else {
      const ordered = [...variants].sort((a, b) => (labelId(a.labels) < labelId(b.labels) ? -1 : 1));
      if (ordered.length > MAX_SERIES_PER_KEY) truncated = true;
      for (const p of ordered.slice(0, MAX_SERIES_PER_KEY)) series.push({ key, label: Object.keys(p.labels || {}).length ? { ...p.labels } : null, points: sample(p) });
    }
  }
  return { window, step_s: step, from, to, series, truncated };
}
