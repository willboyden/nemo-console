// Pure shaping of the Audit page's new cards and filters (addendum 9.16): `GET /audit/summary?window=24h|7d|30d` as actions per day stacked by system, an actor x system matrix and an
// error timeline, and the URL-state filters of the log (`since`, `until`, `actor`, `status`, `system`) as the query `GET /audit` takes. No DOM, no i18n. The summary is aggregate counts
// only (no parameters, no paths). A day with no audited action is a real zero; a figure the answer does not carry is null. Unit-tested in tests/audit-model.test.mjs.
const isNum = (n) => typeof n === 'number' && Number.isFinite(n);
const DAY = 86400;

export const SUMMARY_WINDOWS = ['24h', '7d', '30d'];
export const DEFAULT_SUMMARY_WINDOW = '7d';
export const WINDOW_S = { '24h': DAY, '7d': 7 * DAY, '30d': 30 * DAY, '1h': 3600 };
/** The most systems named in the stack (the rest are summed as "other": a stacked bar has six colours) and the most actors in the matrix. */
export const MAX_STACK_SYSTEMS = 5;
export const MAX_ACTORS = 12;
export const LOG_RANGES = ['any', '1h', '24h', '7d', '30d', 'custom'];
export const LOG_STATUSES = ['', 'ok', 'error'];
export const LOG_LIMIT = 200;

export const summaryWindow = (v) => (SUMMARY_WINDOWS.includes(v) ? v : DEFAULT_SUMMARY_WINDOW);

/** A `day` of by_day: "YYYY-MM-DD" (UTC midnight), epoch seconds, or epoch milliseconds -> epoch seconds of that day's start; null for anything else. */
export function dayStart(day) {
  if (typeof day === 'string') {
    const m = /^(\d{4})-(\d{2})-(\d{2})/.exec(day);
    if (!m) return null;
    const t = Date.UTC(Number(m[1]), Number(m[2]) - 1, Number(m[3])) / 1000;
    return Number.isFinite(t) ? t : null;
  }
  if (!isNum(day) || day <= 0) return null;
  const s = day > 1e11 ? day / 1000 : day;
  return Math.floor(s / DAY) * DAY;
}

const countOf = (v) => (isNum(v) && v >= 0 ? v : null);
const nameOf = (v, dflt) => (typeof v === 'string' && v !== '' ? v : dflt);
/** The summary's `system: null` (a call that belongs to no system: a settings save) and `actor: null` get their own ids, which no real system or key alias can have. */
export const CONSOLE_SYSTEM = '~console';
export const NO_ACTOR = '~none';

/**
 * The summary answer as the three cards' data.
 * `days`: {ts (day starts, oldest first, every day between the first and the last), bucketS: 86400, parts: [system names, busiest first, at most MAX_STACK_SYSTEMS + "other"], series: [{key, values}]};
 * `systems`: every system seen, busiest first, with its total; `actors`: every actor seen with its total (busiest first), `matrix(actor, system)` -> count | null (no call by that actor on that system);
 * `errors`: {ts, bucketS, values, total, peak} on a grid over the answer's own window (`from`, `to`, `bucket_s`; else the window ending `now`); `deniedTotal`: the answer's number or null; `total`: all actions; `truncated`: the answer says
 * its actor list was cut at the cap; `dropped`: rows that did not parse. A `system: null` row is the console's own (settings) calls (CONSOLE_SYSTEM), an `actor: null` row has no key alias (NO_ACTOR).
 */
export function shapeSummary(answer, { now = Date.now() / 1000, maxSystems = MAX_STACK_SYSTEMS, maxActors = MAX_ACTORS } = {}) {
  const a = answer && typeof answer === 'object' ? answer : {};
  let dropped = 0;
  const win = summaryWindow(a.window);
  // ---- by day
  const byDay = new Map(); // dayStart -> Map(system -> count)
  const sysTotal = new Map();
  for (const r of Array.isArray(a.by_day) ? a.by_day : []) {
    const d = r ? dayStart(r.day) : null,
      c = r ? countOf(r.count) : null;
    if (d == null || c == null) {
      dropped++;
      continue;
    }
    const sys = r.system === null ? CONSOLE_SYSTEM : nameOf(r.system, 'other');
    if (!byDay.has(d)) byDay.set(d, new Map());
    byDay.get(d).set(sys, (byDay.get(d).get(sys) || 0) + c);
    sysTotal.set(sys, (sysTotal.get(sys) || 0) + c);
  }
  const systems = [...sysTotal.entries()].sort((x, y) => y[1] - x[1] || (x[0] < y[0] ? -1 : 1)).map(([system, total]) => ({ system, total }));
  const named = systems.length > maxSystems + 1 ? systems.slice(0, maxSystems).map((s) => s.system) : systems.map((s) => s.system);
  const partOf = (sys) => (named.includes(sys) ? sys : 'other');
  const hasOther = systems.some((s) => !named.includes(s.system));
  const parts = hasOther ? [...named.filter((s) => s !== 'other'), 'other'] : named;
  const dayKeys = [...byDay.keys()].sort((x, y) => x - y);
  const ts = [];
  if (dayKeys.length) for (let d = dayKeys[0]; d <= dayKeys.at(-1) && ts.length < 400; d += DAY) ts.push(d);
  const series = parts.map((key) => ({
    key,
    values: ts.map((d) => {
      const m = byDay.get(d);
      let v = 0;
      if (m) for (const [sys, c] of m) if (partOf(sys) === key) v += c;
      return v;
    }),
  }));
  const total = [...sysTotal.values()].reduce((x, y) => x + y, 0);
  // ---- actor x system
  const am = new Map(); // actor -> Map(system -> count)
  const actorTotal = new Map();
  for (const r of Array.isArray(a.by_actor) ? a.by_actor : []) {
    const c = r ? countOf(r.count) : null;
    if (c == null) {
      dropped++;
      continue;
    }
    const actor = r.actor === null ? NO_ACTOR : nameOf(r.actor, NO_ACTOR),
      sys = r.system === null ? CONSOLE_SYSTEM : nameOf(r.system, 'other');
    if (!am.has(actor)) am.set(actor, new Map());
    am.get(actor).set(sys, (am.get(actor).get(sys) || 0) + c);
    actorTotal.set(actor, (actorTotal.get(actor) || 0) + c);
  }
  const actorsAll = [...actorTotal.entries()].sort((x, y) => y[1] - x[1] || (x[0] < y[0] ? -1 : 1)).map(([actor, n]) => ({ actor, total: n }));
  const actors = actorsAll.slice(0, maxActors);
  const matrixSystems = [...new Set([...systems.map((s) => s.system), ...[...am.values()].flatMap((m) => [...m.keys()])])];
  const matrixCols = matrixSystems.sort((x, y) => (sysTotal.get(y) ?? 0) - (sysTotal.get(x) ?? 0) || (x < y ? -1 : 1));
  const matrix = (actor, sys) => (am.get(actor)?.has(sys) ? am.get(actor).get(sys) : null);
  const matrixMax = Math.max(0, ...actors.flatMap((x) => matrixCols.map((s) => matrix(x.actor, s) ?? 0)));
  // ---- errors over the window
  const errRows = (Array.isArray(a.errors) ? a.errors : [])
    .map((r) => ({ ts: r && isNum(r.ts_bucket) ? r.ts_bucket : null, count: r ? countOf(r.count) : null }))
    .filter((r) => r.ts != null && r.count != null);
  const errors = errorGrid(errRows, win, now, { from: a.from, to: a.to, bucketS: a.bucket_s });
  return {
    window: win,
    total,
    dropped,
    days: { ts, bucketS: DAY, parts, series },
    systems,
    actors,
    actorsHidden: actorsAll.length - actors.length,
    matrixCols,
    matrix,
    matrixMax,
    errors,
    deniedTotal: countOf(a.denied_total),
    truncated: a.truncated === true,
  };
}

/**
 * The error counts on a regular grid. The answer says its own window (`from`, `to`) and bucket size (`bucketS`: 3600, 21600 or 86400); without them the grid ends at `now` and the bucket is the smallest
 * gap between the listed buckets, else a default by window. The answer lists only buckets that had an error: a bucket it does not list had none (a real 0). Returns {ts, bucketS, values, total, peak, from, to}.
 */
export function errorGrid(rows, win, now, own = {}) {
  const span = WINDOW_S[win] ?? WINDOW_S['7d'];
  const sorted = [...rows].sort((x, y) => x.ts - y.ts);
  let step = { '24h': 3600, '7d': 6 * 3600, '30d': DAY }[win] ?? DAY;
  if (isNum(own.bucketS) && own.bucketS > 0) step = own.bucketS;
  else
    for (let i = 1; i < sorted.length; i++) {
      const d = sorted[i].ts - sorted[i - 1].ts;
      if (d > 0 && d < step) step = d;
    }
  const end = isNum(own.to) ? own.to : now;
  const start = isNum(own.from) ? own.from : end - span;
  const to = Math.ceil(end / step) * step;
  const from = Math.floor(start / step) * step;
  const n = Math.max(1, Math.min(2000, Math.round((to - from) / step)));
  const values = new Array(n).fill(0);
  let total = 0;
  for (const r of sorted) {
    const i = Math.floor((r.ts - from) / step);
    if (i < 0 || i >= n) continue;
    values[i] += r.count;
    total += r.count;
  }
  return { ts: Array.from({ length: n }, (_, i) => from + i * step), bucketS: step, values, total, peak: Math.max(0, ...values), from, to };
}

// ---------------------------------------------------------------- log filters

/** Epoch seconds of a `datetime-local` value read as UTC ("2026-11-14T08:30"); null for anything else. */
export function parseUtcInput(v) {
  const m = /^(\d{4})-(\d{2})-(\d{2})[T ](\d{2}):(\d{2})/.exec(String(v ?? ''));
  if (!m) return null;
  const t = Date.UTC(Number(m[1]), Number(m[2]) - 1, Number(m[3]), Number(m[4]), Number(m[5])) / 1000;
  return Number.isFinite(t) ? t : null;
}
/** The inverse, for filling a `datetime-local` input: "YYYY-MM-DDTHH:MM" in UTC; '' for no time. */
export const toUtcInput = (s) => (isNum(s) ? new Date(s * 1000).toISOString().slice(0, 16) : '');

/** The filter state of the log from the URL hash query (everything is checked: an unknown range or status falls back to "any"). */
export function logState(q = {}) {
  const intOrNull = (v) => (/^\d{1,11}$/.test(String(v ?? '')) ? Number(v) : null);
  const range = LOG_RANGES.includes(q.range) ? q.range : q.since || q.until ? 'custom' : 'any';
  return {
    range,
    since: intOrNull(q.since),
    until: intOrNull(q.until),
    actor: typeof q.actor === 'string' ? q.actor.slice(0, 128) : '',
    status: LOG_STATUSES.includes(q.status) ? q.status : '',
    system: typeof q.system === 'string' ? q.system.slice(0, 32) : '',
  };
}

/** The query `GET /audit` takes for a filter state: empty fields are left out, a preset range is `since = now - range`, `custom` uses its own since / until. */
export function logQuery(st, now = Date.now() / 1000, limit = LOG_LIMIT) {
  const q = { limit };
  if (st.system) q.system = st.system;
  if (st.actor) q.actor = st.actor;
  if (st.status) q.status = st.status;
  if (st.range === 'custom') {
    if (st.since != null) q.since = st.since;
    if (st.until != null) q.until = st.until;
  } else if (WINDOW_S[st.range]) q.since = Math.floor(now - WINDOW_S[st.range]);
  return q;
}

/** True when any filter narrows the log. */
export const logFiltered = (st) => st.range !== 'any' || !!st.actor || !!st.status || !!st.system;
