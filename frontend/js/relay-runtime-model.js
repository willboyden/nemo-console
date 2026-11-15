// Pure data layer of the Relay runtime cards on the Capture tab (docs/ARCHITECTURE.md addendum 9.17), no DOM and no
// messages (every word on screen is looked up by the card modules, js/rt/resources-card.js and js/rt/diagnostics-card.js).
// Two halves:
//   * the gateway's own resource figures: which metric keys are charted together, how a call to the series route is
//     split (at most 12 keys), how a value of each unit is printed, whether the newest sample is fresh;
//   * the runtime diagnostics answer: normalised defensively, sorted, filtered. Unit-tested in tests/relay-runtime-model.test.mjs.
// Nothing here forwards or invents data: a figure the answer does not carry stays null and prints as an en dash.
import { fmtBytes, fmtInt, fmtMs, fmtCompact } from './format.js';
import { niceScale } from './viz/scale.js';

const isNum = (n) => typeof n === 'number' && Number.isFinite(n);

// ------------------------------------------------------------------------------------------------------------------ resources
export const RES_WINDOWS = ['1h', '24h', '7d'];
export const RES_DEFAULT_WINDOW = '24h';
/** The series route takes at most this many keys per call (addendum 9.13). */
export const SERIES_MAX_KEYS = 12;
/** A sample is fresh while it is no older than this many sampling intervals (addendum 9.17). */
export const FRESH_INTERVALS = 3;

/**
 * The charts, in page order. A card is one chart card; a panel is one chart inside it (they share a time axis); a line is
 * one metric key. `unit` picks the number format (fmtResource). A card is never split across two calls to the series route.
 */
export const RES_CARDS = [
  { id: 'cpu', panels: [{ id: 'cpu', unit: 'cores', lines: ['relay.gw_cpu'] }] },
  { id: 'memory', panels: [{ id: 'memory', unit: 'B', lines: ['relay.gw_rss', 'relay.gw_mem_accounted'] }] },
  { id: 'handles', panels: [{ id: 'handles', unit: 'n', lines: ['relay.gw_threads', 'relay.gw_fds'] }] },
  { id: 'disk', panels: [{ id: 'disk', unit: 'B/s', lines: ['relay.gw_disk_read', 'relay.gw_disk_write'] }] },
  {
    id: 'fs',
    panels: [
      { id: 'free', unit: 'B', lines: ['relay.atof_fs_free'] },
      { id: 'used', unit: 'ratio', lines: ['relay.atof_fs_used'] },
    ],
  },
  {
    id: 'pressure',
    panels: [
      { id: 'stall', unit: 'ms/s', lines: ['relay.gw_cpu_pressure', 'relay.gw_mem_pressure'] },
      { id: 'limits', unit: 'n', lines: ['relay.gw_limit_events'] },
    ],
  },
  {
    id: 'routing',
    wide: true,
    panels: [
      { id: 'rps', unit: 'req/s', lines: ['relay.route_rps', 'relay.route_failures'] },
      { id: 'overhead', unit: 'ms', lines: ['relay.route_overhead_p95'] },
      { id: 'tokens', unit: 'tok/s', lines: ['relay.route_tokens'] },
    ],
  },
];

/** The latest-values strip: one tile per entry, from the metrics snapshot. `sub` is a second figure shown under the first. */
export const RES_TILES = [
  { id: 'cpu', key: 'relay.gw_cpu', unit: 'cores' },
  { id: 'memory', key: 'relay.gw_rss', unit: 'B', sub: { key: 'relay.gw_mem_accounted', unit: 'B' } },
  { id: 'handles', key: 'relay.gw_threads', unit: 'n', sub: { key: 'relay.gw_fds', unit: 'n' } },
  { id: 'disk', key: 'relay.gw_disk_read', unit: 'B/s', sub: { key: 'relay.gw_disk_write', unit: 'B/s' } },
  { id: 'fs', key: 'relay.atof_fs_free', unit: 'B', sub: { key: 'relay.atof_fs_used', unit: 'ratio' } },
  { id: 'routing', key: 'relay.route_rps', unit: 'req/s', sub: { key: 'relay.route_failures', unit: 'req/s' } },
  { id: 'overhead', key: 'relay.route_overhead_p95', unit: 'ms' },
  { id: 'tokens', key: 'relay.route_tokens', unit: 'tok/s' },
  { id: 'pressure', key: 'relay.gw_cpu_pressure', unit: 'ms/s', sub: { key: 'relay.gw_mem_pressure', unit: 'ms/s' } },
  { id: 'limits', key: 'relay.gw_limit_events', unit: 'n', hotAbove: 0 },
];
/** Every key the card draws or reads, once each, in page order. (There is no age key: how fresh the samples are comes from the capture answer, see sampleAge.) */
export const RESOURCE_KEYS = [...new Set([...RES_CARDS.flatMap((c) => c.panels.flatMap((p) => p.lines)), ...RES_TILES.flatMap((x) => [x.key, x.sub?.key]).filter(Boolean)])];

/** The keys one card needs, in order. */
export const cardKeys = (card) => card.panels.flatMap((p) => p.lines);

/**
 * Split the cards over calls to the series route: whole cards in page order, at most `max` keys a call (a card is never split,
 * so one answer holds everything a card draws). Returns [{keys, cards: [card id]}]. A card with more keys than `max` is refused.
 */
export function planCalls(cards = RES_CARDS, max = SERIES_MAX_KEYS) {
  const out = [];
  for (const card of cards) {
    const keys = cardKeys(card);
    if (keys.length > max) throw new Error(`card ${card.id} needs ${keys.length} keys, a call takes at most ${max}`);
    const last = out[out.length - 1];
    if (last && last.keys.length + keys.length <= max) {
      last.keys.push(...keys);
      last.cards.push(card.id);
    } else out.push({ keys: [...keys], cards: [card.id] });
  }
  return out;
}

/** The window of the card: only 1h, 24h and 7d are offered; anything else is the default. */
export const resWindow = (w) => (RES_WINDOWS.includes(w) ? w : RES_DEFAULT_WINDOW);

/** Decimals that tell values `step` apart: none for a step of 1 or more, then one more per decade below (1-2-5 steps: 0.5 needs 1, 0.02 needs 2). */
export const decimalsForStep = (step) => (isNum(step) && step > 0 && step < 1 ? Math.min(6, Math.max(0, Math.ceil(-Math.log10(step) - 1e-9))) : 0);
const fixed = (v, d) => String(+v.toFixed(d));
/** Two significant digits, trailing zeros dropped: 0.00051 -> 0.00051, 0.5 -> 0.5. Used where a value can be far below 1. */
const sig2 = (v) => String(+v.toPrecision(2));
const BYTE_UNITS = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];

/**
 * How a value of `unit` reads (tiles, tooltips, data tables). n = a plain count, cores = CPU cores (below 0.1 in millicores),
 * ms/s = milliseconds stalled per second, B, B/s, ratio (0..1), req/s, ms, tok/s. Small values keep two significant digits instead of
 * rounding to zero. A value that is not a finite number is an en dash, never a 0.
 */
export function fmtResource(unit, v) {
  if (!isNum(v)) return '–';
  // a zero is a zero in every unit ("0 req/s", never "–" and never "0.00 cores"): a quiet gateway reports 0, not nothing
  if (v === 0) return unit === 'n' ? '0' : unit === 'ratio' ? '0%' : `0 ${unit === 'cores' ? 'cores' : unit}`;
  switch (unit) {
    case 'cores':
      return v < 0.1 ? `${sig2(v * 1000)} millicores` : `${v.toFixed(2)} cores`;
    case 'B':
    case 'B/s': {
      const per = unit === 'B/s' ? '/s' : '';
      return v < 10 && !Number.isInteger(v) ? `${sig2(v)} B${per}` : `${fmtBytes(v)}${per}`;
    }
    case 'ratio': {
      const pct = v * 100;
      return `${pct < 0.1 ? sig2(pct) : pct.toFixed(pct < 10 ? 1 : 0)}%`;
    }
    case 'ms/s':
      return `${v < 0.1 ? sig2(v) : +v.toFixed(v < 10 ? 2 : 1)} ms/s`;
    case 'req/s':
      return `${v >= 100 ? v.toFixed(0) : sig2(v)} req/s`; // two significant digits below 100 (so 0.00041 stays), normal rounding from 100 up
    case 'tok/s':
      return `${v < 1 ? sig2(v) : v >= 100 ? v.toFixed(0) : v.toFixed(1)} tok/s`;
    case 'ms':
      return v < 0.1 ? `${sig2(v)} ms` : fmtMs(v);
    default:
      return Math.abs(v) >= 10000 ? fmtCompact(v) : fmtInt(v);
  }
}

/**
 * The step a chart's y axis will use for these lines: the same rule the chart draws with (a nice 1-2-5 step over 0..max, or min..max when
 * there are values below zero, 4 ticks), so a formatter can print labels the right precision. `lines` = [{segments: [[[ts, v, max]]]}].
 */
export function axisStep(lines) {
  const ys = (lines || []).flatMap((l) => (l?.segments || []).flatMap((seg) => seg.map((p) => p?.[1]))).filter(isNum);
  if (!ys.length) return 1;
  return niceScale(Math.min(0, ...ys), Math.max(...ys), 4).step;
}

/**
 * A y-axis label formatter for `unit` on an axis whose ticks are `step` apart: every tick on the axis reads in one unit and with enough
 * decimals that neighbouring ticks differ (0, 0.2 m, 0.4 m, 0.6 m for a CPU axis of 0..0.0006 cores; never 0, 0, 0, 0).
 */
export function axisFormatter(unit, step) {
  const st = isNum(step) && step > 0 ? step : 1;
  const bad = (v) => !isNum(v);
  switch (unit) {
    case 'cores': {
      if (st >= 0.05) return (v) => (bad(v) ? '–' : fixed(v, decimalsForStep(st)));
      const d = decimalsForStep(st * 1000);
      return (v) => (bad(v) ? '–' : `${fixed(v * 1000, d)} m`);
    }
    case 'B':
    case 'B/s': {
      let e = 0;
      while (e < BYTE_UNITS.length - 1 && st / 1024 ** (e + 1) >= 0.5) e++;
      const d = decimalsForStep(st / 1024 ** e);
      const per = unit === 'B/s' ? '/s' : '';
      return (v) => (bad(v) ? '–' : `${fixed(v / 1024 ** e, d)} ${BYTE_UNITS[e]}${per}`);
    }
    case 'ratio': {
      const d = decimalsForStep(st * 100);
      return (v) => (bad(v) ? '–' : `${fixed(v * 100, d)}%`);
    }
    case 'ms':
      return (v) => (bad(v) ? '–' : st < 1 ? `${fixed(v, decimalsForStep(st))} ms` : fmtMs(v));
    default: {
      const d = decimalsForStep(st);
      return (v) => (bad(v) ? '–' : Math.abs(v) >= 10000 ? fmtCompact(v) : fixed(v, d));
    }
  }
}

/**
 * Seconds since the gateway's newest resource sample: `now` minus the time of the newest `nemo.relay.resource_metrics` mark in its event
 * files, which the capture answer gives as the `resource_metrics` row's `observed.last_ts` (epoch seconds). null when none was seen
 * (null, not a number, or a time that is not before the year 3000); a stamp a little in the future (clock skew) counts as 0.
 */
export function sampleAge(lastTs, nowS) {
  if (!isNum(lastTs) || !isNum(nowS) || lastTs <= 0 || lastTs > 32503680000) return null;
  return Math.max(0, nowS - lastTs);
}

/**
 * The newest sample's state from its age (sampleAge) and the sampling interval (`resources.interval_s`): `fresh` up to
 * FRESH_INTERVALS x interval, `stale` beyond it, `unknown` when there is no age (no sample seen yet) or the interval is not known
 * (then the age is still returned, but no verdict is given).
 */
export function freshness(ageS, intervalS) {
  const age = isNum(ageS) && ageS >= 0 ? ageS : null;
  const every = isNum(intervalS) && intervalS > 0 ? intervalS : null;
  if (age == null) return { state: 'unknown', ageS: null, limitS: every == null ? null : every * FRESH_INTERVALS };
  if (every == null) return { state: 'unknown', ageS: age, limitS: null };
  const limitS = every * FRESH_INTERVALS;
  return { state: age <= limitS ? 'fresh' : 'stale', ageS: age, limitS };
}

/** `{key: value}` of the gateway-resource points of a metrics snapshot: the first finite, non-negative value per key (labels are never used). */
export function latestValues(snapshot) {
  const want = new Set(RESOURCE_KEYS);
  const out = {};
  for (const p of snapshot?.points || []) {
    if (!want.has(p?.key) || p.key in out) continue;
    if (isNum(p.value) && p.value >= 0) out[p.key] = p.value;
  }
  return out;
}

/** Whether the snapshot carries any gateway-resource figure at all (nothing sampled yet reads as no). */
export const hasResourceFigures = (latest) => RESOURCE_KEYS.some((k) => isNum(latest?.[k]));

/** A tile's text: its main figure and, if it has one, the second. `hot` marks a count above its threshold (limit events). */
export function tileModel(tile, latest) {
  const v = latest?.[tile.key];
  const sub = tile.sub ? latest?.[tile.sub.key] : undefined;
  return {
    id: tile.id,
    value: fmtResource(tile.unit, v),
    sub: tile.sub ? fmtResource(tile.sub.unit, sub) : null,
    hot: tile.hotAbove != null && isNum(v) && v > tile.hotAbove,
    known: isNum(v),
  };
}

// ------------------------------------------------------------------------------------------------------------------ diagnostics
export const DIAG_REASONS = ['disabled', 'unreachable', 'unsupported_version', 'error'];
const ID_RE = /^[A-Za-z0-9_.:-]{1,96}$/;
const RFC3339 = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2}):(\d{2})(?:\.(\d+))?(Z|[+-]\d{2}:\d{2})$/;
const count = (n) => (isNum(n) && n >= 0 ? Math.min(n, 2 ** 53) : null);
const id = (s) => (typeof s === 'string' && ID_RE.test(s) ? s : 'other');

/** Epoch seconds (fractions kept to the millisecond) from an RFC 3339 stamp as Relay prints it; null for anything else. */
export function parseStamp(s) {
  const m = RFC3339.exec(String(s ?? ''));
  if (!m) return null;
  const ms = Date.parse(`${m[1]}-${m[2]}-${m[3]}T${m[4]}:${m[5]}:${m[6]}.${(m[7] || '0').padEnd(3, '0').slice(0, 3)}${m[8]}`);
  return Number.isFinite(ms) ? ms / 1000 : null;
}

/**
 * The route's answer as the card draws it. Never throws; a missing piece is null or empty.
 * `reason` is one of DIAG_REASONS or null; with a reason (or `available: false` without one: `error`) everything else is empty.
 */
export function normalizeDiagnostics(a) {
  const o = a && typeof a === 'object' ? a : {};
  const reason = DIAG_REASONS.includes(o.reason) ? o.reason : o.available === false || o.available == null ? 'error' : null;
  if (reason)
    return { available: false, reason, version: null, capturedAt: null, elapsedS: null, totals: null, lastFailureAt: null, components: [], events: [], loss: [], retention: null, memory: null };
  const t = o.totals && typeof o.totals === 'object' ? o.totals : {};
  const components = (Array.isArray(o.components) ? o.components : [])
    .filter((c) => c && typeof c.key === 'string')
    .map((c) => ({
      key: c.key,
      instances: count(c.instances),
      successes: count(c.successes),
      failures: count(c.failures),
      occurrences: count(c.occurrences),
      lastFailureAt: parseStamp(c.last_failure_at),
    }));
  const events = (Array.isArray(o.events) ? o.events : [])
    .filter((e) => e && typeof e === 'object')
    .map((e) => ({
      seq: count(e.seq),
      at: parseStamp(e.at),
      component: id(e.component),
      code: id(e.code),
      failureClass: id(e.failure_class),
      severity: id(e.severity),
      outcome: id(e.outcome),
      count: count(e.count),
    }));
  const r = o.retention && typeof o.retention === 'object' ? o.retention : null;
  const m = o.memory && typeof o.memory === 'object' ? o.memory : null;
  return {
    available: true,
    reason: null,
    version: typeof o.relay_version === 'string' ? o.relay_version : null,
    capturedAt: parseStamp(o.captured_at),
    elapsedS: isNum(o.elapsed_s) && o.elapsed_s >= 0 ? o.elapsed_s : null,
    totals: { recorded: count(t.recorded), successes: count(t.successes), failures: count(t.failures), unspecified: count(t.unspecified), evictions: count(t.evictions) },
    lastFailureAt: parseStamp(o.last_failure_at),
    components, // in the order the server gave them (failures first, then by key); nothing here re-sorts them
    events,
    loss: lossRows(o.loss),
    retention: r ? { retained: count(r.events_retained), oldestSeq: count(r.oldest_seq), latestSeq: count(r.latest_seq), truncated: typeof r.truncated === 'boolean' ? r.truncated : null } : null,
    memory: m
      ? {
          reservedBytes: count(m.reserved_bytes),
          events: count(m.events),
          eventCapacity: count(m.event_capacity),
          detailBytes: count(m.detail_bytes),
          detailCapacityBytes: count(m.detail_capacity_bytes),
        }
      : null,
  };
}

/** A name cut after each dot, underscore or dash, so a long component name can wrap at a sensible place; the parts joined are the name. */
export const breakParts = (name) => String(name ?? '').split(/(?<=[._-])/);

/**
 * The bars of "failures by component": components with at least one failure, most failures first (equal counts keep the order the
 * server gave them), at most `max`; `share` is of the largest.
 */
export function failureBars(components, max = 8) {
  const rows = components
    .filter((c) => (c.failures ?? 0) > 0)
    .sort((a, b) => b.failures - a.failures)
    .slice(0, max);
  const top = rows[0]?.failures ?? 0;
  return {
    rows: rows.map((c) => ({ key: c.key, failures: c.failures, share: top > 0 ? c.failures / top : 0 })),
    top,
    shown: rows.length,
    total: components.filter((c) => (c.failures ?? 0) > 0).length,
  };
}

/** The outcomes present in the events, in the order they first appear, each with its count: the options of the filter. */
export function outcomeOptions(events) {
  const seen = new Map();
  for (const e of events || []) seen.set(e.outcome, (seen.get(e.outcome) ?? 0) + 1);
  return [...seen.entries()].map(([value, n]) => ({ value, count: n }));
}

/**
 * Whether events beyond those gathered exist (the console follows the gateway's continuation pages up to a cap): the backend's
 * `retention.truncated`, and, when an older backend does not say, more retained events than events listed.
 */
export const eventsTruncated = (retention, gathered) =>
  retention?.truncated === true ? true : retention?.truncated == null && isNum(retention?.retained) && isNum(gathered) && retention.retained > gathered;

/** The events with `outcome` (`all` keeps every one). The answer's order (newest first) is kept. */
export const filterEvents = (events, outcome) => (outcome && outcome !== 'all' ? (events || []).filter((e) => e.outcome === outcome) : [...(events || [])]);

/** `loss` ({name: n}) as rows, the non-zero ones first (largest first), then the zeros by name; a value that is not a count is dropped. */
export function lossRows(loss) {
  const rows = Object.entries(loss && typeof loss === 'object' && !Array.isArray(loss) ? loss : {})
    .filter(([name]) => ID_RE.test(name))
    .map(([name, v]) => ({ name, value: count(v === true ? 1 : v) }))
    .filter((r) => r.value != null)
    .map((r) => ({ ...r, nonZero: r.value > 0 }));
  return rows.sort((a, b) => Number(b.nonZero) - Number(a.nonZero) || b.value - a.value || (a.name < b.name ? -1 : a.name > b.name ? 1 : 0));
}

/**
 * The gauges of the memory meter: event slots used of the slots there are, and the detail pool used of its capacity. Each is
 * {id, used, capacity, ratio (0..1, clamped; null when the capacity is not known)}. Relay reserves `reservedBytes` for the whole
 * recorder; the answer carries no capacity for that total, so it is a figure, not a gauge.
 */
export function memoryGauges(memory) {
  if (!memory) return [];
  const gauge = (gid, used, capacity) => ({ id: gid, used, capacity, ratio: used != null && capacity != null && capacity > 0 ? Math.min(1, used / capacity) : null });
  return [gauge('events', memory.events, memory.eventCapacity), gauge('detail', memory.detailBytes, memory.detailCapacityBytes)].filter((g) => g.used != null);
}

/** Badge kind (ok | warn | bad | info | muted) of an outcome and of a severity: never colour alone, the card prints the word too. */
const OUTCOME_KIND = new Map([
  ['failure', 'bad'],
  ['success', 'ok'],
]);
const SEVERITY_KIND = new Map([
  ['critical', 'bad'],
  ['fatal', 'bad'],
  ['error', 'bad'],
  ['warn', 'warn'],
  ['warning', 'warn'],
  ['info', 'info'],
  ['notice', 'info'],
]);
const OUTCOME_GLYPH = new Map([
  ['failure', '✕'],
  ['success', '✓'],
  ['unspecified', '–'],
]);
export const outcomeKind = (o) => OUTCOME_KIND.get(o) ?? 'muted';
/** The glyph beside an outcome's word (colour is never the only cue). */
export const outcomeGlyph = (o) => OUTCOME_GLYPH.get(o) ?? '·';
export const severityKind = (s) => SEVERITY_KIND.get(s) ?? 'muted';
