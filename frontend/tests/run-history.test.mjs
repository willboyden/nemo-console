// The run history data layer (addendum 9.13): descriptors against event data shaped like the Rust adapters' SystemEvent.data,
// flattening, filtering, grouping, A/B compare with better / worse, missing fields, and the URL-hash selection.
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { setMessages } from '../js/i18n.js';
import {
  shapeRuns,
  resolveNumbers,
  enumValues,
  applyFilters,
  valueOf,
  trendGroups,
  compareRuns,
  resolveRunState,
  stateToQuery,
  fmtRunValue,
  fmtDelta,
  fmtAxis,
  getField,
  verdictClass,
  VERDICT_GLYPH,
} from '../js/runs.js';
import { RUN_DESCRIPTORS, AIPERF, EVALUATOR, GYM, MODELOPT, SKILLEVAL, DATADESIGNER, DCGM, GUARDRAILS, descriptorKeys, GUARDRAILS_EXPECTED } from '../js/run-descriptors.js';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const en = JSON.parse(fs.readFileSync(path.join(root, 'i18n/en.json'), 'utf8'));
setMessages(en);

const ev = (kind, ts, data, summary = '') => ({ ts, system: 'x', kind, summary, data });
const labelOf = (def, suffix) => (suffix == null ? def.label : `${def.label}:${suffix}`);

const aiperf = [
  ev(
    'aiperf.run',
    300,
    {
      run_id: 'r3',
      preset: 'chat-short',
      model: 'glm',
      verdict: 'succeeded',
      concurrency: 8,
      requests: 200,
      isl: 512,
      osl: 128,
      duration_ms: 90000,
      ttft_p50_ms: 150,
      ttft_p99_ms: 400,
      itl_p50_ms: 18,
      output_tps: 1900,
      request_tps: 12.5,
      error_rate_pct: 0.5,
    },
    'chat-short glm',
  ),
  ev('aiperf.run', 100, {
    run_id: 'r1',
    preset: 'chat-short',
    model: 'glm',
    verdict: 'succeeded',
    concurrency: 8,
    requests: 200,
    isl: 512,
    osl: 128,
    duration_ms: 95000,
    ttft_p50_ms: 200,
    ttft_p99_ms: 520,
    itl_p50_ms: 20,
    output_tps: 1750,
    request_tps: 11,
    error_rate_pct: 0,
  }),
  ev('aiperf.run', 200, { run_id: 'r2', preset: 'burst', model: 'lightning', verdict: 'failed', concurrency: null, requests: null, isl: null, osl: null, duration_ms: null }),
  ev('other.kind', 250, { run_id: 'zzz' }),
];

test('shapeRuns: oldest first, only the descriptor kind, ids unique, labels from the descriptor fields', () => {
  const rows = shapeRuns(aiperf, AIPERF);
  assert.deepEqual(
    rows.map((r) => r.id),
    ['r1', 'r2', 'r3'],
  );
  assert.equal(rows[0].label, 'chat-short · glm');
  assert.equal(rows[1].verdict, 'failed');
  assert.equal(rows[1].vclass, 'bad');
  assert.equal(rows[0].summary, '');
  assert.equal(rows[2].summary, 'chat-short glm');
  const dup = shapeRuns([ev('aiperf.run', 1, { run_id: 'a', verdict: 'succeeded' }), ev('aiperf.run', 2, { run_id: 'a', verdict: 'failed' })], AIPERF);
  assert.deepEqual(
    dup.map((r) => r.id),
    ['a', 'a#2'],
  );
  assert.deepEqual(shapeRuns(null, AIPERF), []);
  assert.deepEqual(shapeRuns([{ kind: 'aiperf.run' }, null, ev('aiperf.run', 5, null)].filter(Boolean), AIPERF).length, 2, 'an event without data still makes a row');
});

test('missing fields are null, never zero: a failed run has no numbers and a null field is not 0', () => {
  const rows = shapeRuns(aiperf, AIPERF);
  const failed = rows.find((r) => r.id === 'r2');
  assert.equal(valueOf(failed, 'ttft_p50_ms'), null);
  assert.equal(valueOf(failed, 'concurrency'), null, 'null in the event stays null');
  assert.equal(valueOf(rows[0], 'error_rate_pct'), 0, 'a real zero is still a zero');
  assert.equal(valueOf({ fields: { x: '5' } }, 'x'), null, 'a string is not a number');
  assert.equal(valueOf({ fields: { x: NaN } }, 'x'), null);
  assert.equal(fmtRunValue(null, 'ms'), '–');
  assert.equal(fmtRunValue(undefined, 'int'), '–');
  assert.equal(fmtRunValue(0, 'ms'), '0.0 ms');
  assert.equal(fmtDelta(null, 'ms'), '–');
});

test('a run without an id or time still gets a stable id', () => {
  const rows = shapeRuns([ev('aiperf.run', 0, { verdict: 'succeeded' }), ev('aiperf.run', 1700000000.5, { verdict: 'failed' })], AIPERF);
  assert.equal(rows.length, 2);
  assert.ok(rows.every((r) => r.id.length > 1));
  assert.equal(new Set(rows.map((r) => r.id)).size, 2);
  assert.equal(rows[0].ts, null, 'ts 0 is "unknown time", not 1970');
});

test('resolveNumbers: only numbers some run has a value for; dynamic families expand from the fields seen, sorted', () => {
  const rows = shapeRuns(aiperf, AIPERF);
  const nums = resolveNumbers(rows, AIPERF, labelOf);
  assert.ok(nums.some((n) => n.field === 'ttft_p50_ms' && n.better === 'lower' && n.unit === 'ms'));
  assert.ok(nums.some((n) => n.field === 'output_tps' && n.better === 'higher'));
  assert.ok(!nums.some((n) => n.field === 'nothing'));
  const onlyFailed = resolveNumbers(shapeRuns([aiperf[2]], AIPERF), AIPERF, labelOf);
  assert.deepEqual(onlyFailed, [], 'a failed run alone has nothing to chart');
});

const evaluator = [
  ev('evaluator.run', 10, {
    run_id: 'e1',
    suite: 'smoke',
    model: 'glm',
    verdict: 'succeeded',
    duration_ms: 60000,
    benchmarks: [
      { bench: 'gsm8k', status: 'ok', metric: 'acc', score: 0.8, samples: 100 },
      { bench: 'ifeval', status: 'ok', metric: 'acc', score: 0.6, samples: 50 },
    ],
  }),
  ev('evaluator.run', 20, {
    run_id: 'e2',
    suite: 'smoke',
    model: 'glm',
    verdict: 'succeeded',
    duration_ms: 61000,
    benchmarks: [
      { bench: 'gsm8k', status: 'ok', metric: 'acc', score: 0.85, samples: 100 },
      { bench: 'mmlu_pro', status: 'ok', metric: 'acc', score: 0.5, samples: 10 },
      { bench: 'broken', status: 'error', metric: null, score: null, samples: null },
    ],
  }),
  ev('evaluator.run', 30, { run_id: 'e3', suite: 'quick', model: 'lightning', verdict: 'failed', duration_ms: 1000, benchmarks: [] }),
];

test('evaluator: benchmarks are flattened to bench:<name> numbers, a benchmark with no score is absent, bench_mean averages the scores', () => {
  const rows = shapeRuns(evaluator, EVALUATOR);
  assert.equal(getField(rows[0].fields, 'bench:gsm8k'), 0.8);
  assert.equal(getField(rows[1].fields, 'bench:broken'), undefined);
  assert.ok(Math.abs(valueOf(rows[0], 'bench_mean') - 0.7) < 1e-12);
  assert.equal(valueOf(rows[2], 'bench_mean'), null, 'no benchmarks, no mean (not 0)');
  const nums = resolveNumbers(rows, EVALUATOR, labelOf);
  assert.deepEqual(
    nums.filter((n) => n.dynamic).map((n) => n.field),
    ['bench:gsm8k', 'bench:ifeval', 'bench:mmlu_pro'],
  );
  assert.equal(nums.find((n) => n.field === 'bench:ifeval').label, 'rh.n.bench:ifeval');
  assert.equal(nums.find((n) => n.field === 'bench:gsm8k').better, 'higher');
});

test("compare: A is the baseline, the difference is B minus A, better / worse follow the number's direction", () => {
  const rows = shapeRuns(aiperf, AIPERF);
  const nums = resolveNumbers(rows, AIPERF, labelOf);
  const [r1, , r3] = rows;
  const cmp = Object.fromEntries(compareRuns(r1, r3, nums).map((c) => [c.field, c]));
  assert.equal(cmp.ttft_p50_ms.diff, -50);
  assert.equal(cmp.ttft_p50_ms.mark, 'better', 'lower latency is better');
  assert.equal(cmp.ttft_p50_ms.pct, -0.25);
  assert.equal(cmp.output_tps.diff, 150);
  assert.equal(cmp.output_tps.mark, 'better', 'higher throughput is better');
  assert.equal(cmp.error_rate_pct.diff, 0.5);
  assert.equal(cmp.error_rate_pct.mark, 'worse', 'a higher error rate is worse');
  assert.equal(cmp.error_rate_pct.pct, null, 'no percentage against a zero baseline');
  assert.equal(cmp.concurrency.mark, 'same');
  assert.equal(cmp.duration_ms.mark, 'none', 'no direction configured: a difference, no verdict');
  const swapped = Object.fromEntries(compareRuns(r3, r1, nums).map((c) => [c.field, c]));
  assert.equal(swapped.ttft_p50_ms.mark, 'worse');
});

test('compare: a number only one run has is not a difference; a number neither has is left out', () => {
  const rows = shapeRuns(aiperf, AIPERF);
  const nums = resolveNumbers(rows, AIPERF, labelOf);
  const cmp = compareRuns(rows[0], rows[1], nums); // r1 (full) vs r2 (failed: no numbers)
  const t50 = cmp.find((c) => c.field === 'ttft_p50_ms');
  assert.equal(t50.b, null);
  assert.equal(t50.diff, null);
  assert.equal(t50.mark, 'na');
  assert.equal(compareRuns(rows[1], rows[1], nums).length, 0, 'a failed run against itself has nothing in common');
  assert.deepEqual(compareRuns(null, null, nums), []);
});

test('compare: no better/worse mark when either run did not succeed (a failed run that stopped early is not faster)', () => {
  const ok = { vclass: 'ok', fields: { duration_ms: 8000 } };
  const bad = { vclass: 'bad', fields: { duration_ms: 2000 } };
  const num = [{ field: 'duration_ms', label: 'Duration', unit: 'ms', better: 'lower' }];
  const [row] = compareRuns(ok, bad, num);
  assert.equal(row.diff, -6000);
  assert.equal(row.mark, 'none');
  assert.equal(compareRuns(ok, { vclass: 'ok', fields: { duration_ms: 2000 } }, num)[0].mark, 'better');
  assert.equal(compareRuns(ok, bad, num, { outcomeVerdict: true })[0].mark, 'better', 'diagnostic-type systems judge every pair');
  for (const d of [SKILLEVAL, DCGM, GUARDRAILS]) assert.equal(d.outcomeVerdict, true);
});

test('trend groups: by a field, most frequent first, a run without the number is not a point, a run without the field is in the dash group', () => {
  const rows = shapeRuns(aiperf, AIPERF);
  const def = { field: 'ttft_p50_ms' };
  const g = trendGroups(rows, def, 'preset');
  assert.deepEqual(
    g.map((x) => [x.key, x.count, x.points.length]),
    [
      ['chat-short', 2, 2],
      ['burst', 1, 0],
    ],
  );
  assert.deepEqual(
    g[0].points.map((p) => p.i),
    [0, 2],
    'i is the run position on the axis',
  );
  const none = trendGroups(rows, def, '');
  assert.equal(none.length, 1);
  assert.equal(none[0].points.length, 2);
  const noField = trendGroups(rows, def, 'no_such_field');
  assert.deepEqual(
    noField.map((x) => x.key),
    ['—'],
  );
});

test('filters: enum values are counted from what was seen; a filter keeps only matching runs', () => {
  const rows = shapeRuns(aiperf, AIPERF);
  const seen = enumValues(rows, ['model', 'preset', 'verdict', 'nothing']);
  assert.deepEqual(seen.model, [
    { value: 'glm', count: 2 },
    { value: 'lightning', count: 1 },
  ]);
  assert.deepEqual(
    seen.verdict.map((x) => x.value),
    ['succeeded', 'failed'],
  );
  assert.deepEqual(seen.nothing, []);
  assert.deepEqual(
    applyFilters(rows, { model: 'glm' }).map((r) => r.id),
    ['r1', 'r3'],
  );
  assert.deepEqual(applyFilters(rows, { model: 'glm', verdict: 'failed' }), []);
  assert.deepEqual(
    applyFilters(rows, { verdict: 'failed' }).map((r) => r.id),
    ['r2'],
  );
  assert.equal(applyFilters(rows, {}).length, 3);
  assert.equal(applyFilters(rows, { model: '' }).length, 3, 'an empty value is no filter');
});

test('hash state: run / vs / metric / group / filters are validated against what exists, defaults are left out of the URL', () => {
  const rows = shapeRuns(aiperf, AIPERF);
  const numbers = resolveNumbers(rows, AIPERF, labelOf);
  const ctx = { rows, numbers, desc: AIPERF };
  const st = resolveRunState({ run: 'r1', vs: 'r3', metric: 'output_tps', group: 'preset', 'f.model': 'glm', 'f.verdict': 'nonsense', 'f.evil': 'x' }, ctx);
  assert.deepEqual(st, { run: 'r1', vs: 'r3', metric: 'output_tps', group: 'preset', filters: { model: 'glm' } });
  const bad = resolveRunState({ run: 'ghost', vs: 'r1', metric: 'ghost', group: 'ghost' }, ctx);
  assert.equal(bad.run, null);
  assert.equal(bad.vs, 'r1', 'a valid vs stays even when run is unknown');
  assert.equal(bad.metric, numbers[0].field, 'unknown metric falls back to the first');
  assert.equal(bad.group, 'model', 'unknown group falls back to the first groupBy');
  assert.equal(resolveRunState({ run: 'r1', vs: 'r1' }, ctx).vs, null, 'a run is not compared with itself');
  assert.equal(resolveRunState({ group: 'none' }, ctx).group, '');
  const q = stateToQuery(st, ctx);
  assert.deepEqual(q, { run: 'r1', vs: 'r3', metric: 'output_tps', group: 'preset', 'f.model': 'glm' });
  const def = stateToQuery(resolveRunState({}, ctx), ctx);
  assert.deepEqual(def, { run: null, vs: null }, 'the default metric and group are not written');
});

test('gym: model and limit label, mean reward / pass@1 higher is better, a run without metrics has no numbers', () => {
  const events = [
    ev('gym.smoke', 1, { run_id: 'g1', model: 'glm', limit: 5, verdict: 'succeeded', duration_ms: 144000, rollouts: 5, rewarded: 4, mean_reward: 0.8, pass_at_1: 0.8 }),
    ev('gym.smoke', 2, { run_id: 'g2', model: 'glm', limit: 5, verdict: 'failed', duration_ms: 9000 }),
  ];
  const rows = shapeRuns(events, GYM);
  assert.equal(rows[0].label, 'glm, 5 tasks');
  assert.equal(valueOf(rows[1], 'mean_reward'), null);
  const nums = resolveNumbers(rows, GYM, labelOf);
  assert.equal(nums.find((n) => n.field === 'pass_at_1').better, 'higher');
  assert.equal(fmtRunValue(0.8, 'ratio'), '80.0%');
});

test('modelopt: a lower ratio and a smaller output are better', () => {
  const rows = shapeRuns(
    [
      ev('modelopt.job', 1, {
        job_id: 'j1',
        recipe: 'reap-glm53',
        method: 'reap',
        base_model: 'glm',
        output: 'o',
        verdict: 'succeeded',
        duration_ms: 3e6,
        output_bytes: 8e10,
        base_bytes: 1e11,
        ratio: 0.8,
      }),
      ev('modelopt.job', 2, {
        job_id: 'j2',
        recipe: 'reap-glm53',
        method: 'reap',
        base_model: 'glm',
        output: 'o',
        verdict: 'succeeded',
        duration_ms: 3e6,
        output_bytes: 6e10,
        base_bytes: 1e11,
        ratio: 0.6,
      }),
    ],
    MODELOPT,
  );
  const cmp = Object.fromEntries(compareRuns(rows[0], rows[1], resolveNumbers(rows, MODELOPT, labelOf)).map((c) => [c.field, c]));
  assert.equal(cmp.ratio.mark, 'better');
  assert.equal(cmp.output_bytes.mark, 'better');
  assert.equal(cmp.base_bytes.mark, 'same');
  assert.equal(rows[0].id, 'j1', 'the id field is job_id');
});

test('skillevaluator: tier1 and tier3 numbers are flattened; the headline lift is the first agent with one; static runs have no lift', () => {
  const events = [
    ev('skilleval.run', 1, {
      run_id: 's1',
      skill: 'a',
      tiers: 'tier1',
      verdict: 'passed',
      model_calls: true,
      tier1: { status: 'passed', quality_score: 87.5, grade: 'B', rubric_score: 82, errors: 0, warnings: 3, high: 0, critical: 0, incomplete: [] },
      tier2_status: null,
      duration_ms: 12000,
    }),
    ev('skilleval.run', 2, {
      run_id: 's2',
      skill: 'a',
      tiers: 'tier3',
      verdict: 'succeeded',
      model_calls: true,
      tier1: null,
      duration_ms: 600000,
      tier3: {
        execution_status: 'succeeded',
        scored_attempts: 8,
        expected_attempts: 8,
        duration_seconds: 590,
        agents: {
          opencode: { model: 'glm', skill_lift: 0.125, with_skill: 0.8, without_skill: 0.675, execution_status: 'succeeded' },
          codex: { model: 'x', skill_lift: null, with_skill: null, without_skill: null, execution_status: 'failed' },
        },
      },
    }),
    ev('skilleval.run', 3, {
      run_id: 's3',
      skill: 'b',
      tiers: 'static',
      verdict: 'failed',
      model_calls: false,
      tier1: { status: 'failed', quality_score: 40, grade: 'D', rubric_score: null, errors: 4, warnings: 0, high: 2, critical: 1, incomplete: ['x'] },
      duration_ms: 900,
    }),
  ];
  const rows = shapeRuns(events, SKILLEVAL);
  assert.equal(valueOf(rows[0], 'quality_score'), 87.5);
  assert.equal(valueOf(rows[0], 'lift'), null);
  assert.equal(valueOf(rows[1], 'lift'), 0.125);
  assert.equal(valueOf(rows[1], 'with_skill'), 0.8);
  assert.equal(getField(rows[1].fields, 'lift:codex'), undefined, 'an agent without a lift is absent');
  assert.equal(valueOf(rows[2], 'rubric_score'), null, 'a null rubric score is not 0');
  assert.equal(valueOf(rows[2], 'critical'), 1);
  assert.equal(rows[0].fields.grade, 'B');
  const nums = resolveNumbers(rows, SKILLEVAL, labelOf);
  assert.deepEqual(
    nums.filter((n) => n.dynamic).map((n) => n.field),
    ['lift:opencode'],
  );
  const cmp = Object.fromEntries(compareRuns(rows[0], rows[2], nums, { outcomeVerdict: SKILLEVAL.outcomeVerdict }).map((c) => [c.field, c]));
  assert.equal(cmp.quality_score.mark, 'worse');
  assert.equal(cmp.errors.mark, 'worse', 'more errors is worse');
  assert.equal(cmp.rubric_score.mark, 'na', 'the static run has no judge score');
});

test('datadesigner: records per second is derived only when both numbers exist', () => {
  const rows = shapeRuns(
    [
      ev('dd.run', 1, {
        run_id: 'd1',
        script: 's.py',
        mode: 'create',
        verdict: 'succeeded',
        num_records_requested: 100,
        dataset_name: 'x',
        destination: 'x',
        format: 'parquet',
        batches: 2,
        duration_ms: 50000,
        num_records: 100,
      }),
      ev('dd.run', 2, {
        run_id: 'd2',
        script: 's.py',
        mode: 'create',
        verdict: 'failed',
        num_records_requested: 100,
        dataset_name: 'x',
        destination: 'x',
        format: 'parquet',
        batches: null,
        duration_ms: 1000,
        num_records: null,
      }),
      ev('dd.run', 3, {
        run_id: 'd3',
        script: 's.py',
        mode: 'preview',
        verdict: 'running',
        num_records_requested: 5,
        dataset_name: null,
        destination: null,
        format: null,
        batches: null,
        duration_ms: null,
        num_records: null,
      }),
    ],
    DATADESIGNER,
  );
  assert.equal(valueOf(rows[0], 'records_per_s'), 2);
  assert.equal(valueOf(rows[1], 'records_per_s'), null);
  assert.equal(valueOf(rows[2], 'records_per_s'), null);
  assert.equal(rows[2].vclass, 'info');
});

test('dcgm: the tests map is flattened; pass is 1, warn 0.5, fail 0, a skipped test has no value', () => {
  const rows = shapeRuns(
    [
      ev('dcgm.diag', 1, { run_id: 'x1', gpu: 0, level: 1, verdict: 'pass', duration_ms: 20000, model: 'RTX', tests: { Deployment: 'Pass', Memory: 'Pass', Skipped: 'Skip' } }),
      ev('dcgm.diag', 2, { run_id: 'x2', gpu: 1, level: 2, verdict: 'fail', duration_ms: 90000, model: 'RTX', tests: { Deployment: 'Pass', Memory: 'Fail', Thermal: 'Warn' } }),
      ev('dcgm.diag', 3, { run_id: 'x3', gpu: 1, level: 1, verdict: 'error', duration_ms: 500, model: 'RTX', tests: {} }),
    ],
    DCGM,
  );
  assert.equal(rows[0].label, 'GPU 0, level 1');
  assert.equal(valueOf(rows[0], 'tests_total'), 3);
  assert.equal(valueOf(rows[0], 'tests_passed'), 2);
  assert.equal(getField(rows[0].fields, 'test:Skipped'), undefined);
  assert.equal(valueOf(rows[1], 'test:Memory'), 0);
  assert.equal(valueOf(rows[1], 'test:Thermal'), 0.5);
  assert.equal(valueOf(rows[2], 'pass_rate'), null, 'no tests, no rate');
  const nums = resolveNumbers(rows, DCGM, labelOf);
  const cmp = Object.fromEntries(compareRuns(rows[0], rows[1], nums, { outcomeVerdict: DCGM.outcomeVerdict }).map((c) => [c.field, c]));
  assert.equal(cmp['test:Memory'].mark, 'worse');
  assert.equal(cmp.pass_rate.mark, 'worse');
  assert.equal(fmtRunValue(0.5, 'passfail'), 'warn');
  assert.equal(rows[0].vclass, 'ok');
  assert.equal(rows[1].vclass, 'bad');
});

test('guardrails: no run id or verdict in the event, so both are derived; probes pass when they match what the rails should do', () => {
  const ok = ev('guardrails.check', 1700000000.25, {
    config: 'lab-input-output',
    ok: true,
    results: [
      { probe: 'benign', status: 'passed', rail: null, ms: 400 },
      { probe: 'jailbreak', status: 'blocked', rail: 'self check input', ms: 600 },
      { probe: 'harmful', status: 'blocked', rail: 'self check input', ms: 700 },
    ],
  });
  const bad = ev('guardrails.check', 1700000100.5, {
    config: 'lab-input-output',
    ok: false,
    results: [
      { probe: 'benign', status: 'passed', rail: null, ms: 300 },
      { probe: 'jailbreak', status: 'passed', rail: null, ms: 350 },
      { probe: 'harmful', status: 'error', rail: null, ms: 90 },
    ],
  });
  const old = ev('guardrails.check', 1700000200, { verdict: 'blocked' }); // an older, differently shaped event
  const rows = shapeRuns([bad, ok, old], GUARDRAILS);
  assert.equal(rows.length, 3);
  assert.equal(rows[0].verdict, 'passed');
  assert.equal(rows[1].verdict, 'failed');
  assert.equal(rows[2].verdict, 'blocked', 'an event without ok keeps its own verdict');
  assert.ok(rows.every((r) => r.id.startsWith('chk')));
  assert.equal(valueOf(rows[0], 'probes_passed'), 3);
  assert.equal(valueOf(rows[1], 'probes_passed'), 1);
  assert.equal(valueOf(rows[1], 'probe:jailbreak'), 0);
  assert.equal(valueOf(rows[0], 'total_ms'), 1700);
  assert.equal(valueOf(rows[2], 'probes_passed'), null, 'no results, no counts');
  assert.deepEqual(GUARDRAILS_EXPECTED, { benign: 'passed', jailbreak: 'blocked', harmful: 'blocked' });
  const nums = resolveNumbers(rows, GUARDRAILS, labelOf);
  assert.deepEqual(
    nums.filter((n) => n.dynamic).map((n) => n.field),
    ['probe:benign', 'probe:harmful', 'probe:jailbreak', 'probe_ms:benign', 'probe_ms:harmful', 'probe_ms:jailbreak'],
  );
  const cmp = Object.fromEntries(compareRuns(rows[0], rows[1], nums, { outcomeVerdict: GUARDRAILS.outcomeVerdict }).map((c) => [c.field, c]));
  assert.equal(cmp['probe_ms:benign'].mark, 'better', 'a lower probe time is better');
  assert.equal(cmp.probes_passed.mark, 'worse');
});

test('every descriptor field has an i18n label, the labels exist in en.json, and the descriptors name real kinds', () => {
  assert.deepEqual(Object.keys(RUN_DESCRIPTORS).sort(), ['aiperf', 'datadesigner', 'dcgm', 'evaluator', 'guardrails', 'gym', 'modelopt', 'skillevaluator']);
  const kinds = {
    aiperf: 'aiperf.run',
    evaluator: 'evaluator.run',
    gym: 'gym.smoke',
    modelopt: 'modelopt.job',
    skillevaluator: 'skilleval.run',
    datadesigner: 'dd.run',
    dcgm: 'dcgm.diag',
    guardrails: 'guardrails.check',
  };
  for (const [sys, d] of Object.entries(RUN_DESCRIPTORS)) {
    assert.equal(d.system, sys);
    assert.equal(d.kind, kinds[sys]);
    for (const k of descriptorKeys(d)) assert.ok(k in en, `${sys}: ${k}`);
    for (const n of d.numbers) {
      assert.ok(['ms', 'dur', 'tps', 'rps', 'pct', 'ratio', 'bytes', 'int', 'score', 'score1', 'signed', 'num'].includes(n.unit), `${sys}.${n.field} unit ${n.unit}`);
      assert.ok([null, 'lower', 'higher'].includes(n.better), `${sys}.${n.field} better`);
    }
    for (const f of d.filters) assert.ok(`rh.f.${f}` in en, `${sys} filter ${f}`);
    assert.ok(d.groupBy.length >= 1 && d.filters.length >= 1, sys);
  }
  for (const v of ['better', 'worse', 'same']) assert.ok(`rh.mark_${v}` in en);
});

test('the descriptors read exactly the fields the adapters put in event data (kept in step by reading the Rust source when it is there)', () => {
  const dir = path.join(root, '..', 'backend', 'crates', 'nemo-console', 'src', 'adapters');
  if (!fs.existsSync(dir)) return;
  const src = (f) => fs.readFileSync(path.join(dir, f), 'utf8');
  const has = (f, ...words) => words.forEach((w) => assert.ok(src(f).includes(`"${w}"`), `${f} should mention "${w}"`));
  has(
    'aiperf.rs',
    'run_id',
    'preset',
    'model',
    'verdict',
    'concurrency',
    'requests',
    'isl',
    'osl',
    'duration_ms',
    'ttft_p50_ms',
    'ttft_p99_ms',
    'itl_p50_ms',
    'output_tps',
    'request_tps',
    'error_rate_pct',
  );
  has('evaluator.rs', 'run_id', 'suite', 'model', 'verdict', 'duration_ms', 'benchmarks', 'bench', 'score');
  has('gym.rs', 'run_id', 'model', 'limit', 'verdict', 'duration_ms', 'rollouts', 'rewarded', 'mean_reward', 'pass_at_1');
  has('modelopt.rs', 'job_id', 'recipe', 'method', 'base_model', 'verdict', 'duration_ms', 'output_bytes', 'base_bytes', 'ratio');
  has(
    'skillevaluator.rs',
    'run_id',
    'skill',
    'tiers',
    'verdict',
    'tier1',
    'tier3',
    'agents',
    'skill_lift',
    'with_skill',
    'without_skill',
    'scored_attempts',
    'quality_score',
    'rubric_score',
    'grade',
    'errors',
    'warnings',
    'high',
    'critical',
  );
  has('datadesigner.rs', 'run_id', 'script', 'mode', 'verdict', 'num_records_requested', 'dataset_name', 'destination', 'format', 'batches', 'duration_ms', 'num_records');
  has('dcgm.rs', 'run_id', 'gpu', 'level', 'verdict', 'duration_ms', 'model', 'tests');
  has('guardrails.rs', 'config', 'ok', 'results', 'probe', 'status', 'ms');
});

test('formatting: units, signed differences, axis ticks and verdict looks', () => {
  assert.equal(fmtRunValue(1840.56, 'tps'), '1841 tok/s');
  assert.equal(fmtRunValue(12.5, 'tps'), '12.5 tok/s');
  assert.equal(fmtRunValue(0.5, 'pct'), '0.5%');
  assert.equal(fmtRunValue(0.8, 'score'), '0.800');
  assert.equal(fmtRunValue(0.125, 'signed'), '+0.125');
  assert.equal(fmtRunValue(-0.1, 'signed'), '-0.100');
  assert.equal(fmtRunValue(90000, 'dur'), '1m 30s');
  assert.equal(fmtRunValue(2 * 1024 ** 3, 'bytes'), '2.00 GiB');
  assert.equal(fmtRunValue(1234, 'int'), '1,234');
  assert.equal(fmtDelta(-50, 'ms'), '−50 ms');
  assert.equal(fmtDelta(150, 'tps'), '+150 tok/s');
  assert.equal(fmtDelta(0.05, 'ratio'), '+5.0 pp');
  assert.equal(fmtDelta(0, 'int'), '0');
  assert.equal(fmtAxis(0.87, 'ratio'), '87%');
  assert.equal(fmtAxis(12345, 'int'), '12k');
  assert.equal(fmtAxis(null, 'ms'), '–');
  for (const v of ['succeeded', 'passed', 'pass']) assert.equal(verdictClass(v), 'ok');
  for (const v of ['failed', 'fail', 'error']) assert.equal(verdictClass(v), 'bad');
  for (const v of ['interrupted', 'incomplete', 'warn', 'unknown']) assert.equal(verdictClass(v), 'warn');
  assert.equal(verdictClass('running'), 'info');
  assert.equal(verdictClass('whatever'), 'muted');
  assert.equal(verdictClass(undefined), 'muted');
  assert.deepEqual(Object.keys(VERDICT_GLYPH).sort(), ['bad', 'info', 'muted', 'ok', 'warn']);
});
