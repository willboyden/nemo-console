// The Switchyard flow-tab shaping (addendum 9.14): decision-flow edges from counters, per-model rows, the call-rate matrix, fallbacks, outcomes.
// A figure the counters cannot give is null, never 0.
import test from 'node:test';
import assert from 'node:assert/strict';
import {
  seriesOfKey,
  nodeId,
  nodeName,
  nodeLayer,
  routeCallsFromSnapshot,
  decisionFlow,
  filterOptions,
  filterCounts,
  modelRows,
  rateMatrix,
  fallbackModel,
  overheadModel,
  outcomeRows,
} from '../js/switchyard-model.js';

const ser = (key, label, points) => ({ key, label, points: points.map(([t, v]) => [t, v, v]) });
const ramp = (n, f, t0 = 0, step = 60) => Array.from({ length: n }, (_, i) => [t0 + i * step, f(i)]);
const dec = (source, target, perMin, extra = {}) =>
  ser(
    'sy.decisions_total',
    { source, target, ...extra },
    ramp(11, (i) => i * perMin),
  );

test('node ids carry their column: a route and a target of the same name stay two nodes', () => {
  assert.notEqual(nodeId('route', 'fast'), nodeId('target', 'fast'));
  assert.equal(nodeName(nodeId('target', 'qwen3.8-27b-1m')), 'qwen3.8-27b-1m');
  assert.equal(nodeName(nodeId('source', 'a:b')), 'a:b');
  assert.equal(nodeLayer(nodeId('route', 'x')), 'route');
  assert.equal(nodeLayer(nodeId('source', 'x')), 'source');
  assert.equal(nodeLayer(nodeId('target', 'x')), 'target');
});

test('seriesOfKey matches the key as requested or its base', () => {
  const s = [ser('sy.model.calls', { model: 'a' }, []), ser('dyn.tokens{kind=completion}', null, []), ser('sy.model.calls', { model: 'b' }, [])];
  assert.equal(seriesOfKey(s, 'sy.model.calls').length, 2);
  assert.equal(seriesOfKey(s, 'dyn.tokens').length, 1);
});

test('decisionFlow without a route label: source -> target edges from the increase over the window, routes kept apart', () => {
  const f = decisionFlow({
    decisions: [dec('heuristic', 'light', 10), dec('judge', 'light', 5), dec('judge', 'big', 5), dec('cache', 'light', 0)],
    routeCalls: [{ route: 'auto', value: 70, window: 'last_500_records' }],
  });
  assert.equal(f.mode, 'two');
  assert.equal(f.total, 100 + 50 + 50, 'a source that did nothing adds no edge');
  const edge = (a, b) => f.flows.find((x) => x.source === nodeId('source', a) && x.target === nodeId('target', b))?.value;
  assert.equal(edge('heuristic', 'light'), 100);
  assert.equal(edge('judge', 'light'), 50);
  assert.equal(edge('judge', 'big'), 50);
  assert.equal(edge('cache', 'light'), undefined);
  assert.equal(f.routeApplied, false);
  assert.deepEqual(f.routeShares, [{ route: 'auto', value: 70, window: 'last_500_records' }]);
  assert.ok(
    f.flows.every((x) => nodeLayer(x.source) !== 'route'),
    'no route column: the counters do not say which route made which decision',
  );
});

test('decisionFlow with a route label on every series: route -> source -> target, and the route filter narrows it', () => {
  const decisions = [dec('heuristic', 'light', 10, { route: 'auto' }), dec('judge', 'big', 4, { route: 'auto' }), dec('judge', 'big', 6, { route: 'deep' })];
  const f = decisionFlow({ decisions, routeCalls: [] });
  assert.equal(f.mode, 'three');
  const r = (a) => f.flows.find((x) => x.source === nodeId('route', a) && x.target === nodeId('source', a === 'auto' ? 'heuristic' : 'judge'));
  assert.equal(r('auto').value, 100);
  assert.equal(f.flows.find((x) => x.source === nodeId('source', 'judge') && x.target === nodeId('target', 'big')).value, 40 + 60, 'two routes feeding one source add up');
  assert.equal(f.routeApplied, true);
  const only = decisionFlow({ decisions, routeCalls: [] }, { route: 'deep' });
  assert.equal(only.total, 60);
  assert.ok(only.flows.every((x) => !x.source.includes('auto')));
  const model = decisionFlow({ decisions, routeCalls: [] }, { model: 'light' });
  assert.equal(model.total, 100);
  assert.equal(decisionFlow({ decisions, routeCalls: [] }, { route: 'auto', model: 'big' }).total, 40);
});

test('decisionFlow: a mix (some series with a route, some without) is two columns, not a guess', () => {
  const f = decisionFlow({ decisions: [dec('a', 'x', 1, { route: 'r' }), dec('b', 'y', 1)], routeCalls: [] });
  assert.equal(f.mode, 'two');
});

test('decisionFlow: a counter reset is counted and reported, one sample or no series is "none" (not an empty flow of 0)', () => {
  const reset = ser('sy.decisions_total', { source: 's', target: 't' }, [
    [0, 50],
    [60, 80],
    [120, 10],
    [180, 30],
  ]);
  const f = decisionFlow({ decisions: [reset], routeCalls: [] });
  assert.equal(f.total, 30 + 10 + 20);
  assert.equal(f.resets, 1);
  assert.equal(decisionFlow({ decisions: [ser('sy.decisions_total', { source: 's', target: 't' }, [[0, 5]])], routeCalls: [] }).mode, 'none');
  assert.equal(decisionFlow({ decisions: [], routeCalls: [] }).mode, 'none');
  assert.equal(
    decisionFlow({
      decisions: [
        ser('sy.decisions_total', { source: 's', target: 't' }, [
          [0, 5],
          [60, 5],
        ]),
      ],
      routeCalls: [],
    }).mode,
    'none',
    'a counter that did not move made no decision',
  );
  assert.equal(
    decisionFlow({
      decisions: [
        ser('sy.decisions_total', { source: 's' }, [
          [0, 1],
          [60, 9],
        ]),
      ],
      routeCalls: [],
    }).mode,
    'none',
    'a series without a target is not an edge',
  );
  assert.equal(decisionFlow({}).mode, 'none');
});

test('routeCallsFromSnapshot: per route, busiest first, the window label kept; labels missing or values non-finite are skipped', () => {
  const r = routeCallsFromSnapshot([
    { key: 'sy.route.calls', value: 21, labels: { route: 'b', window: 'last_500_records' } },
    { key: 'sy.route.calls', value: 412, labels: { route: 'a', window: 'last_500_records' } },
    { key: 'sy.route.calls', value: 'x', labels: { route: 'c' } },
    { key: 'sy.route.calls', value: 3, labels: {} },
    { key: 'sy.requests_total', value: 9, labels: { route: 'z' } },
  ]);
  assert.deepEqual(
    r.map((x) => x.route),
    ['a', 'b'],
  );
  assert.equal(r[0].window, 'last_500_records');
  assert.deepEqual(routeCallsFromSnapshot(undefined), []);
});

test('filterOptions and filterCounts', () => {
  const o = filterOptions({
    routeCalls: [{ route: 'b' }],
    decisions: [ser('x', { route: 'a' }, [])],
    modelSeries: [ser('m', { model: 'z' }, []), ser('m', { model: 'y' }, [])],
    summary: { by_route: { c: 1 }, by_model: { y: 2, q: 3 } },
  });
  assert.deepEqual(o, { routes: ['a', 'b', 'c'], models: ['q', 'y', 'z'] });
  assert.deepEqual(filterCounts({ a: 1, b: 2 }, ''), { a: 1, b: 2 });
  assert.deepEqual(filterCounts({ a: 1, b: 2 }, 'b'), { b: 2 });
  assert.deepEqual(filterCounts({ a: 1 }, 'zz'), {}, 'a name the server did not send leaves nothing');
  assert.equal(filterCounts(undefined, 'a'), undefined);
});

const lat = (m, q, v) =>
  ser(
    'sy.model.latency_ms',
    { model: m, q },
    ramp(5, () => v),
  );
test('modelRows: window means, the error rate, newest cache ratio, largest context; ordered by calls', () => {
  const rows = modelRows({
    latency: [lat('a', 'p50', 400), lat('a', 'p99', 3000), lat('b', 'p50', 900)],
    errors: [
      ser(
        'sy.model.errors_total',
        { model: 'a' },
        ramp(11, (i) => i * 3),
      ),
    ],
    calls: [
      ser(
        'sy.model.calls',
        { model: 'a' },
        ramp(11, (i) => i * 100),
      ),
      ser(
        'sy.model.calls',
        { model: 'b' },
        ramp(11, (i) => i * 400),
      ),
    ],
    cache: [
      ser('sy.model.cache_hit_ratio', { model: 'a' }, [
        [0, 0.5],
        [60, 0.7],
      ]),
    ],
    context: [
      ser('sy.model.max_context_tokens', { model: 'a' }, [
        [0, 1000],
        [60, 262144],
        [120, 5000],
      ]),
    ],
  });
  assert.deepEqual(
    rows.map((r) => r.model),
    ['b', 'a'],
  );
  const a = rows.find((r) => r.model === 'a'),
    b = rows.find((r) => r.model === 'b');
  assert.equal(a.p50, 400);
  assert.equal(a.p99, 3000);
  assert.equal(a.calls, 1000);
  assert.equal(a.errors, 30);
  assert.ok(Math.abs(a.errorRate - 0.03) < 1e-9);
  assert.equal(a.cache, 0.7);
  assert.equal(a.maxContext, 262144);
  assert.equal(b.p99, null, 'no p99 series: unknown, not 0');
  assert.equal(b.errorRate, null, 'no error counter: unknown, not 0%');
  assert.equal(b.errors, null);
  assert.equal(b.cache, null);
  assert.equal(b.maxContext, null);
});

test('modelRows: no calls in the window or a one-sample counter gives no error rate; the model filter keeps one row', () => {
  const idle = modelRows({
    latency: [],
    errors: [
      ser(
        'e',
        { model: 'a' },
        ramp(5, () => 4),
      ),
    ],
    calls: [
      ser(
        'c',
        { model: 'a' },
        ramp(5, () => 100),
      ),
    ],
    cache: [],
    context: [],
  });
  assert.equal(idle[0].errorRate, null, 'zero calls in the window: no rate');
  assert.equal(idle[0].calls, 0);
  const one = modelRows({ errors: [ser('e', { model: 'a' }, [[0, 4]])], calls: [ser('c', { model: 'a' }, [[0, 100]])] });
  assert.equal(one[0].calls, null);
  assert.equal(one[0].errorRate, null);
  const both = { latency: [lat('a', 'p50', 1), lat('b', 'p50', 2)] };
  assert.deepEqual(
    modelRows(both, { model: 'b' }).map((r) => r.model),
    ['b'],
  );
  assert.deepEqual(modelRows({}), []);
  const capped = modelRows({
    errors: [
      ser(
        'e',
        { model: 'a' },
        ramp(5, (i) => i * 50),
      ),
    ],
    calls: [
      ser(
        'c',
        { model: 'a' },
        ramp(5, (i) => i),
      ),
    ],
  });
  assert.equal(capped[0].errorRate, 1, 'an error rate cannot exceed 100%');
});

test('rateMatrix: rate per model per slot; a reset leaves a gap, not a spike; an idle slot is a real 0; rows by total', () => {
  const calls = [
    ser(
      'sy.model.calls',
      { model: 'busy' },
      ramp(6, (i) => i * 60 * 2),
    ),
    ser('sy.model.calls', { model: 'quiet' }, [
      [0, 5],
      [60, 5],
      [120, 5],
      [180, 5],
      [240, 8],
      [300, 8],
    ]),
    ser('sy.model.calls', { model: 'restart' }, [
      [0, 100],
      [60, 160],
      [120, 3],
      [180, 63],
      [240, 123],
      [300, 183],
    ]),
  ];
  const m = rateMatrix(calls, { from: 0, to: 300, stepS: 60 }, { maxCols: 100 });
  assert.equal(m.unit, 's');
  const by = Object.fromEntries(m.rows.map((r) => [r.model, r]));
  assert.deepEqual(by.busy.values, [null, 2, 2, 2, 2, 2]);
  assert.deepEqual(by.quiet.values.slice(1, 4), [0, 0, 0], 'an idle slot is a real zero');
  assert.ok(Math.abs(by.quiet.values[4] - 3 / 60) < 1e-9);
  assert.equal(by.restart.values[2], null, 'the reset leaves a gap at that slot');
  assert.equal(by.restart.values[1], 1);
  assert.ok(
    by.restart.values.every((v) => v == null || (v >= 0 && v < 5)),
    'no spike across the reset',
  );
  assert.equal(m.rows[0].model, 'busy');
  assert.equal(m.ts.length, 6);
  assert.equal(m.bucketS, 60);
});

test('rateMatrix: a quiet fleet is shown per minute, columns merge, the model filter narrows, nothing is an empty matrix', () => {
  const slow = ser(
    'sy.model.calls',
    { model: 'a' },
    ramp(7, (i) => i * 3),
  ); // 0.05/s
  const m = rateMatrix([slow], { from: 0, to: 360, stepS: 60 }, {});
  assert.equal(m.unit, 'min');
  assert.ok(Math.abs(m.rows[0].values[1] - 3) < 1e-9, '3 calls per minute');
  const long = ser(
    'sy.model.calls',
    { model: 'a' },
    ramp(1008, (i) => i * 600 * 2, 0, 600),
  );
  const merged = rateMatrix([long], { from: 0, to: 1007 * 600, stepS: 600 }, { maxCols: 120 });
  assert.ok(merged.ts.length <= 120);
  assert.equal(merged.bucketS, 600 * 9);
  assert.deepEqual(
    rateMatrix([slow, { ...slow, label: { model: 'b' } }], { from: 0, to: 360, stepS: 60 }, { model: 'b' }).rows.map((r) => r.model),
    ['b'],
  );
  assert.deepEqual(rateMatrix([], { from: 0, to: 60, stepS: 60 }).rows, []);
  assert.deepEqual(
    rateMatrix(
      [
        ser(
          'x',
          null,
          ramp(5, (i) => i),
        ),
      ],
      { from: 0, to: 240, stepS: 60 },
    ).rows,
    [],
    'a series without a model is not a row',
  );
});

test('fallbackModel, overheadModel, outcomeRows', () => {
  const fb = fallbackModel([
    ser(
      'sy.fallbacks_total',
      { reason: 'unavailable' },
      ramp(5, (i) => i),
    ),
    ser(
      'sy.fallbacks_total',
      { reason: 'context_window' },
      ramp(5, (i) => i * 5),
    ),
    ser('sy.fallbacks_total', { reason: 'lone' }, [[0, 1]]),
  ]);
  assert.deepEqual(
    fb.rows.map((r) => [r.reason, r.value]),
    [
      ['context_window', 20],
      ['unavailable', 4],
    ],
  );
  assert.equal(fb.total, 24);
  assert.equal(fallbackModel([]).total, null, 'no fallback counter: unknown, not 0');
  assert.equal(
    fallbackModel([
      ser(
        'x',
        { reason: 'r' },
        ramp(3, () => 7),
      ),
    ]).total,
    0,
    'a counter that did not move: a real 0',
  );
  const oh = overheadModel([
    ser('sy.routing_overhead_ms', { q: 'p50' }, [
      [0, 1],
      [60, 3],
    ]),
    ser('sy.routing_overhead_ms', { q: 'p99' }, [[0, 4]]),
    ser('sy.routing_overhead_ms', null, [[0, 9]]),
  ]);
  assert.deepEqual([oh.p50.latest, oh.p50.mean, oh.p99.latest], [3, 2, 4]);
  assert.deepEqual(Object.keys(oh).sort(), ['p50', 'p99']);
  const out = outcomeRows([
    ser(
      'sy.client_responses_total',
      { outcome: 'ok' },
      ramp(5, (i) => i * 9),
    ),
    ser(
      'sy.client_responses_total',
      { outcome: 'error' },
      ramp(5, (i) => i),
    ),
    ser('sy.client_responses_total', { outcome: 'x' }, [[0, 1]]),
  ]);
  assert.deepEqual(
    out.map((r) => r.outcome),
    ['ok', 'error'],
  );
  assert.ok(Math.abs(out[0].pct - 90) < 1e-9);
  assert.deepEqual(outcomeRows([]), []);
});
