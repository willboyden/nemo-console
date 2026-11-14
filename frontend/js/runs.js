// Pure data layer of the run history (addendum 9.13), no DOM: shapes `GET /systems/{id}/events?limit=500` events of one
// run kind into rows, finds the numbers worth trending, groups runs for the trend chart, compares two runs (difference and
// better / worse) and reads / validates the URL-hash selection. Every value comes from the event's `data`; a field the event
// does not carry is null and shows as an en dash, NEVER as 0. Unit-tested in tests/run-history.test.mjs.
import { fmtBytes, fmtInt, fmtMs } from './format.js';
import { fmtDur, fmtStamp } from './viz/fmt.js';

const isNum = (n) => typeof n === 'number' && Number.isFinite(n);
const present = (v) => v != null && v !== '';

/** Verdict -> look: ok / bad / warn / info / muted. Colour is never the only cue (the glyph and the word go with it). */
export const VERDICT_CLASS = {
  succeeded: 'ok',
  passed: 'ok',
  pass: 'ok',
  failed: 'bad',
  fail: 'bad',
  error: 'bad',
  interrupted: 'warn',
  incomplete: 'warn',
  warn: 'warn',
  unknown: 'warn',
  running: 'info',
  skipped: 'muted',
  not_applicable: 'muted',
};
export const verdictClass = (v) => VERDICT_CLASS[String(v ?? '').toLowerCase()] ?? 'muted';
export const VERDICT_GLYPH = { ok: '✓', bad: '✕', warn: '!', info: '◌', muted: '–' };

/** A field of the flattened event data: the exact key first (`bench:gsm8k`), else a dotted path (`tier1.quality_score`). */
export function getField(obj, field) {
  if (obj == null || field == null) return undefined;
  if (Object.hasOwn(obj, field)) return obj[field];
  if (!String(field).includes('.')) return undefined;
  let cur = obj;
  for (const part of String(field).split('.')) {
    if (cur == null || typeof cur !== 'object') return undefined;
    cur = cur[part];
  }
  return cur;
}
/** A finite number or null: strings, booleans, NaN and absent fields are all "no value". */
export const numOrNull = (v) => (isNum(v) ? v : null);

/** Format a number by the descriptor's unit key; null is an en dash. */
export function fmtRunValue(v, unit = 'num') {
  if (!isNum(v)) return '–';
  switch (unit) {
    case 'ms':
      return fmtMs(v);
    case 'dur':
      return fmtDur(v);
    case 'tps':
      return `${v >= 100 ? v.toFixed(0) : v.toFixed(1)} tok/s`;
    case 'rps':
      return `${v.toFixed(2)} req/s`;
    case 'pct':
      return `${+v.toFixed(2)}%`;
    case 'ratio':
      return `${(v * 100).toFixed(1)}%`;
    case 'bytes':
      return fmtBytes(v, 2);
    case 'int':
      return fmtInt(v);
    case 'score':
      return v.toFixed(3);
    case 'score1':
      return v.toFixed(1);
    case 'signed':
      return `${v >= 0 ? '+' : ''}${v.toFixed(3)}`;
    case 'passfail':
      return v >= 1 ? 'pass' : v > 0 ? 'warn' : 'fail';
    default:
      return Number.isInteger(v) ? fmtInt(v) : String(+v.toFixed(3));
  }
}
/** A signed difference in the unit of the numbers it compares (`+12 ms`, `-0.031`, `+4.2%`). */
export function fmtDelta(d, unit = 'num') {
  if (!isNum(d)) return '–';
  const sign = d > 0 ? '+' : d < 0 ? '−' : '';
  const a = Math.abs(d);
  if (unit === 'ratio') return `${sign}${(a * 100).toFixed(1)} pp`;
  if (unit === 'passfail') return `${sign}${a}`;
  if (unit === 'signed') return `${sign}${a.toFixed(3)}`;
  return `${sign}${fmtRunValue(a, unit)}`;
}

/**
 * Event list -> rows, oldest first. A row: {id, ts, verdict, vclass, label, summary, fields, event}.
 * `desc` = {kind, id, verdict, label, flatten}; `id` / `verdict` are a field name or (fields, event) => value; `label` is a
 * list of fields (joined by ' · ') or (fields) => text. Events of
 * another kind are ignored. An id seen twice gets `#2`, `#3` ..., so a row always has its own id (the hash needs one).
 */
export function shapeRuns(events, desc) {
  const rows = [];
  const seen = new Map();
  const get = (spec, fields, ev, dflt) => {
    const v = typeof spec === 'function' ? spec(fields, ev) : spec ? getField(fields, spec) : undefined;
    return present(v) ? v : dflt;
  };
  for (const ev of Array.isArray(events) ? events : []) {
    if (!ev || (desc.kind && ev.kind !== desc.kind)) continue;
    const data = ev.data && typeof ev.data === 'object' ? ev.data : {};
    const fields = { ...data, ...(desc.flatten ? desc.flatten(data, ev) : {}) };
    const ts = isNum(ev.ts) && ev.ts > 0 ? ev.ts : null;
    let id = String(get(desc.id, fields, ev, ts != null ? `t${Math.round(ts * 1000)}` : `n${rows.length + 1}`));
    const n = (seen.get(id) || 0) + 1;
    seen.set(id, n);
    if (n > 1) id = `${id}#${n}`;
    const verdict = String(get(desc.verdict, fields, ev, 'unknown'));
    const parts = typeof desc.label === 'function' ? [desc.label(fields)].filter(present) : (desc.label || []).map((f) => getField(fields, f)).filter(present);
    rows.push({ id, ts, verdict, vclass: verdictClass(verdict), label: parts.length ? parts.join(' · ') : id, summary: typeof ev.summary === 'string' ? ev.summary : '', fields, event: ev });
  }
  // Oldest first; runs without a time keep their relative order at the front.
  return rows
    .map((r, i) => ({ r, i }))
    .sort((a, b) => (a.r.ts ?? -Infinity) - (b.r.ts ?? -Infinity) || a.i - b.i)
    .map((x) => x.r);
}

/**
 * The numbers to offer for these rows: the descriptor's own list, then every dynamic family (`{prefix, label(suffix), unit,
 * better}`, e.g. `bench:`) expanded from the fields the rows really have. Only a number that at least one row has a value for
 * is returned. Each: {field, label, unit, better: 'lower'|'higher'|null, dynamic}. `labelOf(def, suffix)` makes the text.
 */
export function resolveNumbers(rows, desc, labelOf = (def, suffix) => def.label ?? suffix) {
  const out = [];
  const has = (field) => rows.some((r) => numOrNull(getField(r.fields, field)) != null);
  for (const n of desc.numbers || []) if (has(n.field)) out.push({ field: n.field, label: labelOf(n, null), unit: n.unit || 'num', better: n.better ?? null, dynamic: false });
  for (const fam of desc.dynamic || []) {
    const fields = new Set();
    for (const r of rows) for (const k of Object.keys(r.fields)) if (k.startsWith(fam.prefix) && numOrNull(r.fields[k]) != null) fields.add(k);
    for (const f of [...fields].sort()) out.push({ field: f, label: labelOf(fam, f.slice(fam.prefix.length)), unit: fam.unit || 'num', better: fam.better ?? null, dynamic: true });
  }
  return out;
}

/** The values seen for each filter field: {field: [{value, count}]}, most frequent first (ties by value). `verdict` reads the row's verdict. */
export function enumValues(rows, fields) {
  const out = {};
  for (const f of fields) {
    const counts = new Map();
    for (const r of rows) {
      const v = f === 'verdict' ? r.verdict : getField(r.fields, f);
      if (!present(v) || typeof v === 'object') continue;
      counts.set(String(v), (counts.get(String(v)) || 0) + 1);
    }
    out[f] = [...counts.entries()].map(([value, count]) => ({ value, count })).sort((a, b) => b.count - a.count || (a.value < b.value ? -1 : 1));
  }
  return out;
}

/** Keep the rows whose fields equal every filter value (`{model: 'x', verdict: 'failed'}`); an empty filter keeps all. */
export function applyFilters(rows, filters) {
  const active = Object.entries(filters || {}).filter(([, v]) => present(v));
  if (!active.length) return rows;
  return rows.filter((r) =>
    active.every(([f, v]) => {
      const x = f === 'verdict' ? r.verdict : getField(r.fields, f);
      return present(x) && String(x) === String(v);
    }),
  );
}

/** One row's value of a number definition, null when it has none. */
export const valueOf = (row, def) => numOrNull(getField(row.fields, typeof def === 'string' ? def : def.field));

/**
 * Trend of one number across the rows: groups [{key, count, points: [{i, v, id, ts}]}] where `i` is the row's position in
 * `rows` (so the x axis is run order) and a row with no value for the number is not a point. `groupBy` is a field name ('' =
 * one group). The most frequent group comes first (it gets the first colour); a row without the group field is in '—'.
 */
export function trendGroups(rows, def, groupBy) {
  const map = new Map();
  rows.forEach((r, i) => {
    const v = valueOf(r, def);
    const g = groupBy ? getField(r.fields, groupBy) : 'all';
    const key = present(g) && typeof g !== 'object' ? String(g) : '—';
    if (!map.has(key)) map.set(key, { key, count: 0, points: [] });
    const grp = map.get(key);
    grp.count++;
    if (v != null) grp.points.push({ i, v, id: r.id, ts: r.ts });
  });
  return [...map.values()].sort((a, b) => b.count - a.count || (a.key < b.key ? -1 : a.key > b.key ? 1 : 0));
}

/**
 * Compare run A (the baseline) with run B: one row per number either run has a value for.
 * {field, label, unit, better, a, b, diff (b - a), pct (diff / |a|, null when a is 0), mark}. `mark` is 'better' / 'worse'
 * when the number says which way is better, 'same' when they are equal, 'none' when no direction is configured, and
 * 'na' when either run has no value (then diff is null: a missing value is not a zero). For job-type systems better /
 * worse are judged only when both runs succeeded (verdict class 'ok'): a failed run that stopped early is not "faster",
 * so such a row is 'none'. `opts.outcomeVerdict` (diagnostics, rail checks, skill scores: the verdict IS the measured
 * outcome) judges every pair.
 */
export function compareRuns(a, b, numbers, opts = {}) {
  const rows = [];
  const judged = opts.outcomeVerdict || (!!a && !!b && a.vclass === 'ok' && b.vclass === 'ok');
  for (const def of numbers) {
    const x = a ? valueOf(a, def) : null,
      y = b ? valueOf(b, def) : null;
    if (x == null && y == null) continue;
    const diff = x != null && y != null ? y - x : null;
    let mark = 'na';
    if (diff != null) {
      const eq = Math.abs(diff) <= 1e-9 * Math.max(1, Math.abs(x), Math.abs(y));
      if (eq) mark = 'same';
      else if (!judged) mark = 'none';
      else if (def.better === 'lower') mark = diff < 0 ? 'better' : 'worse';
      else if (def.better === 'higher') mark = diff > 0 ? 'better' : 'worse';
      else mark = 'none';
    }
    rows.push({ field: def.field, label: def.label, unit: def.unit, better: def.better, a: x, b: y, diff, pct: diff != null && x ? diff / Math.abs(x) : null, mark });
  }
  return rows;
}

/** `MM-DD HH:MM` UTC of a row, an en dash without a time. */
export const fmtRowTime = (row) => (row?.ts != null ? fmtStamp(row.ts).slice(0, 11) : '–');

// ---- URL-hash state: run= vs= metric= group= f.<field>=<value>
export const FILTER_PREFIX = 'f.';
/**
 * The state a hash query asks for, checked against what exists. `run` / `vs` must be ids of `rows`; `metric` must be one of
 * `numbers` (default: the first); `group` must be one of `desc.groupBy` or 'none' (default: the first groupBy); a filter
 * must be one of `desc.filters` with a value seen in the rows (an unknown value is dropped).
 */
export function resolveRunState(query, { rows, numbers, desc }) {
  const ids = new Set(rows.map((r) => r.id));
  const q = query || {};
  const groups = desc.groupBy || [];
  const filters = {};
  const seen = enumValues(rows, desc.filters || []);
  for (const f of desc.filters || []) {
    const v = q[`${FILTER_PREFIX}${f}`];
    if (present(v) && seen[f].some((x) => x.value === v)) filters[f] = v;
  }
  return {
    run: ids.has(q.run) ? q.run : null,
    vs: ids.has(q.vs) && q.vs !== q.run ? q.vs : null,
    metric: numbers.some((n) => n.field === q.metric) ? q.metric : (numbers[0]?.field ?? null),
    group: q.group === 'none' ? '' : groups.includes(q.group) ? q.group : (groups[0] ?? ''),
    filters,
  };
}
/** The patch that writes a state's non-default parts into the hash (a default is left out so the URL stays short). */
export function stateToQuery(state, { numbers, desc }) {
  const out = { run: state.run, vs: state.vs };
  if (state.metric && state.metric !== numbers[0]?.field) out.metric = state.metric;
  const defGroup = (desc.groupBy || [])[0] ?? '';
  if (state.group !== defGroup) out.group = state.group || 'none';
  for (const [f, v] of Object.entries(state.filters || {})) out[`${FILTER_PREFIX}${f}`] = v;
  return out;
}

/** Short axis-tick text for a number's unit (the full value goes into the tooltips and the table). */
export function fmtAxis(v, unit = 'num') {
  if (!isNum(v)) return '–';
  if (v === 0 && unit !== 'passfail') return '0';
  switch (unit) {
    case 'ratio':
      return `${Math.round(v * 100)}%`;
    case 'pct':
      return `${+v.toFixed(1)}%`;
    case 'score':
    case 'rps':
      return String(+v.toFixed(2));
    case 'signed':
      return `${v > 0 ? '+' : ''}${+v.toFixed(2)}`;
    case 'score1':
      return String(+v.toFixed(1));
    case 'bytes':
      return fmtBytes(v, 0);
    case 'passfail':
      return v >= 1 ? 'pass' : v <= 0 ? 'fail' : '';
    case 'ms':
    case 'dur':
      return fmtRunValue(v, unit);
    default:
      return Math.abs(v) >= 10000 ? `${+(v / 1000).toFixed(0)}k` : String(+v.toFixed(2));
  }
}
