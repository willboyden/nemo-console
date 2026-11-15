// Pure shaping for the Model Optimizer cards (addendum 9.15), from the `modelopt.job` events: the job timeline (duration bars by verdict
// and method), base vs output size with the ratio, and the age of a job that is still running (a stuck job stands out). No DOM. A job with no
// size or duration has none (null), never 0. Unit-tested in tests/modelopt-model.test.mjs.
import { median } from './viz/stats.js';

const isNum = (n) => typeof n === 'number' && Number.isFinite(n);
const num = (v) => (isNum(v) ? v : null);

/** A running job older than this many times the typical finished job (same method, else any) is flagged as taking longer than usual. */
export const STUCK_FACTOR = 2;
/** ... but only once it is also older than this (a short job that runs "twice as long as usual" is not stuck): 10 minutes, in ms. */
export const STUCK_MIN_MS = 10 * 60 * 1000;

const method = (r) => (typeof r.fields?.method === 'string' && r.fields.method ? r.fields.method : null);

/**
 * Jobs from run rows (runs.shapeRuns, oldest first): {id, label, recipe, method, base_model, verdict, vclass, running, ts, durationMs, ageMs, baseBytes, outputBytes, ratio}.
 * `durationMs` is the finished job's duration (null while it runs or when the event has none); `ageMs` is how long a RUNNING job has run (now - the event time, which is
 * when the job started); null for a finished job. The ratio is the event's own, else output / base when both sizes are there, else null.
 */
export function jobsOf(rows, nowMs) {
  return rows.map((r) => {
    const f = r.fields || {};
    const running = r.vclass === 'info' && String(r.verdict).toLowerCase() === 'running';
    const base = num(f.base_bytes),
      out = num(f.output_bytes);
    const ratio = num(f.ratio) ?? (base != null && base > 0 && out != null ? out / base : null);
    return {
      id: r.id,
      label: r.label,
      recipe: typeof f.recipe === 'string' ? f.recipe : null,
      method: method(r),
      base_model: typeof f.base_model === 'string' ? f.base_model : null,
      verdict: r.verdict,
      vclass: r.vclass,
      running,
      ts: r.ts,
      durationMs: running ? null : num(f.duration_ms),
      ageMs: running && isNum(r.ts) && isNum(nowMs) ? Math.max(0, nowMs - r.ts * 1000) : null,
      baseBytes: base,
      outputBytes: out,
      ratio,
    };
  });
}

/** The methods seen, most frequent first (ties by name): they give the bar colours, in this order. */
export function methodsOf(jobs) {
  const c = new Map();
  for (const j of jobs) if (j.method) c.set(j.method, (c.get(j.method) || 0) + 1);
  return [...c.entries()].sort((a, b) => b[1] - a[1] || (a[0] < b[0] ? -1 : 1)).map(([m]) => m);
}

/**
 * The timeline: the last `max` jobs oldest first as {id, label, method, verdict, vclass, running, value (ms: the duration, or the age of a running job), durationKnown}.
 * A finished job with no duration has value null (no bar, a dash); it is not drawn as zero.
 */
export function timelineRows(jobs, { max = 16 } = {}) {
  return jobs.slice(-max).map((j) => ({ id: j.id, label: j.label, method: j.method, verdict: j.verdict, vclass: j.vclass, running: j.running, value: j.running ? j.ageMs : j.durationMs, ts: j.ts }));
}

/** Percent of the base model's size that a ratio is, and the saving: {pct: 31.2, saved: 68.8}; nulls for no ratio. */
export function ratioParts(ratio) {
  return isNum(ratio) && ratio >= 0 ? { pct: ratio * 100, saved: (1 - ratio) * 100 } : { pct: null, saved: null };
}

/**
 * Jobs with BOTH sizes, newest last, at most `max`: {id, label, method, base, output, ratio}. A job with only one size (or none) has no pair to compare and is left
 * out; `missing` counts the jobs left out so the card can say so. The ratio is recomputed from the two sizes when the event's own is missing.
 */
export function sizeRows(jobs, { max = 12 } = {}) {
  const have = jobs.filter((j) => j.baseBytes != null && j.outputBytes != null && j.baseBytes > 0);
  return {
    rows: have.slice(-max).map((j) => ({ id: j.id, label: j.label, method: j.method, base: j.baseBytes, output: j.outputBytes, ratio: j.ratio ?? j.outputBytes / j.baseBytes })),
    missing: jobs.filter((j) => !j.running).length - have.length,
    total: have.length,
  };
}

/**
 * The jobs that are running, each with the typical duration of the finished (succeeded) jobs of the same method (else of all methods) and whether this one has run
 * longer than STUCK_FACTOR times that (and than STUCK_MIN_MS): [{id, label, method, ageMs, typicalMs|null, stuck}]. `typicalMs` is null when no finished job gives one;
 * then nothing is flagged (an age alone says nothing).
 */
export function runningJobs(jobs) {
  const done = jobs.filter((j) => !j.running && j.vclass === 'ok' && j.durationMs != null);
  return jobs
    .filter((j) => j.running && j.ageMs != null)
    .map((j) => {
      const same = done.filter((d) => d.method === j.method).map((d) => d.durationMs);
      const typical = median(same.length ? same : done.map((d) => d.durationMs));
      return { id: j.id, label: j.label, method: j.method, ageMs: j.ageMs, typicalMs: typical, stuck: typical != null && j.ageMs > typical * STUCK_FACTOR && j.ageMs > STUCK_MIN_MS };
    });
}
