// MOCK answers of the wave-3 routes (docs/ARCHITECTURE.md addendum 9.15): `GET /systems/aiperf|skillevaluator/runs/{id}/summary`,
// `GET /systems/aiperf/runs/{id}/requests` and `GET /systems/datadesigner/runs/{id}/profile`. MOCK-ONLY: every number is invented, but the SHAPES follow the
// contract (numbers and identifiers only: no finding text, criterion text, notes or conclusions; `top` of the profile only for an admin, `values_hidden: true`
// for a viewer), and the numbers agree with the run's own event (the summary's percentiles centre on the event's p50 and end at its p99, the request rows
// have the event's concurrency, request count and error rate, the validators carry the event's error / high / critical counts). Deterministic: every figure is
// a function of the run id, so two requests agree. Generic names only: no real host or machine name appears here.

const hash32 = (s) => {
  let h = 2166136261;
  for (let i = 0; i < s.length; i++) h = Math.imul(h ^ s.charCodeAt(i), 16777619);
  return h >>> 0;
};
/** A seeded generator (mulberry32) from a string. */
const rngOf = (s) => {
  let a = hash32(String(s)) || 1;
  return () => {
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
};
const r1 = (v) => Math.round(v * 10) / 10;
const r3 = (v) => Math.round(v * 1000) / 1000;
const normal = (rnd) => Math.sqrt(-2 * Math.log(1 - rnd())) * Math.cos(2 * Math.PI * rnd());
const isNum = (n) => typeof n === 'number' && Number.isFinite(n);
const quantile = (sorted, q) => {
  if (!sorted.length) return null;
  const pos = q * (sorted.length - 1);
  const lo = Math.floor(pos),
    hi = Math.ceil(pos);
  return sorted[lo] + (sorted[hi] - sorted[lo]) * (pos - lo);
};

/**
 * The part of a run id that does not move: the ids of the mock carry the time they were made (`20261112T091512-104-chat-short`), which differs on every start, so a seed
 * made from the whole id would give a different picture each time. Without the stamp, the same run gets the same numbers on every start.
 */
export const stableKey = (id) => String(id).replace(/^\d{8}T\d{6}-/, '');

/** A run id the adapters would accept: letters, digits, dot, underscore, dash (and the `#` of a duplicate). */
export const RUN_ID_RE = /^[A-Za-z0-9._#-]{1,200}$/;

const Z = { p1: -2.326, p5: -1.645, p10: -1.2816, p25: -0.6745, p50: 0, p75: 0.6745, p90: 1.2816, p95: 1.645, p99: 2.326 };
/** A metric object {unit, avg, min, max, std, p1..p99} for a log-normal around `median` with spread `sigma` (a smooth, right-skewed curve). */
function lognormalMetric(unit, median, sigma, scale = 1) {
  const m = { unit };
  for (const [k, z] of Object.entries(Z)) m[k] = r3(median * Math.exp(sigma * z) * scale);
  m.avg = r3(median * Math.exp((sigma * sigma) / 2) * scale);
  m.min = r3(m.p1 * 0.82);
  m.max = r3(m.p99 * 1.3);
  m.std = r3(median * Math.sqrt((Math.exp(sigma * sigma) - 1) * Math.exp(sigma * sigma)) * scale);
  return m;
}
const onlyAvg = (unit, avg) => ({ unit, avg, min: null, max: null, std: null, p1: null, p5: null, p10: null, p25: null, p50: null, p75: null, p90: null, p95: null, p99: null });

// ------------------------------------------------------------------------------------------------ aiperf summary
/** `GET /systems/aiperf/runs/{id}/summary` for one `aiperf.run` event. */
export function aiperfSummary(ev) {
  const d = ev.data;
  const doc = {
    run_id: d.run_id,
    preset: d.preset ?? null,
    model: d.model ?? null,
    verdict: d.verdict ?? null,
    concurrency: d.concurrency ?? null,
    requests: d.requests ?? null,
    isl: d.isl ?? null,
    osl: d.osl ?? null,
    duration_s: isNum(d.duration_ms) ? r1(d.duration_ms / 1000) : null,
    metrics: {},
    http_phases_ms: null,
  };
  if (!isNum(d.ttft_p50_ms)) return doc; // a failed, interrupted or running run measured nothing
  const rnd = rngOf(`${stableKey(d.run_id)}|summary`);
  const ttftSigma = isNum(d.ttft_p99_ms) ? Math.max(0.05, Math.log(d.ttft_p99_ms / d.ttft_p50_ms) / Z.p99) : 0.4;
  const itl = isNum(d.itl_p50_ms) ? d.itl_p50_ms : 15;
  const osl = d.osl ?? 128;
  const m = doc.metrics;
  m.time_to_first_token = lognormalMetric('ms', d.ttft_p50_ms, ttftSigma);
  m.inter_token_latency = lognormalMetric('ms', itl, 0.16 + rnd() * 0.05);
  m.request_latency = lognormalMetric('ms', d.ttft_p50_ms + itl * (osl - 1), 0.2 + rnd() * 0.06);
  if (isNum(d.output_tps)) m.output_token_throughput = onlyAvg('tokens/sec', d.output_tps);
  if (isNum(d.request_tps)) m.request_throughput = onlyAvg('requests/sec', d.request_tps);
  if (isNum(d.requests)) m.request_count = onlyAvg('requests', d.requests);
  if (isNum(d.error_rate_pct)) m.request_error_rate = onlyAvg('%', d.error_rate_pct);
  if (isNum(d.duration_ms)) m.benchmark_duration = onlyAvg('sec', r1(d.duration_ms / 1000));
  if (isNum(d.isl)) m.input_sequence_length = { ...lognormalMetric('tokens', d.isl, 0.08), p1: null };
  if (isNum(d.osl)) m.output_sequence_length = lognormalMetric('tokens', d.osl, 0.12);
  // Some runs (every fifth, by id) did not record the HTTP phases: the card says so.
  if (hash32(`${stableKey(d.run_id)}|phases`) % 5 !== 0)
    doc.http_phases_ms = {
      dns: r3(0.2 + rnd() * 0.5),
      connecting: r3(0.6 + rnd() * 1.4),
      sending: r3(0.3 + rnd() * 0.5),
      waiting: r3(d.ttft_p50_ms * (0.93 + rnd() * 0.05)),
      receiving: r3(itl * (osl - 1) * (0.85 + rnd() * 0.1)),
    };
  return doc;
}

// ------------------------------------------------------------------------------------------------ aiperf requests
export const MAX_REQUEST_ROWS = 5000;
const CODES = [429, 429, 429, 503, 503, 500, 408, 0]; // 0 = failed without a usable HTTP code (the route's own value)

/**
 * `GET /systems/aiperf/runs/{id}/requests` for one `aiperf.run` event: `{run_id, total, returned, truncated, rows}`, at most MAX_REQUEST_ROWS rows evenly sampled.
 * `start_ms` is relative to the first request; `worker` is a small integer assigned by first appearance; `error_code` an integer or null; no ids, no error text.
 */
export function aiperfRequests(ev, { maxRows = MAX_REQUEST_ROWS } = {}) {
  const d = ev.data;
  const rnd = rngOf(`${stableKey(d.run_id)}|requests`);
  const verdict = d.verdict;
  let total = isNum(d.requests) ? d.requests : 0;
  if (verdict === 'running') total = 0;
  if (verdict === 'interrupted') total = Math.round(total * 0.45);
  const failedRun = verdict === 'failed';
  if (failedRun) total = 12;
  const c = Math.max(1, d.concurrency ?? 4);
  const workers = Math.min(c, 8);
  const itl = d.itl_p50_ms ?? 15,
    ttft = d.ttft_p50_ms ?? 220,
    osl = d.osl ?? 128,
    isl = d.isl ?? 256;
  const errRate = failedRun ? 1 : (d.error_rate_pct ?? 0) / 100;
  // which requests fail: as many as the error rate says (at least one when it is not zero), picked by the run's own seed
  const errAt = new Set();
  const want = errRate > 0 ? Math.min(total, Math.max(1, Math.round(total * errRate))) : 0;
  while (errAt.size < want) errAt.add(Math.floor(rnd() * total));
  const free = new Array(c).fill(0);
  const all = [];
  for (let k = 0; k < total; k++) {
    let slot = 0;
    for (let s = 1; s < c; s++) if (free[s] < free[slot]) slot = s;
    const start = free[slot] + rnd() * 18;
    const isErr = errAt.has(k);
    let row;
    if (isErr) {
      const latency = 40 + rnd() * 420;
      row = {
        latency_ms: latency,
        ttft_ms: null,
        itl_avg_ms: null,
        input_tokens: Math.round(isl * (0.9 + rnd() * 0.2)),
        output_tokens: 0,
        error_code: failedRun ? 503 : CODES[Math.floor(rnd() * CODES.length)],
      };
    } else {
      const f = Math.exp(0.35 * normal(rnd));
      const t0 = ttft * f;
      const out = Math.max(8, Math.round(osl * (0.7 + rnd() * 0.5)));
      const itlAvg = itl * Math.exp(0.15 * normal(rnd));
      row = { latency_ms: t0 + itlAvg * (out - 1), ttft_ms: t0, itl_avg_ms: itlAvg, input_tokens: Math.round(isl * (0.9 + rnd() * 0.2)), output_tokens: out, error_code: null };
    }
    free[slot] = start + row.latency_ms;
    all.push({ i: k, start_ms: start, end_ms: start + row.latency_ms, worker: slot % workers, ...row });
  }
  // `worker` by first appearance: the slots are taken in order, so the workers already appear as 0, 1, 2 ...
  const t0 = all.length ? Math.min(...all.map((r) => r.start_ms)) : 0;
  const clean = (r) => ({
    i: r.i,
    start_ms: r1(r.start_ms - t0),
    end_ms: r1(r.end_ms - t0),
    latency_ms: r1(r.latency_ms),
    ttft_ms: r.ttft_ms == null ? null : r1(r.ttft_ms),
    itl_avg_ms: r.itl_avg_ms == null ? null : r1(r.itl_avg_ms),
    input_tokens: r.input_tokens,
    output_tokens: r.output_tokens,
    worker: r.worker,
    error_code: r.error_code,
  });
  const truncated = all.length > maxRows;
  const picked = truncated ? Array.from({ length: maxRows }, (_, k) => all[Math.floor((k * all.length) / maxRows)]) : all;
  return { run_id: d.run_id, total: all.length, returned: picked.length, skipped: 0, truncated, rows: picked.map(clean) };
}

// ------------------------------------------------------------------------------------------------ skillevaluator summary
const VALIDATORS = ['frontmatter', 'structure', 'links', 'naming', 'description-quality', 'script-safety', 'prompt-injection', 'secrets', 'size-limits', 'llm-security'];
const RUBRIC = [
  ['clear-trigger', 'high'],
  ['steps-are-checkable', 'high'],
  ['examples-present', 'medium'],
  ['failure-modes-named', 'medium'],
  ['scope-is-bounded', 'medium'],
  ['no-ambiguous-verbs', 'low'],
  ['output-is-verifiable', 'high'],
];
const DIMENSIONS = ['clarity', 'completeness', 'safety', 'structure', 'specificity'];
const METRICS = ['correctness', 'tool_use', 'efficiency', 'format'];

/** `GET /systems/skillevaluator/runs/{id}/summary` for one `skilleval.run` event. Tier 1 and Tier 3 are null for a run that did not have them. */
export function skillSummary(ev) {
  const d = ev.data;
  const rnd = rngOf(`${stableKey(d.run_id)}|skill`);
  const doc = { run_id: d.run_id, skill: d.skill ?? null, tiers: d.tiers ?? null, verdict: d.verdict ?? null, tier1: null, tier3: null };
  const t1 = d.tier1;
  if (t1 && typeof t1 === 'object') {
    const status = t1.status;
    const incomplete = Array.isArray(t1.incomplete) ? t1.incomplete : [];
    const failing = new Set();
    // the failing validators are picked by id, as many as the run had errors (at most five)
    const order = [...VALIDATORS].sort((a, b) => hash32(`${stableKey(d.run_id)}${a}`) - hash32(`${stableKey(d.run_id)}${b}`));
    for (const n of order.slice(0, Math.min(5, Math.max(0, t1.errors ?? 0)))) failing.add(n);
    let critical = t1.critical ?? 0,
      high = t1.high ?? 0,
      warn = t1.warnings ?? 0;
    const validators = VALIDATORS.map((name) => {
      const f = { critical: 0, high: 0, medium: 0, low: 0, info: 0 };
      if (incomplete.includes(name)) return { name, passed: null, status: 'incomplete', findings: f };
      if (failing.has(name) && status === 'failed') {
        if (critical > 0) {
          f.critical = 1;
          critical--;
        } else if (high > 0) {
          f.high = 1;
          high--;
        } else f.medium = 1 + Math.floor(rnd() * 2);
        return { name, passed: false, status: 'failed', findings: f };
      }
      if (warn > 0 && rnd() < 0.4) {
        f.medium = rnd() < 0.5 ? 1 : 0;
        f.low = f.medium ? 0 : 1 + Math.floor(rnd() * 2);
        warn -= f.medium + f.low;
      }
      f.info = rnd() < 0.3 ? 1 : 0;
      return { name, passed: true, status: 'passed', findings: f };
    });
    doc.tier1 = { validators, rubric: [], dimensions: {} };
    if (isNum(t1.rubric_score)) {
      doc.tier1.rubric = RUBRIC.map(([id, importance]) => {
        const score = Math.min(0.99, Math.max(0.05, t1.rubric_score / 100 + (rnd() - 0.5) * 0.35));
        const pass = score >= 0.6;
        return { id, importance, score: r3(score), pass, judge_pass: rnd() < 0.9 ? pass : !pass };
      });
    }
    if (isNum(t1.quality_score)) for (const n of DIMENSIONS) doc.tier1.dimensions[n] = r3(Math.min(0.99, Math.max(0.1, t1.quality_score / 100 + (rnd() - 0.5) * 0.3)));
  }
  const t3 = d.tier3;
  if (t3 && typeof t3 === 'object') {
    doc.tier3 = {
      agents: Object.entries(t3.agents ?? {}).map(([name, a]) => ({
        name,
        model: a.model ?? null,
        skill_lift: a.skill_lift ?? null,
        with_skill: a.with_skill ?? null,
        without_skill: a.without_skill ?? null,
        metrics:
          a.skill_lift == null || a.with_skill == null
            ? []
            : METRICS.map((m, k) => {
                const swing = (rnd() - 0.4) * 0.12;
                const w = Math.min(0.99, Math.max(0.05, a.with_skill + (k - 1.5) * 0.03 + swing));
                const wo = Math.min(0.99, Math.max(0.05, a.without_skill + (k - 1.5) * 0.03 - swing * 0.5));
                return { name: m, with_skill: r3(w), without_skill: r3(wo), delta: r3(w - wo) };
              }),
      })),
    };
  }
  return doc;
}

// ------------------------------------------------------------------------------------------------ data designer profile
const MAX_PROFILE_COLUMNS = 200;
const MAX_TOP = 10;
const MAX_DISTINCT_FOR_TOP = 50;
const DISTINCT_CAP = 1000;
const typeOf = (v) =>
  v === null || v === undefined ? 'null' : Array.isArray(v) ? 'array' : typeof v === 'object' ? 'object' : typeof v === 'boolean' ? 'bool' : typeof v === 'number' ? 'number' : 'string';
const cut = (s, n = 64) => (s.length > n ? s.slice(0, n) : s);

/**
 * `GET /systems/datadesigner/runs/{id}/profile` for one stored run (its `rows` and `columns`, as the records route would serve them). `top` is filled ONLY for an admin
 * (the most frequent values of a column with at most 50 distinct values); a viewer gets `top: null` and `values_hidden: true`.
 */
export function ddProfile(run, { admin }) {
  const rows = Array.isArray(run.rows) ? run.rows : [];
  const columns = (rows.length ? (run.columns ?? Object.keys(rows[0])) : []).slice(0, MAX_PROFILE_COLUMNS).map((name) => {
    const types = { string: 0, number: 0, bool: 0, null: 0, object: 0, array: 0 };
    const seen = new Map();
    const lengths = [],
      numbers = [];
    for (const r of rows) {
      const v = r[name];
      types[typeOf(v)]++;
      if (v === null || v === undefined) continue;
      if (typeof v === 'string') lengths.push(v.length);
      if (typeof v === 'number' && Number.isFinite(v)) numbers.push(v);
      const key = typeof v === 'object' ? JSON.stringify(v) : String(v);
      if (seen.size < DISTINCT_CAP || seen.has(key)) seen.set(key, (seen.get(key) || 0) + 1);
    }
    const stats = (xs) => {
      if (!xs.length) return null;
      const s = [...xs].sort((a, b) => a - b);
      return { min: s[0], p50: r1(quantile(s, 0.5)), p95: r1(quantile(s, 0.95)), max: s[s.length - 1] };
    };
    const top =
      admin && seen.size <= MAX_DISTINCT_FOR_TOP
        ? [...seen.entries()]
            .sort((a, b) => b[1] - a[1] || (a[0] < b[0] ? -1 : 1))
            .slice(0, MAX_TOP)
            .map(([value, count]) => ({ value: cut(value), count }))
        : null;
    return { name, types, non_null: rows.length - types.null, distinct: seen.size, distinct_capped: seen.size >= DISTINCT_CAP, length: stats(lengths), numeric: admin ? stats(numbers) : null, top };
  });
  // `values_hidden` is always there (false for an admin); `truncated` is true when rows > 2000, columns > 200 or bad column names were dropped
  // numeric ranges are real cell values: admin-only like `top`; `columns_dropped` counts non-identifier / over-cap columns
  return { run_id: run.run_id, rows: rows.length, columns, values_hidden: !admin, columns_dropped: 0, truncated: rows.length > 2000 };
}
