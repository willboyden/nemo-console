// AIPerf run detail shaping (addendum 9.15): percentile curves from the summary route, the HTTP phases, the per-request histogram, swim lanes and error
// codes from the requests route, and the throughput-vs-latency points over runs. A statistic a run lacks is null, never 0.
import test from 'node:test';
import assert from 'node:assert/strict';
import {
  PCT_KEYS,
  PCT_X,
  CURVE_METRICS,
  msFactor,
  curveOf,
  percentileCurves,
  httpPhases,
  summaryFacts,
  cleanRequests,
  requestHistogram,
  swimLanes,
  errorCodes,
  throughputPoints,
  SCATTER_X,
  REQUEST_MEASURES,
} from '../js/aiperf-model.js';
import { shapeRuns } from '../js/runs.js';
import { AIPERF } from '../js/run-descriptors.js';
import { aiperfSummary, aiperfRequests } from '../dev/mock-wave3.mjs';

const metric = (base, unit = 'ms') => ({ unit, avg: base * 1.1, min: base * 0.5, max: base * 4, std: 3, ...Object.fromEntries(PCT_KEYS.map((k, i) => [k, base * (1 + i / 4)])) });
const summary = (over = {}) => ({
  run_id: 'r1',
  metrics: { time_to_first_token: metric(100), inter_token_latency: metric(10), request_latency: metric(2000) },
  http_phases_ms: { dns: 1, connecting: 2, sending: 1, waiting: 200, receiving: 900 },
  ...over,
});

test('the percentile keys are the nine of the contract, in order, with their numbers', () => {
  assert.deepEqual(PCT_KEYS, ['p1', 'p5', 'p10', 'p25', 'p50', 'p75', 'p90', 'p95', 'p99']);
  assert.deepEqual(PCT_X, [1, 5, 10, 25, 50, 75, 90, 95, 99]);
});

test('curveOf: nine values in ms, null where a percentile is absent; a metric with none is no curve', () => {
  const c = curveOf(summary(), CURVE_METRICS[0]);
  assert.equal(c.values.length, 9);
  assert.equal(c.unit, 'ms');
  assert.equal(c.values[0], 100);
  assert.equal(c.values[8], 300);
  assert.equal(c.n, 9);
  const holes = summary();
  holes.metrics.time_to_first_token.p25 = null;
  delete holes.metrics.time_to_first_token.p75;
  const h = curveOf(holes, CURVE_METRICS[0]);
  assert.equal(h.values[3], null, 'a missing percentile is null, not 0');
  assert.equal(h.values[5], null);
  assert.equal(h.n, 7);
  assert.equal(curveOf({ metrics: { time_to_first_token: { unit: 'ms', avg: 3 } } }, CURVE_METRICS[0]), null, 'only an average is no curve');
  assert.equal(curveOf({ metrics: {} }, CURVE_METRICS[0]), null);
  assert.equal(curveOf(null, CURVE_METRICS[0]), null);
  assert.equal(curveOf({ metrics: [] }, CURVE_METRICS[0]), null);
});

test('curveOf: a unit other than ms is converted; an unknown one is read as ms; alternative metric names are found', () => {
  assert.equal(msFactor('s'), 1000);
  assert.equal(msFactor('us'), 0.001);
  assert.equal(msFactor('ns'), 1e-6);
  assert.equal(msFactor('ms'), 1);
  assert.equal(msFactor(undefined), 1);
  assert.equal(msFactor('furlongs'), 1);
  const sec = { metrics: { time_to_first_token: metric(0.25, 's') } };
  assert.equal(curveOf(sec, CURVE_METRICS[0]).values[0], 250);
  assert.equal(curveOf({ metrics: { ttft: metric(5) } }, CURVE_METRICS[0]).values[0], 5);
});

test('percentileCurves: the metrics at least one run has; A and B side by side; a run without a metric has null for it', () => {
  const a = summary();
  const b = summary();
  delete b.metrics.inter_token_latency;
  const cs = percentileCurves(a, b);
  assert.deepEqual(
    cs.map((c) => c.id),
    ['ttft', 'itl', 'latency'],
  );
  assert.ok(cs[0].a && cs[0].b);
  assert.ok(cs[1].a && cs[1].b === null);
  assert.deepEqual(
    percentileCurves(a, null).map((c) => c.id),
    ['ttft', 'itl', 'latency'],
  );
  assert.ok(percentileCurves(a, null).every((c) => c.b === null));
  assert.deepEqual(percentileCurves({ metrics: {} }, null), []);
  assert.deepEqual(percentileCurves(null, null), []);
});

test('httpPhases: the five phases in order, null for a missing one, null when the run has none; a negative figure is no figure', () => {
  assert.deepEqual(httpPhases(summary()), { dns: 1, connecting: 2, sending: 1, waiting: 200, receiving: 900 });
  assert.deepEqual(httpPhases({ http_phases_ms: { waiting: 5 } }), { dns: null, connecting: null, sending: null, waiting: 5, receiving: null });
  assert.equal(httpPhases({ http_phases_ms: null }), null);
  assert.equal(httpPhases({}), null);
  assert.equal(httpPhases({ http_phases_ms: { dns: -1, waiting: 'x' } }), null);
  assert.equal(httpPhases({ http_phases_ms: [1, 2] }), null);
});

test('summaryFacts keeps numbers and words, and nothing else', () => {
  const f = summaryFacts({ requests: 200, concurrency: 8, isl: 256, osl: 128, duration_s: null, preset: 'x', model: '', verdict: 7 });
  assert.deepEqual(f, { requests: 200, concurrency: 8, isl: 256, osl: 128, duration_s: null, preset: 'x', model: null, verdict: null });
});

const row = (i, over = {}) => ({
  i,
  start_ms: i * 10,
  end_ms: i * 10 + 100 + i,
  latency_ms: 100 + i,
  ttft_ms: 20 + i,
  itl_avg_ms: 5,
  input_tokens: 10,
  output_tokens: 20,
  worker: i % 2,
  error_code: null,
  ...over,
});

test('cleanRequests: documented fields only; error_code is null (success), 0 (failed without a code) or an integer; a row with no times is dropped', () => {
  const a = cleanRequests({
    total: 5,
    returned: 4,
    truncated: true,
    rows: [row(0), row(1, { error_code: 429 }), row(2, { error_code: 0 }), { i: 3, start_ms: null, end_ms: 5 }, row(4, { worker: -1, error_code: 'x', latency_ms: 'slow', extra: 'drop me' })],
  });
  assert.equal(a.rows.length, 4);
  assert.equal(a.total, 5);
  assert.equal(a.returned, 4);
  assert.equal(a.truncated, true);
  assert.equal(a.rows[0].error_code, null);
  assert.equal(a.rows[1].error_code, 429);
  assert.equal(a.rows[2].error_code, 0, '0 stays 0: a failure with no usable HTTP code');
  assert.equal(a.rows[3].error_code, null);
  assert.equal(a.rows[3].worker, 0, 'a bad worker index becomes 0');
  assert.equal(a.rows[3].latency_ms, null, 'a text latency is no latency');
  assert.ok(!('extra' in a.rows[3]));
  assert.deepEqual(Object.keys(a.rows[0]).sort(), ['end_ms', 'error_code', 'i', 'input_tokens', 'itl_avg_ms', 'latency_ms', 'output_tokens', 'start_ms', 'ttft_ms', 'worker']);
  const none = cleanRequests(null);
  assert.deepEqual(none, { rows: [], total: 0, returned: 0, truncated: false });
});

test('cleanRequests: an end before its start is the start (the bar has no negative length)', () => {
  const a = cleanRequests({ rows: [row(0, { start_ms: 50, end_ms: 20 })] });
  assert.equal(a.rows[0].end_ms, 50);
});

test('requestHistogram: only the requests that succeeded; the median and p95; failed and missing counted; no data is null', () => {
  const rows = [...Array.from({ length: 20 }, (_, i) => row(i)), row(20, { error_code: 503, latency_ms: 5 }), row(21, { latency_ms: null })];
  const h = requestHistogram(rows, 'latency_ms', { bins: 5 });
  assert.equal(h.n, 20);
  assert.equal(h.failed, 1);
  assert.equal(h.missing, 1, 'a successful request without a latency is counted, not binned as 0');
  assert.equal(
    h.counts.reduce((a, b) => a + b, 0),
    20,
  );
  assert.ok(Math.abs(h.p50 - 109.5) < 1e-9);
  assert.ok(h.p95 > h.p50);
  assert.equal(requestHistogram([row(0, { error_code: 500 })], 'latency_ms'), null);
  assert.equal(requestHistogram([], 'latency_ms'), null);
  assert.equal(requestHistogram([row(0, { ttft_ms: null })], 'ttft_ms'), null);
  assert.deepEqual(
    REQUEST_MEASURES.map((m) => m.field),
    ['latency_ms', 'ttft_ms', 'itl_avg_ms'],
  );
});

test('swimLanes: a lane per worker in worker order, every failed request kept, the rest sampled to maxBars, the busiest lanes when there are too many', () => {
  const rows = Array.from({ length: 100 }, (_, i) => row(i, { worker: i % 4, error_code: i % 25 === 0 ? 0 : null }));
  const all = swimLanes(rows, { maxBars: 1000 });
  assert.deepEqual(
    all.lanes.map((l) => l.worker),
    [0, 1, 2, 3],
  );
  assert.equal(all.items.length, 100);
  assert.equal(all.sampled, false);
  assert.equal(all.workers, 4);
  assert.ok(all.lanes.every((l) => l.n === 25));
  assert.equal(all.lanes[0].errors, 1);
  assert.equal(
    all.lanes.reduce((a, l) => a + l.errors, 0),
    4,
  );
  const capped = swimLanes(rows, { maxBars: 30 });
  assert.ok(capped.items.length <= 30);
  assert.equal(capped.items.filter((x) => x.error).length, 4, 'every failed request survives the sampling');
  assert.equal(capped.sampled, true);
  assert.ok(
    capped.items.every((x, i, a) => i === 0 || x.start >= a[i - 1].start),
    'in time order',
  );
  const many = swimLanes(
    Array.from({ length: 60 }, (_, i) => row(i, { worker: i % 30 })),
    { maxLanes: 5 },
  );
  assert.equal(many.lanes.length, 5);
  assert.equal(many.workers, 30);
  assert.ok(many.items.every((x) => x.lane >= 0 && x.lane < 5));
  assert.equal(all.items.find((x) => x.error).code, 0);
});

test('errorCodes: counts by code, most frequent first; 0 is a code of its own; the rate is null for no rows', () => {
  const rows = [row(0), row(1, { error_code: 429 }), row(2, { error_code: 429 }), row(3, { error_code: 0 }), row(4, { error_code: 503 }), row(5)];
  const e = errorCodes(rows);
  assert.equal(e.errors, 4);
  assert.equal(e.total, 6);
  assert.ok(Math.abs(e.rate - 4 / 6) < 1e-9);
  assert.deepEqual(
    e.codes.map((c) => [c.code, c.count]),
    [
      [429, 2],
      [0, 1],
      [503, 1],
    ],
  );
  assert.ok(Math.abs(e.codes[0].share - 0.5) < 1e-9);
  const clean = errorCodes([row(0), row(1)]);
  assert.equal(clean.errors, 0);
  assert.deepEqual(clean.codes, []);
  assert.equal(errorCodes([]).rate, null);
});

test('throughputPoints: one point per run with both numbers, grouped by concurrency (lowest first), the slots run out at six; a run lacking one is counted, not drawn', () => {
  const ev = (id, f) => ({ ts: 1000 + Number(id.slice(1)), system: 'aiperf', kind: 'aiperf.run', summary: '', data: { run_id: id, verdict: 'succeeded', preset: 'p', model: 'm', ...f } });
  const rows = shapeRuns(
    [
      ev('r1', { concurrency: 8, ttft_p50_ms: 100, output_tps: 1000 }),
      ev('r2', { concurrency: 4, ttft_p50_ms: 200, output_tps: 500 }),
      ev('r3', { concurrency: 8, ttft_p50_ms: null, output_tps: 900 }),
      ev('r4', { concurrency: 8, ttft_p50_ms: 120 }),
      ev('r5', { ttft_p50_ms: 90, output_tps: 1100 }),
    ],
    AIPERF,
  );
  const s = throughputPoints(rows, 'ttft_p50_ms');
  assert.equal(s.points.length, 3);
  assert.equal(s.skipped, 2);
  assert.deepEqual(
    s.groups.map((g) => g.value),
    [4, 8, null],
    'lowest concurrency first, an unknown one last',
  );
  assert.deepEqual(
    s.groups.map((g) => g.g),
    [0, 1, 2],
  );
  const p = (id) => s.points.find((x) => x.id === id);
  assert.equal(p('r2').g, 0);
  assert.equal(p('r1').g, 1);
  assert.equal(p('r5').concurrency, null);
  assert.deepEqual(
    SCATTER_X.map((x) => x.id),
    ['ttft_p50_ms', 'ttft_p99_ms', 'itl_p50_ms'],
  );
  const lots = shapeRuns(
    Array.from({ length: 9 }, (_, i) => ev(`r${i + 10}`, { concurrency: i + 1, ttft_p50_ms: 10 + i, output_tps: 100 + i })),
    AIPERF,
  );
  const l = throughputPoints(lots, 'ttft_p50_ms');
  assert.equal(Math.max(...l.points.map((x) => x.g)), 5, 'six slots at most');
});

// The mock's answers go through the same shaping the page uses: they must come out right.
const mockEv = (over = {}) => ({
  data: {
    run_id: 'run-1',
    preset: 'chat-short',
    model: 'm',
    verdict: 'succeeded',
    concurrency: 8,
    requests: 200,
    isl: 256,
    osl: 128,
    duration_ms: 80000,
    ttft_p50_ms: 200,
    ttft_p99_ms: 600,
    itl_p50_ms: 20,
    output_tps: 1500,
    request_tps: 3,
    error_rate_pct: 0,
    ...over,
  },
});

test('the mock summary centres its curves on the event: the median is the event median, p99 is the event p99, the curve rises', () => {
  const s = aiperfSummary(mockEv());
  const c = curveOf(s, CURVE_METRICS[0]);
  assert.ok(Math.abs(c.values[4] - 200) < 0.01);
  assert.ok(Math.abs(c.values[8] - 600) < 1);
  assert.ok(c.values.every((v, i) => i === 0 || v > c.values[i - 1]));
  assert.equal(percentileCurves(s, null).length, 3);
});

test('the mock requests, shaped: workers by first appearance, failures carry codes (0 among them), the histogram counts what succeeded', () => {
  const ans = aiperfRequests(mockEv({ error_rate_pct: 8, requests: 300 }));
  const req = cleanRequests(ans);
  assert.equal(req.rows.length, 300);
  assert.equal(req.truncated, false);
  const workers = [...new Set(req.rows.map((r) => r.worker))];
  assert.deepEqual(workers.slice(0, 3), [0, 1, 2], 'indices are assigned by first appearance');
  const e = errorCodes(req.rows);
  assert.ok(e.errors > 5 && e.errors < 60, `an error rate near 8%: ${e.errors}`);
  const h = requestHistogram(req.rows, 'latency_ms');
  assert.equal(h.n + h.failed + h.missing, 300);
  assert.equal(swimLanes(req.rows).lanes.length, 8);
});
