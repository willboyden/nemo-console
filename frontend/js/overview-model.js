// Pure shaping of the Overview additions (addendum 9.16): the metric each system tile draws as a 1 h sparkline, the up / down strip for the systems that have a `*.up`-style
// metric, and the problem counts by severity across systems. No DOM, no i18n. A tile with no samples has no sparkline (null), never a flat line at 0. Unit-tested in tests/overview-model.test.mjs.
import { normalizeAnswer, rateSegments, pickRateScale, scaleSegments } from './series-model.js';
import { baseKey, finite, gridSize, alignToGrid, arrayStats } from './series-math.js';
import { stripFromAnswer } from './strip-model.js';

/**
 * The one metric each system's tile draws. `how`: 'rate' (a counter, drawn as a per-second or per-minute rate), 'value' (a gauge as it is); `combine`: how the variants of a
 * labelled key become one line ('sum' for counts and rates, 'mean' for ratios and utilisation); `unit`: how the newest figure reads (a key of fmtSparkValue); `up`: the 0 / 1 key
 * whose history is the tile's up / down strip, where the system has one. The keys are the adapters' own (backend/crates/nemo-console/src/adapters/*.rs).
 */
export const SPARKS = {
  switchyard: { key: 'sy.requests_total', how: 'rate', combine: 'sum', unit: 'rate' },
  relay: { key: 'relay.atof_files', how: 'value', combine: 'sum', unit: 'int', up: 'relay.gateway_up' },
  dynamo: { key: 'dyn.requests_total', how: 'rate', combine: 'sum', unit: 'rate', up: 'dyn.up' },
  skillevaluator: { key: 'se.runs', how: 'value', combine: 'sum', unit: 'int' },
  datadesigner: { key: 'dd.runs', how: 'value', combine: 'sum', unit: 'int' },
  openshell: { key: 'os.sandboxes', how: 'value', combine: 'sum', unit: 'int', up: 'os.gateway_up' },
  dcgm: { key: 'dcgm.gpu_util', how: 'value', combine: 'mean', unit: 'ratio' },
  aiperf: { key: 'ap.output_tps', how: 'value', combine: 'sum', unit: 'num' },
  guardrails: { key: 'gr.configs', how: 'value', combine: 'sum', unit: 'int', up: 'gr.up' },
  evaluator: { key: 'ev.score', how: 'value', combine: 'mean', unit: 'ratio' },
  modelopt: { key: 'mo.compression_ratio', how: 'value', combine: 'sum', unit: 'ratio' },
  gym: { key: 'gym.mean_reward', how: 'value', combine: 'sum', unit: 'num' },
  flare: { key: 'flare.generated_total', how: 'rate', combine: 'sum', unit: 'rate' },
};
export const SPARK_WINDOW = '1h';
/** A tile is refreshed at most this often (ms): the sparkline is a trend, not a live number. */
export const SPARK_STALE_MS = 60000;

/** The series request of a system's tile: one batched request, the spark key and (when there is one) the up key; labelled keys are split so the tile can average them. */
export function sparkRequest(system) {
  const spec = SPARKS[system];
  if (!spec) return null;
  return { keys: spec.up ? [spec.key, spec.up] : [spec.key], window: SPARK_WINDOW, split: spec.combine === 'mean' };
}

/** The sampled grid slots of several series combined: 'sum' adds the series that have a value there, 'mean' averages them; a slot no series has is null. */
function combineOnGrid(lists, combine) {
  const n = Math.max(0, ...lists.map((l) => l.length));
  return Array.from({ length: n }, (_, i) => {
    const v = lists.map((l) => l[i]).filter(finite);
    if (!v.length) return null;
    const s = v.reduce((a, b) => a + b, 0);
    return combine === 'mean' ? s / v.length : s;
  });
}

/**
 * The tile's sparkline from the series answer: {values: [number|null per slot, oldest first], ts, latest, min, max, per: 's'|'min'|null, any, spec}.
 * A counter becomes a rate first (a reset leaves a gap, as a missing sample does); the line is the combination of the key's variants on one grid.
 */
export function sparkModel(answer, system) {
  const spec = SPARKS[system];
  const a = answer && typeof answer === 'object' ? answer : {};
  const empty = { values: [], ts: [], latest: null, min: null, max: null, per: null, any: false, spec: spec ?? null };
  if (!spec) return empty;
  const lines = normalizeAnswer(a).filter((s) => baseKey(s.key) === baseKey(spec.key) && s.points.length);
  const n = gridSize(a.from, a.to, a.step_s);
  if (!lines.length || !n) return empty;
  let per = null;
  let perLine;
  if (spec.how === 'rate') {
    const segs = lines.map((s) => rateSegments(s.points, a.step_s));
    const scale = pickRateScale(segs);
    per = scale.per;
    perLine = segs.map((sg) => alignToGrid(scaleSegments(sg, scale.factor).flat(), { from: a.from, stepS: a.step_s, n }));
  } else perLine = lines.map((s) => alignToGrid(s.points, { from: a.from, stepS: a.step_s, n }));
  const values = combineOnGrid(perLine, spec.combine);
  const st = arrayStats(values);
  return { values, ts: values.map((_, i) => a.from + i * a.step_s), latest: st.latest, min: st.min, max: st.max, per, any: st.n > 0, spec };
}

/** The tile's up / down strip, or null when the system has no up key or the answer has no series for it. */
export function tileStrip(answer, system, opts = {}) {
  const up = SPARKS[system]?.up;
  return up ? stripFromAnswer(answer, up, { maxCells: 60, ...opts }) : null;
}

/** The newest figure of a sparkline as text, by the spec's unit; an en dash for none. */
export function fmtSparkValue(v, unit, per) {
  if (!finite(v)) return '–';
  switch (unit) {
    case 'ratio':
      return `${(v * 100).toFixed(v < 0.1 ? 1 : 0)}%`;
    case 'rate':
      return `${v >= 100 ? v.toFixed(0) : v >= 10 ? v.toFixed(1) : v.toFixed(2)}/${per === 'min' ? 'min' : 's'}`;
    case 'int':
      return String(Math.round(v));
    default:
      return Math.abs(v) >= 1000 ? Math.round(v).toLocaleString('en-US') : String(+v.toFixed(2));
  }
}

export const SEVERITIES = ['error', 'warn', 'info'];

/**
 * Problem counts by severity across systems from the overview's systems: {totals: {error, warn, info, all}, rows: [{system, error, warn, info, total}], systems (with a problem), clean (without)}.
 * Rows are the systems that have at least one problem, the worst first (errors, then warnings, then info, then name). A problem with an unknown severity counts as info.
 */
export function problemCounts(systems) {
  const rows = [];
  const totals = { error: 0, warn: 0, info: 0, all: 0 };
  let clean = 0;
  for (const s of Array.isArray(systems) ? systems : []) {
    const row = { system: s.system, error: 0, warn: 0, info: 0, total: 0 };
    for (const p of Array.isArray(s.problems) ? s.problems : []) {
      const sev = p?.severity === 'error' || p?.severity === 'warn' ? p.severity : 'info';
      row[sev]++;
      row.total++;
      totals[sev]++;
      totals.all++;
    }
    if (row.total) rows.push(row);
    else clean++;
  }
  rows.sort((a, b) => b.error - a.error || b.warn - a.warn || b.info - a.info || (a.system < b.system ? -1 : 1));
  return { totals, rows, systems: rows.length, clean };
}
