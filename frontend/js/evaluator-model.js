// Pure shaping for the NeMo Evaluator cards (addendum 9.15), from the `evaluator.run` events' `benchmarks` ([{bench, status, metric, score, samples,
// ci_low, ci_high, tokens, elapsed_s, failure_rate}]): scores with confidence-interval bars for run A (and B), the model x benchmark matrix of the
// latest score per pair, cost against score, and failure-rate bars. No DOM. A figure an older event lacks is null and shows as an en dash, never as 0.
// Unit-tested in tests/evaluator-model.test.mjs.
const isNum = (n) => typeof n === 'number' && Number.isFinite(n);
const num = (v) => (isNum(v) ? v : null);

/**
 * The benchmarks of one run row (runs.shapeRuns): clean objects {bench, status, metric, score, lo, hi, samples, tokens, elapsed_s, failure_rate}.
 * An interval counts only when both ends are numbers and lo <= hi (a half or an upside-down interval is no interval: lo and hi are null).
 */
export function benchesOf(row) {
  const list = row?.event?.data?.benchmarks;
  if (!Array.isArray(list)) return [];
  const out = [];
  for (const b of list) {
    if (!b || typeof b.bench !== 'string' || !b.bench) continue;
    const lo = num(b.ci_low),
      hi = num(b.ci_high);
    const ci = lo != null && hi != null && lo <= hi;
    out.push({
      bench: b.bench,
      status: typeof b.status === 'string' ? b.status : null,
      metric: typeof b.metric === 'string' ? b.metric : null,
      score: num(b.score),
      lo: ci ? lo : null,
      hi: ci ? hi : null,
      samples: num(b.samples),
      tokens: num(b.tokens),
      elapsed_s: num(b.elapsed_s),
      failure_rate: num(b.failure_rate),
    });
  }
  return out;
}

/** The run with benchmarks that is newest (rows are oldest first); null when none. Used when no run is selected. */
export const newestWithBenchmarks = (rows) => [...rows].reverse().find((r) => benchesOf(r).some((b) => b.score != null)) ?? null;

/**
 * Scores with their intervals, benchmark by benchmark: [{bench, a: {score, lo, hi, samples} | null, b: ... | null}]. The benchmarks of A come
 * first in their own order, then those only B has. A benchmark with no score in a run has no bar for it (null), never a bar at zero.
 */
export function ciRows(rowA, rowB) {
  const A = new Map(benchesOf(rowA).map((b) => [b.bench, b])),
    B = new Map(benchesOf(rowB).map((b) => [b.bench, b]));
  const names = [...new Set([...A.keys(), ...B.keys()])];
  const pick = (b) => (b && b.score != null ? { score: b.score, lo: b.lo, hi: b.hi, samples: b.samples, metric: b.metric } : null);
  return names.map((bench) => ({ bench, a: pick(A.get(bench)), b: pick(B.get(bench)) })).filter((r) => r.a || r.b);
}

/** The model of a run row: the event's `model`, else a dash-free placeholder is the caller's business (null here). */
export const modelOf = (row) => (typeof row?.fields?.model === 'string' && row.fields.model ? row.fields.model : null);

/**
 * The newest result of every (model, benchmark) pair over the rows: {models: [name], benches: [name], cells: Map("model|bench" -> {score, lo, hi, runId, ts, failure_rate, tokens, elapsed_s, samples}), domain: {min, max}}.
 * Models run in order of their best average score, highest first (ties by name); benchmarks run in order of how many models have them, most first. The
 * colour domain is 0..1 when every score is in [0, 1] (accuracy), else the data's own range.
 */
export const cellKey = (model, bench) => `${model}|${bench}`;
export function latestMatrix(rows) {
  const cells = new Map();
  for (const row of rows) {
    const model = modelOf(row);
    if (model == null) continue;
    for (const b of benchesOf(row)) {
      if (b.score == null) continue;
      const k = cellKey(model, b.bench);
      const prev = cells.get(k);
      if (prev && (prev.ts ?? -Infinity) > (row.ts ?? -Infinity)) continue;
      cells.set(k, {
        score: b.score,
        lo: b.lo,
        hi: b.hi,
        runId: row.id,
        ts: row.ts,
        failure_rate: b.failure_rate,
        tokens: b.tokens,
        elapsed_s: b.elapsed_s,
        samples: b.samples,
        model,
        bench: b.bench,
      });
    }
  }
  const models = [...new Set([...cells.values()].map((c) => c.model))];
  const benches = [...new Set([...cells.values()].map((c) => c.bench))];
  const avg = (m) => {
    const s = [...cells.values()].filter((c) => c.model === m).map((c) => c.score);
    return s.reduce((a, b) => a + b, 0) / s.length;
  };
  models.sort((a, b) => avg(b) - avg(a) || (a < b ? -1 : 1));
  benches.sort((a, b) => models.filter((m) => cells.has(cellKey(m, b))).length - models.filter((m) => cells.has(cellKey(m, a))).length || (a < b ? -1 : 1));
  const scores = [...cells.values()].map((c) => c.score);
  const inUnit = scores.length > 0 && scores.every((s) => s >= 0 && s <= 1);
  const domain = inUnit ? { min: 0, max: 1 } : scores.length ? { min: Math.min(...scores), max: Math.max(...scores) } : { min: 0, max: 1 };
  return { models, benches, cells, domain };
}

/** The cost axes of the cost-vs-score scatter: id -> the benchmark field. */
export const COST_AXES = [
  { id: 'tokens', field: 'tokens' },
  { id: 'elapsed_s', field: 'elapsed_s' },
];

/**
 * One point per (run, benchmark) result that has both a score and the chosen cost: {id (run id), bench, model, x (cost), y (score), g (colour/shape slot by benchmark)}.
 * `skipped` counts the results with a score but no cost figure (an older event). The slots run out at `maxGroups`; later benchmarks share the last one.
 */
export function costPoints(rows, axis, { maxGroups = 6 } = {}) {
  const field = COST_AXES.find((a) => a.id === axis)?.field ?? 'tokens';
  const pts = [];
  let skipped = 0;
  for (const row of rows) {
    for (const b of benchesOf(row)) {
      if (b.score == null) continue;
      if (b[field] == null) {
        skipped++;
        continue;
      }
      pts.push({ id: row.id, bench: b.bench, model: modelOf(row), x: b[field], y: b.score });
    }
  }
  const benches = [...new Set(pts.map((p) => p.bench))].sort();
  const slot = new Map(benches.map((b, i) => [b, Math.min(i, maxGroups - 1)]));
  return { points: pts.map((p) => ({ ...p, g: slot.get(p.bench) })), groups: benches.map((b) => ({ bench: b, g: slot.get(b), count: pts.filter((p) => p.bench === b).length })), skipped };
}

/**
 * Failure rates of the newest result of each (model, benchmark) pair, highest first: [{model, bench, runId, rate (0..1)}] and the unit used.
 * The field is a share of the samples that failed (0..1). If any value is above 1 the whole set is read as percent and divided by 100, so a bar never
 * runs off its scale. A pair with no failure figure is left out (and counted in `missing`), never drawn as 0.
 */
export function failureRows(matrix, { limit = 12 } = {}) {
  const have = [...matrix.cells.values()].filter((c) => c.failure_rate != null);
  const percent = have.some((c) => c.failure_rate > 1);
  const rows = have
    .map((c) => ({ model: c.model, bench: c.bench, runId: c.runId, rate: Math.min(1, Math.max(0, percent ? c.failure_rate / 100 : c.failure_rate)) }))
    .sort((a, b) => b.rate - a.rate || (a.model < b.model ? -1 : a.model > b.model ? 1 : a.bench < b.bench ? -1 : 1));
  return { rows: rows.slice(0, limit), total: rows.length, missing: matrix.cells.size - have.length, percent };
}
