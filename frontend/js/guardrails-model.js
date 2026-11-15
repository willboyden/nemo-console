// Pure shaping of the Guardrails cards (addendum 9.16): the probe matrix over check runs (rows = the three probes, columns = runs, a cell = what the rails answered against
// what they should have), the latency strip under it, and the rail pipeline of each served config from `GET /systems/guardrails/rails`. No DOM, no i18n. A probe a run did not
// reach (the check stops at the first probe that errors) is null, never "passed"; a latency that was not measured is null, never 0. Unit-tested in tests/guardrails-model.test.mjs.
import { GUARDRAILS_EXPECTED } from './run-descriptors.js';
import { fmtRowTime } from './runs.js';
import { median } from './viz/stats.js';

const isNum = (n) => typeof n === 'number' && Number.isFinite(n);

/** The three fixed probes of the `check` action, in the order the adapter runs them (guardrails.rs PROBES). */
export const PROBES = ['benign', 'jailbreak', 'harmful'];
export const EXPECTED = GUARDRAILS_EXPECTED;
/** How many runs the matrix shows (the newest ones of the filtered view): a column of a cell matrix is at least 34 px wide. */
export const MAX_RUNS = 24;
/** The adapter's HTTP timeout for one probe is 90 s: a latency at or above this was a timeout, not an answer. */
export const TIMEOUT_MS = 90000;

/** `/v1/checks` says passed / blocked; the console says allowed / blocked. Any other status (error ...) is shown as it is. */
export const statusWord = (s) => (s === 'passed' ? 'allowed' : s == null || s === '' ? null : String(s));

/**
 * One probe's cell of a run, from the event's `results`: {status, word, expected, expectedWord, match, error, rail, ms, timeout}, or null when the run has no result for it.
 * `match` is true when the status is the one the rails should give; an `error` status is never a match (and not a mismatch of policy either: it says the probe did not run).
 */
export function probeCell(results, probe) {
  const r = (Array.isArray(results) ? results : []).find((x) => x && x.probe === probe);
  if (!r) return null;
  const status = typeof r.status === 'string' ? r.status : null;
  const expected = EXPECTED[probe] ?? null;
  const ms = isNum(r.ms) ? r.ms : null;
  return {
    probe,
    status,
    word: statusWord(status),
    expected,
    expectedWord: statusWord(expected),
    match: status != null && expected != null ? status === expected : null,
    error: status === 'error',
    rail: typeof r.rail === 'string' && r.rail ? r.rail : null,
    ms,
    timeout: ms != null && ms >= TIMEOUT_MS,
  };
}

/** The columns' short label: the time only when the runs are within a day, else the date and time (the table has both always). */
function colLabel(row, sameDay) {
  const full = fmtRowTime(row); // "MM-DD HH:MM"
  return sameDay ? full.slice(6) : full;
}

/**
 * The matrix from run rows (runs.shapeRuns with the GUARDRAILS descriptor, oldest first).
 * Returns {cols: [{id, ts, label, title, config, verdict, vclass}], probes: [{name, expected, expectedWord}], cells: {probe: [cell|null per col]}, latency: {probe: [ms|null per col]},
 * maxMs, stats: {runs, clean, slips, errors, perProbe: {probe: {n, match, mismatch, errors, p50, max}}}, hidden}.
 * `hidden` = runs older than the newest MAX_RUNS. Probes beyond the three fixed ones that a result carries are appended (a future probe still shows).
 */
export function probeMatrix(rows, { max = MAX_RUNS } = {}) {
  const all = Array.isArray(rows) ? rows : [];
  const shown = all.slice(-max);
  const extra = [];
  for (const r of shown)
    for (const x of Array.isArray(r.fields?.results) ? r.fields.results : []) if (x && typeof x.probe === 'string' && !PROBES.includes(x.probe) && !extra.includes(x.probe)) extra.push(x.probe);
  const names = [...PROBES, ...extra.sort()];
  const first = shown[0]?.ts,
    last = shown.at(-1)?.ts;
  const sameDay = isNum(first) && isNum(last) && last - first < 86400 && new Date(first * 1000).getUTCDate() === new Date(last * 1000).getUTCDate();
  const cols = shown.map((r) => ({
    id: r.id,
    ts: r.ts,
    label: colLabel(r, sameDay),
    title: `${fmtRowTime(r)} · ${r.label}`,
    config: typeof r.fields?.config === 'string' ? r.fields.config : null,
    verdict: r.verdict,
    vclass: r.vclass,
  }));
  const cells = {},
    latency = {};
  for (const p of names) {
    cells[p] = shown.map((r) => probeCell(r.fields?.results, p));
    latency[p] = cells[p].map((c) => c?.ms ?? null);
  }
  const lat = names.flatMap((p) => latency[p]).filter(isNum);
  const perProbe = {};
  for (const p of names) {
    const cs = cells[p].filter(Boolean);
    perProbe[p] = {
      n: cs.length,
      match: cs.filter((c) => c.match === true).length,
      mismatch: cs.filter((c) => c.match === false && !c.error).length,
      errors: cs.filter((c) => c.error).length,
      p50: median(cs.map((c) => c.ms).filter((m) => isNum(m) && m < TIMEOUT_MS)),
      max: cs.some((c) => isNum(c.ms)) ? Math.max(...cs.map((c) => c.ms).filter(isNum)) : null,
    };
  }
  const slips = shown.filter((_, i) => names.some((p) => cells[p][i] && cells[p][i].match === false && !cells[p][i].error)).length;
  const errors = shown.filter((_, i) => names.some((p) => cells[p][i]?.error)).length;
  return {
    cols,
    probes: names.map((name) => ({ name, expected: EXPECTED[name] ?? null, expectedWord: statusWord(EXPECTED[name] ?? null) })),
    cells,
    latency,
    maxMs: lat.length ? Math.max(...lat.filter((m) => m < TIMEOUT_MS), 0) || null : null,
    stats: { runs: shown.length, clean: shown.length - slips - errors, slips, errors, perProbe },
    hidden: Math.max(0, all.length - shown.length),
  };
}

// ---------------------------------------------------------------- rails

const flowList = (v) => (Array.isArray(v) ? v.filter((x) => typeof x === 'string' && x.trim() !== '') : []);
const posInt = (v) => (isNum(v) && v >= 0 ? Math.round(v) : null);

/**
 * `GET /systems/guardrails/rails` as the diagram's model. Tolerant: a config without a main model keeps `mainModel: null`, `streaming` stays null when the config has none, and a
 * stray non-string flow is dropped. Returns {configs: [{id, mainModel, input, output, retrieval, streaming: {enabled, chunkSize, contextSize} | null, rails (input + output count)}], problems: [{config: id | null, code, count: n | null}]} (the backend's problem codes: configs.capped, config.unreadable, config.too_large, config.not_utf8, config.yaml_error, config.not_a_mapping, flows.dropped, flows.capped, main_model.dropped).
 */
export function shapeRails(answer) {
  const a = answer && typeof answer === 'object' ? answer : {};
  const configs = (Array.isArray(a.configs) ? a.configs : [])
    .filter((c) => c && typeof c === 'object' && typeof c.id === 'string' && c.id)
    .map((c) => {
      const s = c.streaming && typeof c.streaming === 'object' ? c.streaming : null;
      const input = flowList(c.input),
        output = flowList(c.output);
      return {
        id: c.id,
        mainModel: typeof c.main_model === 'string' && c.main_model ? c.main_model : null,
        input,
        output,
        retrieval: flowList(c.retrieval),
        streaming: s ? { enabled: typeof s.enabled === 'boolean' ? s.enabled : null, chunkSize: posInt(s.chunk_size), contextSize: posInt(s.context_size) } : null,
        rails: input.length + output.length,
      };
    });
  const problems = (Array.isArray(a.problems) ? a.problems : [])
    .filter((p) => p && typeof p === 'object' && typeof p.code === 'string' && p.code)
    .map((p) => ({ config: typeof p.config === 'string' && p.config ? p.config : null, code: p.code, count: posInt(p.count) }));
  return { configs, problems };
}

/** The stages of one config's pipeline in the order a request meets them: input rails, the main model, output rails. A stage with no flow keeps its place (`none: true`: no rail there). */
export function pipelineStages(cfg) {
  return [
    { kind: 'input', flows: cfg.input, none: cfg.input.length === 0 },
    { kind: 'model', model: cfg.mainModel },
    { kind: 'output', flows: cfg.output, none: cfg.output.length === 0 },
  ];
}
