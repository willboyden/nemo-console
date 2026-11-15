// Pure shaping for the Data Designer column profile and run timeline (addendum 9.15), from `GET /systems/datadesigner/runs/{id}/profile`:
// {run_id, rows, columns: [{name, types: {string, number, bool, null, object, array}, non_null, distinct, distinct_capped, length: {min, p50, p95, max} | null,
// numeric: {min, p50, p95, max} | null, top: [{value, count}] | null}], values_hidden?}.
//
// ROLE: the values of the records are model-written content, so `top` (the most frequent values) is for an admin only. The API withholds it from a viewer
// (top null, values_hidden true) and the page must not show it either: shapeProfile drops it unless `admin` is true AND the answer does not say values_hidden,
// even if a (wrong) answer carried it. Column names and aggregates are shown to both. No DOM. Unit-tested in tests/dd-profile-model.test.mjs.
const isNum = (n) => typeof n === 'number' && Number.isFinite(n);
const num = (v) => (isNum(v) ? v : null);

/** The JSON types a column can hold, in the order the type-mix bar stacks them. */
export const TYPE_KEYS = ['string', 'number', 'bool', 'null', 'object', 'array'];

/** The most columns a profile carries (the contract's bound) and the most top values per column. */
export const MAX_COLUMNS = 200;
export const MAX_TOP = 10;

const range = (r) => {
  if (!r || typeof r !== 'object') return null;
  const out = { min: num(r.min), p50: num(r.p50), p95: num(r.p95), max: num(r.max) };
  return Object.values(out).some((v) => v != null) ? out : null;
};

/**
 * The route's answer as {runId, rows, columns, valuesHidden, truncated, topShown}. `truncated` is the route's flag: it read only part of the records (rows over 2000) or dropped columns (over 200, or bad names). `admin` is the caller's role (isAdmin()). Each column: {name, types (counts, 0 for absent), typeTotal,
 * dominant (the most frequent type among the non-null ones, or null), nonNull, nullRate (0..1, null when it cannot be told), distinct, distinctCapped, length, numeric, top}.
 * `top` is [] unless the caller is an admin and the answer is not marked values_hidden; it is then at most MAX_TOP entries {value (text), count}. null count means no value.
 */
export function shapeProfile(answer, { admin = false } = {}) {
  const rows = isNum(answer?.rows) && answer.rows >= 0 ? answer.rows : null;
  const valuesHidden = answer?.values_hidden === true || !admin;
  const cols = Array.isArray(answer?.columns) ? answer.columns.slice(0, MAX_COLUMNS) : [];
  const columns = [];
  for (const c of cols) {
    if (!c || typeof c.name !== 'string' || !c.name) continue;
    const types = {};
    for (const k of TYPE_KEYS) types[k] = isNum(c.types?.[k]) && c.types[k] > 0 ? c.types[k] : 0;
    const typeTotal = TYPE_KEYS.reduce((a, k) => a + types[k], 0);
    const nonNull = num(c.non_null);
    const denom = rows != null && rows > 0 ? rows : typeTotal > 0 ? typeTotal : null;
    let nullRate = null;
    if (denom != null && nonNull != null) nullRate = Math.min(1, Math.max(0, 1 - nonNull / denom));
    else if (typeTotal > 0) nullRate = types.null / typeTotal;
    const typed = TYPE_KEYS.filter((k) => k !== 'null' && types[k] > 0).sort((a, b) => types[b] - types[a]);
    const top =
      !valuesHidden && Array.isArray(c.top)
        ? c.top
            .filter((t) => t && isNum(t.count) && t.count >= 0 && (typeof t.value === 'string' || isNum(t.value) || typeof t.value === 'boolean'))
            .slice(0, MAX_TOP)
            .map((t) => ({ value: String(t.value), count: t.count }))
        : [];
    columns.push({
      name: c.name,
      types,
      typeTotal,
      dominant: typed[0] ?? null,
      nonNull,
      nullRate,
      distinct: num(c.distinct),
      distinctCapped: c.distinct_capped === true,
      length: range(c.length),
      // numeric ranges are real cell values: shown only under the same rule as `top` (the server also withholds them)
      numeric: valuesHidden ? null : range(c.numeric),
      top,
    });
  }
  return {
    runId: typeof answer?.run_id === 'string' ? answer.run_id : null,
    rows,
    columns,
    valuesHidden,
    truncated: answer?.truncated === true,
    topShown: !valuesHidden && columns.some((c) => c.top.length > 0),
  };
}

/** The type-mix rows for the stacked bar: [{id, label, values: {string, number, ...}}] one per column with at least one value; counts, null parts left as given (0 hides the segment). */
export function typeMixRows(profile) {
  return profile.columns.filter((c) => c.typeTotal > 0).map((c) => ({ id: c.name, label: c.name, values: { ...c.types }, totalText: String(c.typeTotal) }));
}

/** The columns that have top values (an admin only gets any): the picker of the top-values chart. */
export const topColumns = (profile) => profile.columns.filter((c) => c.top.length > 0);

/** The share of the column's non-null values that the top values cover (0..1) or null; the rest is "other". */
export function topCoverage(col) {
  if (!col.top.length || col.nonNull == null || col.nonNull <= 0) return null;
  const sum = col.top.reduce((a, t) => a + t.count, 0);
  return Math.min(1, sum / col.nonNull);
}

// ------------------------------------------------------------------------------------------------ the run timeline (from the dd.run events)

/** The newest event of each run id (a run that started and finished has two events): [event] in the order of the newest of each. Events without a run id are dropped. */
export function latestPerRun(events) {
  const latest = new Map();
  for (const e of Array.isArray(events) ? events : []) {
    const id = e?.data?.run_id;
    if (typeof id !== 'string' || !id) continue;
    const prev = latest.get(id);
    if (!prev || (isNum(e.ts) ? e.ts : 0) >= (isNum(prev.ts) ? prev.ts : 0)) latest.set(id, e);
  }
  return [...latest.values()];
}

/**
 * The timeline rows from run rows (runs.shapeRuns of `dd.run` events, oldest first): the last `max` runs as {id, label, script, mode, verdict, vclass, running, ts, durationMs,
 * requested, written}. `written` is the records the run wrote (num_records) and `requested` what it was asked for (num_records_requested); either may be null.
 */
export function ddTimelineRows(rows, nowMs, { max = 16 } = {}) {
  return rows.slice(-max).map((r) => {
    const f = r.fields || {};
    const running = String(r.verdict).toLowerCase() === 'running';
    return {
      id: r.id,
      label: r.label,
      script: typeof f.script === 'string' ? f.script : null,
      mode: typeof f.mode === 'string' ? f.mode : null,
      verdict: r.verdict,
      vclass: r.vclass,
      running,
      ts: r.ts,
      durationMs: running ? (isNum(r.ts) && isNum(nowMs) ? Math.max(0, nowMs - r.ts * 1000) : null) : num(f.duration_ms),
      requested: num(f.num_records_requested),
      written: num(f.num_records),
    };
  });
}
