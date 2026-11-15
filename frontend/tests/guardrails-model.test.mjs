// The Guardrails probe matrix and rail pipeline models (addendum 9.16).
import test from 'node:test';
import assert from 'node:assert/strict';
import { probeMatrix, probeCell, shapeRails, pipelineStages, statusWord, PROBES, MAX_RUNS, TIMEOUT_MS } from '../js/guardrails-model.js';
import { shapeRuns } from '../js/runs.js';
import { GUARDRAILS } from '../js/run-descriptors.js';

const T0 = 1_790_000_000;
const ev = (i, results, config = 'lab-input-output', ok = true) => ({ ts: T0 + i * 600, system: 'guardrails', kind: 'guardrails.check', summary: `check ${config}`, data: { config, ok, results } });
const clean = (ms = 500) => [
  { probe: 'benign', status: 'passed', rail: null, ms },
  { probe: 'jailbreak', status: 'blocked', rail: 'self check input', ms: ms + 100 },
  { probe: 'harmful', status: 'blocked', rail: 'self check input', ms: ms + 200 },
];
const rowsOf = (events) => shapeRuns(events, GUARDRAILS);

test('the three fixed probes in the adapter order; passed reads as allowed', () => {
  assert.deepEqual(PROBES, ['benign', 'jailbreak', 'harmful']);
  assert.equal(statusWord('passed'), 'allowed');
  assert.equal(statusWord('blocked'), 'blocked');
  assert.equal(statusWord(null), null);
});

test('a cell compares the answer with what the rails should give: match, slip, error, not reached', () => {
  const results = [
    { probe: 'benign', status: 'passed', ms: 400 },
    { probe: 'jailbreak', status: 'passed', rail: null, ms: 700 }, // a jailbreak let through
    { probe: 'harmful', status: 'error', ms: 90001 },
  ];
  const b = probeCell(results, 'benign');
  assert.equal(b.match, true);
  assert.equal(b.word, 'allowed');
  assert.equal(b.expectedWord, 'allowed');
  const j = probeCell(results, 'jailbreak');
  assert.equal(j.match, false);
  assert.equal(j.error, false);
  assert.equal(j.expectedWord, 'blocked');
  assert.equal(j.word, 'allowed');
  const h = probeCell(results, 'harmful');
  assert.equal(h.error, true);
  assert.equal(h.match, false);
  assert.equal(h.timeout, true);
  assert.equal(probeCell([], 'benign'), null);
  assert.equal(probeCell(undefined, 'benign'), null);
  assert.ok(TIMEOUT_MS >= 90000);
});

test('the matrix: a column per run oldest first, a row per probe, cells and latency on the same grid', () => {
  const events = [
    ev(2, clean(300)),
    ev(0, clean(500)),
    ev(
      1,
      [
        { probe: 'benign', status: 'passed', ms: 450 },
        { probe: 'jailbreak', status: 'passed', ms: 600 },
        { probe: 'harmful', status: 'blocked', ms: 640 },
      ],
      'lab-default',
      false,
    ),
  ];
  const m = probeMatrix(rowsOf(events));
  assert.equal(m.cols.length, 3);
  assert.deepEqual(
    m.cols.map((c) => c.ts),
    [T0, T0 + 600, T0 + 1200],
  );
  assert.deepEqual(
    m.probes.map((p) => [p.name, p.expectedWord]),
    [
      ['benign', 'allowed'],
      ['jailbreak', 'blocked'],
      ['harmful', 'blocked'],
    ],
  );
  assert.deepEqual(
    m.cells.jailbreak.map((c) => c.match),
    [true, false, true],
  );
  assert.deepEqual(m.latency.benign, [500, 450, 300]);
  assert.equal(m.cols[1].config, 'lab-default');
  assert.equal(m.stats.runs, 3);
  assert.equal(m.stats.slips, 1);
  assert.equal(m.stats.clean, 2);
  assert.equal(m.stats.perProbe.jailbreak.mismatch, 1);
  assert.equal(m.stats.perProbe.benign.p50, 450);
  assert.equal(m.maxMs, 700);
});

test('a probe a run did not reach is null in the cell AND the latency, never "passed" and never 0 ms', () => {
  // the check stops at the first probe that errors
  const stopped = ev(
    0,
    [
      { probe: 'benign', status: 'passed', ms: 410 },
      { probe: 'jailbreak', status: 'error', rail: null, ms: 90001 },
    ],
    'lab-input-output',
    false,
  );
  const m = probeMatrix(rowsOf([stopped]));
  assert.equal(m.cells.benign[0].match, true);
  assert.equal(m.cells.jailbreak[0].error, true);
  assert.equal(m.cells.harmful[0], null);
  assert.equal(m.latency.harmful[0], null);
  assert.equal(m.stats.errors, 1);
  assert.equal(m.stats.slips, 0, 'an error is a probe that did not run, not a policy slip');
  assert.equal(m.maxMs, 410, 'a timeout does not stretch the latency scale');
});

test('only the newest `max` runs are drawn and the rest are counted', () => {
  const events = Array.from({ length: MAX_RUNS + 6 }, (_, i) => ev(i, clean()));
  const m = probeMatrix(rowsOf(events));
  assert.equal(m.cols.length, MAX_RUNS);
  assert.equal(m.hidden, 6);
  assert.equal(m.cols.at(-1).ts, T0 + (MAX_RUNS + 5) * 600, 'the newest is kept');
  assert.equal(probeMatrix(rowsOf(events), { max: 10 }).cols.length, 10);
});

test('an unknown extra probe still gets its row (no expectation: match is null) and the label has the date when the runs span days', () => {
  const r = [...clean(), { probe: 'pii', status: 'blocked', ms: 300 }];
  const m = probeMatrix(rowsOf([ev(0, r), { ...ev(0, r), ts: T0 + 3 * 86400 }]));
  assert.deepEqual(
    m.probes.map((p) => p.name),
    [...PROBES, 'pii'],
  );
  assert.equal(m.cells.pii[0].match, null);
  assert.match(m.cols[0].label, /^\d\d-\d\d \d\d:\d\d$/);
  const same = probeMatrix(rowsOf([ev(0, clean()), ev(1, clean())]));
  assert.match(same.cols[0].label, /^\d\d:\d\d$/);
});

test('no events, or events without results: an empty matrix, no crash', () => {
  const m = probeMatrix(rowsOf([]));
  assert.equal(m.cols.length, 0);
  assert.equal(m.maxMs, null);
  assert.equal(probeMatrix(null).cols.length, 0);
  const bare = probeMatrix(rowsOf([{ ts: T0, kind: 'guardrails.check', data: { config: 'x', ok: true } }]));
  assert.equal(bare.cols.length, 1);
  assert.ok(bare.cells.benign[0] === null);
});

// ---- rails

test('shapeRails keeps the flow order, the streaming settings and the retrieval flows; absent fields are null, never invented', () => {
  const m = shapeRails({
    configs: [
      { id: 'a', main_model: 'm1', input: ['x', 'y'], output: ['z'], streaming: { enabled: true, chunk_size: 200, context_size: 50 }, retrieval: ['r'] },
      { id: 'b', main_model: null, input: [], output: [], streaming: null, retrieval: [] },
    ],
    problems: [
      { config: 'b', code: 'config.yaml_error', count: null },
      { config: null, code: 'configs.capped', count: 3 },
    ],
  });
  assert.equal(m.configs.length, 2);
  assert.deepEqual(m.configs[0].input, ['x', 'y']);
  assert.deepEqual(m.configs[0].streaming, { enabled: true, chunkSize: 200, contextSize: 50 });
  assert.equal(m.configs[0].rails, 3);
  assert.equal(m.configs[1].mainModel, null);
  assert.equal(m.configs[1].streaming, null);
  assert.deepEqual(m.problems, [
    { config: 'b', code: 'config.yaml_error', count: null },
    { config: null, code: 'configs.capped', count: 3 },
  ]);
});

test('shapeRails is tolerant: stray non-strings dropped, missing arrays empty, garbage is an empty list', () => {
  const m = shapeRails({ configs: [{ id: 'a', input: ['ok', 3, null, '  ', { x: 1 }], output: 'nope', streaming: { enabled: 'yes', chunk_size: -1 } }, { nope: true }, null, { id: '' }] });
  assert.equal(m.configs.length, 1);
  assert.deepEqual(m.configs[0].input, ['ok']);
  assert.deepEqual(m.configs[0].output, []);
  assert.deepEqual(m.configs[0].streaming, { enabled: null, chunkSize: null, contextSize: null });
  for (const bad of [null, undefined, 'x', 3, [], {}]) assert.deepEqual(shapeRails(bad), { configs: [], problems: [] });
  assert.deepEqual(
    shapeRails({ problems: [{ code: '' }, { config: 'a' }, null, { code: 'flows.dropped', config: 'a', count: -2 }] }).problems,
    [{ config: 'a', code: 'flows.dropped', count: null }],
    'a problem needs a code; a count must be a non-negative number',
  );
});

test('the pipeline stages are input rails, the main model, output rails; an empty stage keeps its place and says none', () => {
  const [cfg] = shapeRails({ configs: [{ id: 'a', main_model: 'm', input: ['x'], output: [] }] }).configs;
  const st = pipelineStages(cfg);
  assert.deepEqual(
    st.map((s) => s.kind),
    ['input', 'model', 'output'],
  );
  assert.equal(st[0].none, false);
  assert.equal(st[1].model, 'm');
  assert.equal(st[2].none, true);
});
