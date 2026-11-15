// The run-history descriptors of the eight run-based pages (addendum 9.13). Pure data plus small `flatten` functions, no DOM.
// Every field name below is read from what the Rust adapters put into the event's `data` (backend/crates/nemo-console/src/
// adapters/<system>.rs, the function that builds the SystemEvent): the comment above each descriptor names it. A field an
// event lacks is simply missing (shown as an en dash); a number is only ever a JSON number.
//
// Descriptor: {system, kind, id, verdict, label: [fields] | (fields) => text, numbers: [{field, label (i18n key), unit, better}], dynamic: [{prefix,
// label (i18n key with {name}), unit, better}], flatten(data, event) -> extra fields, groupBy: [fields], filters: [fields]}.
// `id` / `verdict` are a field name or (fields, event) => value. `better` is 'lower' | 'higher' | null (no direction: the
// compare table shows the difference without a better / worse mark). `unit` is a key of runs.js fmtRunValue.
import { t } from './i18n.js';

const isNum = (n) => typeof n === 'number' && Number.isFinite(n);
const N = (field, unit, better, label = `rh.n.${field}`) => ({ field, label, unit, better });
const mean = (xs) => (xs.length ? xs.reduce((a, b) => a + b, 0) / xs.length : null);
const asObj = (v) => (v && typeof v === 'object' && !Array.isArray(v) ? v : {});

// aiperf.rs `event()`: run_id, preset, model, verdict, concurrency, requests, isl, osl, duration_ms always (null when unknown);
// ttft_p50_ms, ttft_p99_ms, itl_p50_ms, output_tps, request_tps, error_rate_pct only when the run measured them.
export const AIPERF = {
  system: 'aiperf',
  kind: 'aiperf.run',
  id: 'run_id',
  verdict: 'verdict',
  label: ['preset', 'model'],
  numbers: [
    N('ttft_p50_ms', 'ms', 'lower'),
    N('ttft_p99_ms', 'ms', 'lower'),
    N('itl_p50_ms', 'ms', 'lower'),
    N('output_tps', 'tps', 'higher'),
    N('request_tps', 'rps', 'higher'),
    N('error_rate_pct', 'pct', 'lower'),
    N('concurrency', 'int', null),
    N('requests', 'int', null),
    N('isl', 'int', null),
    N('osl', 'int', null),
    N('duration_ms', 'dur', null, 'rh.n.duration'),
  ],
  groupBy: ['model', 'preset'],
  filters: ['model', 'preset', 'verdict'],
};

// evaluator.rs `event()`: run_id, suite, model, verdict, duration_ms, and benchmarks: [{bench, status, metric, score, samples}]
// (at most 8). Each benchmark becomes the number `bench:<name>` (its score); `bench_mean` is the mean of those scores.
export const EVALUATOR = {
  system: 'evaluator',
  kind: 'evaluator.run',
  id: 'run_id',
  verdict: 'verdict',
  label: ['suite', 'model'],
  flatten(data) {
    const out = {};
    const scores = [];
    for (const b of Array.isArray(data.benchmarks) ? data.benchmarks : []) {
      if (!b || typeof b.bench !== 'string' || !isNum(b.score)) continue;
      out[`bench:${b.bench}`] = b.score;
      scores.push(b.score);
    }
    if (scores.length) out.bench_mean = mean(scores);
    return out;
  },
  numbers: [N('bench_mean', 'score', 'higher'), N('duration_ms', 'dur', null, 'rh.n.duration')],
  dynamic: [{ prefix: 'bench:', label: 'rh.n.bench', unit: 'score', better: 'higher' }],
  groupBy: ['model', 'suite'],
  filters: ['model', 'suite', 'verdict'],
};

// gym.rs `event()`: run_id, model, limit, verdict, duration_ms always; rollouts, rewarded, mean_reward, pass_at_1 only for a run
// that has metrics.
export const GYM = {
  system: 'gym',
  kind: 'gym.smoke',
  id: 'run_id',
  verdict: 'verdict',
  label: (f) => (isNum(f.limit) ? t('rh.lbl.gym', { model: f.model ?? '–', limit: f.limit }) : (f.model ?? null)),
  numbers: [
    N('mean_reward', 'score', 'higher'),
    N('pass_at_1', 'ratio', 'higher'),
    N('rewarded', 'int', 'higher'),
    N('rollouts', 'int', null),
    N('limit', 'int', null),
    N('duration_ms', 'dur', null, 'rh.n.duration'),
  ],
  groupBy: ['model', 'limit'],
  filters: ['model', 'limit', 'verdict'],
};

// modelopt.rs `event()`: job_id, recipe, method, base_model, output, verdict, duration_ms always; output_bytes, base_bytes, ratio
// (output / base) only for a job that has metrics. A lower ratio is a smaller model.
export const MODELOPT = {
  system: 'modelopt',
  kind: 'modelopt.job',
  id: 'job_id',
  verdict: 'verdict',
  label: ['recipe', 'method'],
  numbers: [N('ratio', 'ratio', 'lower', 'rh.n.mo_ratio'), N('output_bytes', 'bytes', 'lower'), N('base_bytes', 'bytes', null), N('duration_ms', 'dur', 'lower', 'rh.n.duration')],
  groupBy: ['recipe', 'method', 'base_model'],
  filters: ['recipe', 'method', 'base_model', 'verdict'],
};

// skillevaluator.rs `event()`: run_id, skill, tiers ('static' | 'tier1' | 'tier1+2' | 'tier3'), verdict, model_calls, tier1
// {status, quality_score, grade, rubric_score, errors, warnings, high, critical, incomplete}, tier2_status, duration_ms; for a
// Tier 3 run also tier3 {execution_status, scored_attempts, expected_attempts, duration_seconds, agents: {name: {model,
// skill_lift, with_skill, without_skill, execution_status}}}. Flattened to quality_score, rubric_score, errors, warnings, high,
// critical, grade, scored_attempts, and for the agents: `lift:<agent>`; `lift` / `with_skill` / `without_skill` = the first agent
// that has a lift (the one the run's headline shows).
export const SKILLEVAL = {
  system: 'skillevaluator',
  kind: 'skilleval.run',
  // the verdict is the measured outcome: compare and judge failed runs too
  outcomeVerdict: true,
  id: 'run_id',
  verdict: 'verdict',
  label: ['skill', 'tiers'],
  flatten(data) {
    const t1 = asObj(data.tier1),
      t3 = asObj(data.tier3);
    const out = {};
    for (const k of ['quality_score', 'rubric_score', 'errors', 'warnings', 'high', 'critical']) if (isNum(t1[k])) out[k] = t1[k];
    if (typeof t1.grade === 'string') out.grade = t1.grade;
    if (isNum(t3.scored_attempts)) out.scored_attempts = t3.scored_attempts;
    let headline = false;
    for (const [name, a0] of Object.entries(asObj(t3.agents))) {
      const a = asObj(a0);
      if (!isNum(a.skill_lift)) continue;
      out[`lift:${name}`] = a.skill_lift;
      if (!headline) {
        headline = true;
        out.lift = a.skill_lift;
        if (isNum(a.with_skill)) out.with_skill = a.with_skill;
        if (isNum(a.without_skill)) out.without_skill = a.without_skill;
      }
    }
    return out;
  },
  numbers: [
    N('quality_score', 'score1', 'higher'),
    N('rubric_score', 'score1', 'higher'),
    N('errors', 'int', 'lower'),
    N('warnings', 'int', 'lower'),
    N('high', 'int', 'lower'),
    N('critical', 'int', 'lower'),
    N('lift', 'signed', 'higher'),
    N('with_skill', 'score', 'higher'),
    N('without_skill', 'score', null),
    N('scored_attempts', 'int', null),
    N('duration_ms', 'dur', null, 'rh.n.duration'),
  ],
  dynamic: [{ prefix: 'lift:', label: 'rh.n.lift_agent', unit: 'signed', better: 'higher' }],
  groupBy: ['skill', 'tiers', 'grade'],
  filters: ['skill', 'tiers', 'grade', 'verdict'],
};

// datadesigner.rs `event()`: run_id, script, mode ('preview' | 'create'), verdict, num_records_requested, dataset_name,
// destination, format, batches, duration_ms always (null when unknown), and num_records (the records written, from `records`).
// `records_per_s` = records / seconds, derived.
export const DATADESIGNER = {
  system: 'datadesigner',
  kind: 'dd.run',
  id: 'run_id',
  verdict: 'verdict',
  label: ['script', 'mode'],
  flatten(data) {
    return isNum(data.num_records) && isNum(data.duration_ms) && data.duration_ms > 0 ? { records_per_s: data.num_records / (data.duration_ms / 1000) } : {};
  },
  numbers: [N('records_per_s', 'num', 'higher'), N('num_records', 'int', null), N('num_records_requested', 'int', null), N('batches', 'int', null), N('duration_ms', 'dur', 'lower', 'rh.n.duration')],
  groupBy: ['script', 'mode', 'format'],
  filters: ['script', 'mode', 'format', 'verdict'],
};

// dcgm.rs `diag_event()`: run_id, gpu, level, verdict ('pass' | 'warn' | 'fail' | 'error'), duration_ms, model, and tests: a
// map {test name: status text} (at most 20). Flattened to tests_total / tests_passed / pass_rate and one `test:<name>` per test
// (1 = pass, 0.5 = warn, 0 = fail; a skipped or unknown status has no value).
export const DCGM = {
  system: 'dcgm',
  kind: 'dcgm.diag',
  // the verdict is the measured outcome: compare and judge failed runs too
  outcomeVerdict: true,
  id: 'run_id',
  verdict: 'verdict',
  label: (f) => t('rh.lbl.dcgm', { gpu: f.gpu ?? '–', level: f.level ?? '–' }),
  flatten(data) {
    const tests = Object.entries(asObj(data.tests));
    if (!tests.length) return {};
    const out = { tests_total: tests.length };
    let passed = 0;
    for (const [name, status] of tests) {
      const s = String(status).toLowerCase();
      if (s.startsWith('pass')) {
        passed++;
        out[`test:${name}`] = 1;
      } else if (s.startsWith('warn')) out[`test:${name}`] = 0.5;
      else if (s.startsWith('fail')) out[`test:${name}`] = 0;
    }
    out.tests_passed = passed;
    out.pass_rate = passed / tests.length;
    return out;
  },
  numbers: [N('pass_rate', 'ratio', 'higher'), N('tests_passed', 'int', 'higher'), N('tests_total', 'int', null), N('duration_ms', 'dur', null, 'rh.n.duration')],
  dynamic: [{ prefix: 'test:', label: 'rh.n.test', unit: 'passfail', better: 'higher' }],
  groupBy: ['gpu', 'level', 'model'],
  filters: ['gpu', 'level', 'verdict'],
};

// guardrails.rs `check` action event (the only run-like event it emits): config, ok, results: [{probe, status, rail, ms}]. It has
// no run id, no verdict and no duration, so the id is the event time, the verdict is ok -> 'passed' / else 'failed', and the
// numbers are derived: `probe:<name>` (1 when the probe's status is the one the rails should give, 0 when not),
// `probe_ms:<name>`, probes_passed / probes_total and total_ms. EXPECTED mirrors PROBES in guardrails.rs.
export const GUARDRAILS_EXPECTED = { benign: 'passed', jailbreak: 'blocked', harmful: 'blocked' };
export const GUARDRAILS = {
  system: 'guardrails',
  kind: 'guardrails.check',
  // the verdict is the measured outcome: compare and judge failed runs too
  outcomeVerdict: true,
  id: (f, ev) => (isNum(ev?.ts) ? `chk${Math.round(ev.ts * 1000)}` : null),
  verdict: (f) => (typeof f.ok === 'boolean' ? (f.ok ? 'passed' : 'failed') : (f.verdict ?? null)),
  label: ['config'],
  flatten(data) {
    const results = Array.isArray(data.results) ? data.results : [];
    if (!results.length) return {};
    const out = {};
    let passed = 0,
      total = 0,
      ms = 0,
      haveMs = false;
    for (const r of results) {
      if (!r || typeof r.probe !== 'string') continue;
      total++;
      const want = GUARDRAILS_EXPECTED[r.probe];
      if (want != null) {
        const ok = r.status === want;
        out[`probe:${r.probe}`] = ok ? 1 : 0;
        if (ok) passed++;
      }
      if (isNum(r.ms)) {
        out[`probe_ms:${r.probe}`] = r.ms;
        ms += r.ms;
        haveMs = true;
      }
    }
    out.probes_total = total;
    out.probes_passed = passed;
    if (haveMs) out.total_ms = ms;
    return out;
  },
  numbers: [N('probes_passed', 'int', 'higher'), N('probes_total', 'int', null), N('total_ms', 'ms', 'lower')],
  dynamic: [
    { prefix: 'probe:', label: 'rh.n.probe', unit: 'passfail', better: 'higher' },
    { prefix: 'probe_ms:', label: 'rh.n.probe_ms', unit: 'ms', better: 'lower' },
  ],
  groupBy: ['config'],
  filters: ['config', 'verdict'],
};

export const RUN_DESCRIPTORS = { aiperf: AIPERF, evaluator: EVALUATOR, gym: GYM, modelopt: MODELOPT, skillevaluator: SKILLEVAL, datadesigner: DATADESIGNER, dcgm: DCGM, guardrails: GUARDRAILS };

/** Every i18n key a descriptor (and the shared field labels) needs: used by the unit test to prove en.json has them all. */
export function descriptorKeys(desc) {
  const keys = new Set();
  for (const n of [...(desc.numbers || []), ...(desc.dynamic || [])]) keys.add(n.label);
  for (const f of [...(desc.groupBy || []), ...(desc.filters || []), ...(Array.isArray(desc.label) ? desc.label : [])]) keys.add(`rh.f.${f}`);
  return [...keys];
}
