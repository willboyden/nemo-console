// The Overview models (addendum 9.16): which metric each tile draws, the sparkline from one series answer, the up / down strip and the problem counts.
import test from 'node:test';
import assert from 'node:assert/strict';
import { SPARKS, SPARK_WINDOW, sparkRequest, sparkModel, tileStrip, fmtSparkValue, problemCounts, SEVERITIES } from '../js/overview-model.js';
import { SYSTEM_IDS } from '../js/systems.js';

const T0 = 50_000;
const ans = (series, n = 60) => ({ window: '1h', step_s: 60, from: T0, to: T0 + (n - 1) * 60, series, truncated: false });
const pts = (n, f) => Array.from({ length: n }, (_, i) => [T0 + i * 60, f(i), f(i)]).filter((p) => p[1] != null);

test('every managed system has exactly one sparkline metric, a unit and a way to combine its variants', () => {
  for (const id of SYSTEM_IDS) {
    const s = SPARKS[id];
    assert.ok(s, id);
    assert.match(s.key, /^[a-z][a-z0-9_.]*$/);
    assert.ok(['rate', 'value'].includes(s.how) && ['sum', 'mean'].includes(s.combine), id);
    assert.ok(['rate', 'int', 'ratio', 'num'].includes(s.unit), id);
  }
  // the systems whose adapters report a `*.up`-style key (guardrails.rs gr.up, openshell.rs os.gateway_up, relay.rs relay.gateway_up, dynamo.rs dyn.up)
  assert.deepEqual(
    Object.entries(SPARKS)
      .filter(([, s]) => s.up)
      .map(([id, s]) => [id, s.up])
      .sort(),
    [
      ['dynamo', 'dyn.up'],
      ['guardrails', 'gr.up'],
      ['openshell', 'os.gateway_up'],
      ['relay', 'relay.gateway_up'],
    ],
  );
});

test('one batched request per system: the spark key and its up key, one hour, split only where variants are averaged', () => {
  assert.deepEqual(sparkRequest('guardrails'), { keys: ['gr.configs', 'gr.up'], window: '1h', split: false });
  assert.deepEqual(sparkRequest('switchyard'), { keys: ['sy.requests_total'], window: SPARK_WINDOW, split: false });
  assert.deepEqual(sparkRequest('dcgm'), { keys: ['dcgm.gpu_util'], window: '1h', split: true });
  assert.equal(sparkRequest('nope'), null);
  for (const id of SYSTEM_IDS) assert.ok(sparkRequest(id).keys.length <= 2, 'at most the metric and its up key');
});

test('a counter tile is a rate; a reset leaves a gap, never a negative or a spike', () => {
  const c = pts(60, (i) => (i < 30 ? i * 120 : (i - 30) * 120)); // 2 per second, restarted at 30
  const m = sparkModel(ans([{ key: 'sy.requests_total', label: null, points: c }]), 'switchyard');
  assert.equal(m.per, 's');
  assert.equal(m.any, true);
  assert.ok(Math.abs(m.latest - 2) < 1e-9);
  assert.ok(m.values.every((v) => v == null || v >= 0));
  assert.equal(m.values[0], null, 'the first bucket has no rate');
  assert.equal(m.values[30], null, 'the restart bucket has no rate');
  assert.equal(m.values.filter((v) => v == null).length, 2);
});

test('a slow counter is shown per minute', () => {
  const m = sparkModel(ans([{ key: 'dyn.requests_total', label: null, points: pts(60, (i) => i * 3) }]), 'dynamo');
  assert.equal(m.per, 'min');
  assert.ok(Math.abs(m.latest - 3) < 1e-9);
  assert.equal(fmtSparkValue(m.latest, 'rate', m.per), '3.00/min');
});

test('a gauge tile keeps its points; a missing sample is null in the line (the pen lifts), never 0', () => {
  const g = pts(60, (i) => (i >= 20 && i < 25 ? null : 5));
  const m = sparkModel(ans([{ key: 'os.sandboxes', label: null, points: g }]), 'openshell');
  assert.equal(m.values.length, 60);
  assert.deepEqual(m.values.slice(18, 27), [5, 5, null, null, null, null, null, 5, 5]);
  assert.deepEqual([m.min, m.max, m.latest], [5, 5, 5]);
  assert.equal(m.per, null);
});

test('labelled keys are averaged (utilisation, scores) or summed, per time slot', () => {
  const lines = [
    { key: 'dcgm.gpu_util', label: { gpu: '0' }, points: pts(60, () => 0.8) },
    { key: 'dcgm.gpu_util', label: { gpu: '1' }, points: pts(60, (i) => (i < 30 ? 0.2 : null)) },
  ];
  const m = sparkModel(ans(lines), 'dcgm');
  assert.ok(Math.abs(m.values[0] - 0.5) < 1e-9, 'mean of the two where both have a sample');
  assert.ok(Math.abs(m.values[40] - 0.8) < 1e-9, 'the one that has a sample where the other has none');
  const sum = sparkModel(
    ans([
      { key: 'flare.generated_total', label: { target: 'a' }, points: pts(60, (i) => i * 60) },
      { key: 'flare.generated_total', label: { target: 'b' }, points: pts(60, (i) => i * 120) },
    ]),
    'flare',
  );
  assert.ok(Math.abs(sum.latest - 3) < 1e-9, 'rates of the targets add up');
});

test('no samples: an empty model (no flat line at 0), a system without a spec and garbage too', () => {
  for (const a of [ans([{ key: 'os.sandboxes', label: null, points: [] }]), ans([]), null, {}]) {
    const m = sparkModel(a, 'openshell');
    assert.equal(m.any, false);
    assert.equal(m.latest, null);
    assert.deepEqual(m.values, []);
  }
  assert.equal(sparkModel(ans([]), 'nope').spec, null);
});

test('the up / down strip of a tile: only for systems with an up key, read from the same answer', () => {
  const a = ans([
    { key: 'gr.configs', label: null, points: pts(60, () => 3) },
    { key: 'gr.up', label: null, points: pts(60, (i) => (i >= 10 && i < 14 ? 0 : 1)) },
  ]);
  const s = tileStrip(a, 'guardrails');
  assert.equal(s.cells.length, 60);
  assert.equal(s.downRuns.length, 1);
  assert.equal(s.downRuns[0].cells, 4);
  assert.equal(tileStrip(a, 'switchyard'), null, 'no up key for this system');
  assert.equal(tileStrip(ans([]), 'guardrails'), null, 'no series for the up key');
});

test('the newest figure reads by unit; an en dash for none', () => {
  assert.equal(fmtSparkValue(null, 'int'), '–');
  assert.equal(fmtSparkValue(0.653, 'ratio'), '65%');
  assert.equal(fmtSparkValue(0.031, 'ratio'), '3.1%');
  assert.equal(fmtSparkValue(12.345, 'rate', 's'), '12.3/s');
  assert.equal(fmtSparkValue(3.2, 'int'), '3');
  assert.equal(fmtSparkValue(1840.5, 'num'), '1,841');
  assert.equal(fmtSparkValue(0.82, 'num'), '0.82');
});

test('problem counts by severity: totals, a row per system with a problem (worst first), clean systems counted', () => {
  const sys = [
    { system: 'a', problems: [{ severity: 'warn' }, { severity: 'info' }] },
    { system: 'b', problems: [{ severity: 'error' }, { severity: 'warn' }, { severity: 'warn' }] },
    { system: 'c', problems: [] },
    { system: 'd', problems: [{ severity: 'weird' }] },
    { system: 'e' },
  ];
  const c = problemCounts(sys);
  assert.deepEqual(c.totals, { error: 1, warn: 3, info: 2, all: 6 });
  assert.deepEqual(
    c.rows.map((r) => r.system),
    ['b', 'a', 'd'],
  );
  assert.equal(c.rows[2].info, 1, 'an unknown severity counts as info');
  assert.equal(c.systems, 3);
  assert.equal(c.clean, 2);
  assert.deepEqual(SEVERITIES, ['error', 'warn', 'info']);
  assert.deepEqual(problemCounts(null).totals, { error: 0, warn: 0, info: 0, all: 0 });
});
