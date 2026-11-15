// NeMo Evaluator cards (addendum 9.15): confidence-interval bars for A and B, the model x benchmark matrix of the newest score per pair, cost against score, and
// failure-rate bars, all from the `evaluator.run` events' benchmarks. A figure an older event lacks is null, never 0.
import test from 'node:test';
import assert from 'node:assert/strict';
import { benchesOf, newestWithBenchmarks, ciRows, latestMatrix, cellKey, costPoints, COST_AXES, failureRows } from '../js/evaluator-model.js';
import { shapeRuns } from '../js/runs.js';
import { EVALUATOR } from '../js/run-descriptors.js';

const bench = (bench, score, over = {}) => ({
  bench,
  status: 'succeeded',
  metric: 'accuracy',
  score,
  ci_low: null,
  ci_high: null,
  samples: 50,
  tokens: null,
  elapsed_s: null,
  failure_rate: null,
  ...over,
});
const ev = (id, ts, model, benchmarks, verdict = 'succeeded') => ({
  ts,
  system: 'evaluator',
  kind: 'evaluator.run',
  summary: '',
  data: { run_id: id, suite: 'quick', model, verdict, duration_ms: 1000, benchmarks },
});
const rows = (events) => shapeRuns(events, EVALUATOR);

test('benchesOf: clean numbers, an interval only when both ends are there and in order, everything else null', () => {
  const [r] = rows([
    ev('r1', 100, 'm', [
      bench('gsm8k', 0.9, { ci_low: 0.8, ci_high: 0.95, tokens: 1000, elapsed_s: 12.5, failure_rate: 0.01 }),
      bench('mmlu', 0.7, { ci_low: 0.6 }),
      bench('bbh', 0.5, { ci_low: 0.9, ci_high: 0.1 }),
      bench('arc', null),
      { bench: 'bare', score: 0.4 },
      { bench: '', score: 1 },
      { score: 1 },
      null,
    ]),
  ]);
  const b = benchesOf(r);
  assert.deepEqual(
    b.map((x) => x.bench),
    ['gsm8k', 'mmlu', 'bbh', 'arc', 'bare'],
  );
  assert.deepEqual([b[0].lo, b[0].hi, b[0].tokens, b[0].elapsed_s, b[0].failure_rate], [0.8, 0.95, 1000, 12.5, 0.01]);
  assert.deepEqual([b[1].lo, b[1].hi], [null, null], 'half an interval is none');
  assert.deepEqual([b[2].lo, b[2].hi], [null, null], 'an upside-down interval is none');
  assert.equal(b[3].score, null);
  assert.deepEqual([b[4].lo, b[4].tokens, b[4].failure_rate, b[4].status], [null, null, null, null], 'the keys an older event lacks are null, not 0');
  assert.deepEqual(benchesOf(null), []);
  assert.deepEqual(benchesOf({ event: { data: { benchmarks: 'x' } } }), []);
});

test('ciRows: A first, then what only B has; a benchmark with no score in a run has no bar for it', () => {
  const [a, b] = rows([
    ev('a', 100, 'm', [bench('gsm8k', 0.9, { ci_low: 0.8, ci_high: 0.95 }), bench('mmlu', 0.7)]),
    ev('b', 200, 'm', [bench('mmlu', 0.75), bench('arc', 0.6), bench('gsm8k', null)]),
  ]);
  const r = ciRows(a, b);
  assert.deepEqual(
    r.map((x) => x.bench),
    ['gsm8k', 'mmlu', 'arc'],
  );
  assert.equal(r[0].a.score, 0.9);
  assert.equal(r[0].a.lo, 0.8);
  assert.equal(r[0].b, null, 'B has gsm8k with no score: no bar, not a bar at zero');
  assert.equal(r[1].b.score, 0.75);
  assert.equal(r[2].a, null);
  assert.deepEqual(
    ciRows(a, null).map((x) => x.bench),
    ['gsm8k', 'mmlu'],
  );
  assert.deepEqual(ciRows(null, null), []);
});

test('newestWithBenchmarks: the newest run that has a score; none when nothing does', () => {
  const rs = rows([ev('a', 100, 'm', [bench('x', 0.5)]), ev('b', 200, 'm', [bench('x', null)]), ev('c', 300, 'm', [], 'failed')]);
  assert.equal(newestWithBenchmarks(rs).id, 'a');
  assert.equal(newestWithBenchmarks([]), null);
});

const many = () =>
  rows([
    ev('old', 100, 'qwen', [bench('gsm8k', 0.5), bench('mmlu', 0.4)]),
    ev('new', 300, 'qwen', [bench('gsm8k', 0.8, { failure_rate: 0.02, tokens: 5000, elapsed_s: 30 })]),
    ev('glm', 200, 'glm', [bench('gsm8k', 0.9, { failure_rate: 0.1, tokens: 9000, elapsed_s: 60 }), bench('mmlu', 0.85, { tokens: 3000 }), bench('bbh', 0.6)]),
    ev('bad', 400, 'nemo', [], 'failed'),
  ]);

test('latestMatrix: the newest result of each (model, benchmark) pair; models by best average, benchmarks by how many models have them; a run with no model is skipped', () => {
  const m = latestMatrix(many());
  assert.equal(m.cells.get(cellKey('qwen', 'gsm8k')).score, 0.8, 'the newer run wins');
  assert.equal(m.cells.get(cellKey('qwen', 'gsm8k')).runId, 'new');
  assert.equal(m.cells.get(cellKey('qwen', 'mmlu')).score, 0.4, 'a pair the newer run lacks keeps the older result');
  assert.equal(m.cells.get(cellKey('nemo', 'gsm8k')), undefined);
  assert.deepEqual(m.models, ['glm', 'qwen'], 'the better average first');
  assert.deepEqual(m.benches.slice(0, 2), ['gsm8k', 'mmlu'], 'the benchmarks most models share first');
  assert.equal(m.benches.at(-1), 'bbh');
  assert.deepEqual(m.domain, { min: 0, max: 1 });
  const noModel = rows([{ ts: 1, system: 'evaluator', kind: 'evaluator.run', summary: '', data: { run_id: 'x', verdict: 'succeeded', benchmarks: [bench('a', 0.5)] } }]);
  assert.equal(latestMatrix(noModel).cells.size, 0);
  const odd = latestMatrix(rows([ev('o', 1, 'm', [bench('x', 12), bench('y', 30)])]));
  assert.deepEqual(odd.domain, { min: 12, max: 30 }, 'a score outside 0..1 sets its own scale');
  assert.deepEqual(latestMatrix([]).domain, { min: 0, max: 1 });
});

test('costPoints: a point per result with both a score and the cost; results with no cost are counted; groups are benchmarks, six slots at most', () => {
  const c = costPoints(many(), 'tokens');
  assert.equal(c.points.length, 3);
  assert.equal(c.skipped, 3, 'the scored results with no token figure');
  assert.ok(c.points.every((p) => p.x > 0 && p.y > 0));
  assert.deepEqual(
    c.groups.map((g) => g.bench),
    ['gsm8k', 'mmlu'],
  );
  const t = costPoints(many(), 'elapsed_s');
  assert.equal(t.points.length, 2);
  assert.equal(costPoints(many(), 'nonsense').points.length, 3, 'an unknown axis falls back to tokens');
  assert.deepEqual(
    COST_AXES.map((a) => a.id),
    ['tokens', 'elapsed_s'],
  );
  const lots = costPoints(
    rows([
      ev(
        'l',
        1,
        'm',
        Array.from({ length: 9 }, (_, i) => bench(`b${i}`, 0.5, { tokens: 100 + i })),
      ),
    ]),
    'tokens',
  );
  assert.equal(Math.max(...lots.points.map((p) => p.g)), 5);
});

test('failureRows: highest first among the pairs that recorded one; the rest are counted, never drawn as 0; above 1 is read as percent', () => {
  const f = failureRows(latestMatrix(many()));
  assert.deepEqual(
    f.rows.map((r) => [r.model, r.bench, r.rate]),
    [
      ['glm', 'gsm8k', 0.1],
      ['qwen', 'gsm8k', 0.02],
    ],
  );
  assert.equal(f.missing, 3);
  assert.equal(f.percent, false);
  const pct = failureRows(latestMatrix(rows([ev('p', 1, 'm', [bench('a', 0.5, { failure_rate: 12 }), bench('b', 0.5, { failure_rate: 3 })])])));
  assert.equal(pct.percent, true);
  assert.deepEqual(
    pct.rows.map((r) => r.rate),
    [0.12, 0.03],
  );
  assert.equal(failureRows(latestMatrix(many()), { limit: 1 }).rows.length, 1);
  assert.equal(failureRows(latestMatrix(many()), { limit: 1 }).total, 2);
  assert.deepEqual(failureRows(latestMatrix([])).rows, []);
});
