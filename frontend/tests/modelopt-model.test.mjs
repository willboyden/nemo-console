// Model Optimizer cards (addendum 9.15): the job timeline, base vs output size with the ratio, and the age of a running job against what a finished
// job of the same method takes. A job with no size or duration has none (null), never 0.
import test from 'node:test';
import assert from 'node:assert/strict';
import { jobsOf, methodsOf, timelineRows, ratioParts, sizeRows, runningJobs, STUCK_FACTOR, STUCK_MIN_MS } from '../js/modelopt-model.js';
import { shapeRuns } from '../js/runs.js';
import { MODELOPT } from '../js/run-descriptors.js';

const NOW = 1_000_000 * 1000; // ms
const MIN = 60 * 1000;
const ev = (id, tsMinAgo, over = {}) => ({
  ts: NOW / 1000 - tsMinAgo * 60,
  system: 'modelopt',
  kind: 'modelopt.job',
  summary: '',
  data: { job_id: id, recipe: `r-${id}`, method: 'reap', base_model: 'org/base', output: '/runs/x', verdict: 'succeeded', duration_ms: 40 * MIN, ...over },
});
const jobs = (events) => jobsOf(shapeRuns(events, MODELOPT), NOW);

test("ratio: the ratio is the event's own, else output / base, else null; percent and saving follow", () => {
  const j = jobs([
    ev('a', 300, { base_bytes: 100e9, output_bytes: 31e9, ratio: 0.31 }),
    ev('b', 200, { base_bytes: 200e9, output_bytes: 100e9 }),
    ev('c', 100, { base_bytes: null, output_bytes: 100e9 }),
    ev('d', 50, { verdict: 'failed', duration_ms: 2 * MIN }),
  ]);
  assert.equal(j[0].ratio, 0.31);
  assert.equal(j[1].ratio, 0.5);
  assert.equal(j[2].ratio, null);
  assert.equal(j[3].ratio, null);
  assert.equal(ratioParts(0.31).pct, 31);
  assert.ok(Math.abs(ratioParts(0.31).saved - 69) < 1e-9);
  assert.deepEqual(ratioParts(null), { pct: null, saved: null });
  assert.deepEqual(ratioParts(-1), { pct: null, saved: null });
});

test('jobsOf: a finished job has its duration and no age; a running one has an age (now minus its start) and no duration', () => {
  const j = jobs([ev('a', 90, { verdict: 'running', duration_ms: null }), ev('b', 120, { duration_ms: 30 * MIN }), ev('c', 60, { verdict: 'interrupted', duration_ms: null })]);
  const by = Object.fromEntries(j.map((x) => [x.id, x]));
  assert.equal(by.a.running, true);
  assert.equal(by.a.durationMs, null);
  assert.ok(Math.abs(by.a.ageMs - 90 * MIN) < 1);
  assert.equal(by.b.running, false);
  assert.equal(by.b.ageMs, null);
  assert.equal(by.b.durationMs, 30 * MIN);
  assert.equal(by.c.durationMs, null, 'an interrupted job has no duration: a dash, not zero');
  assert.equal(by.c.running, false);
  const noClock = jobsOf(shapeRuns([ev('a', 5, { verdict: 'running' })], MODELOPT), null);
  assert.equal(noClock[0].ageMs, null);
});

test('methodsOf: the methods seen, most frequent first (they give the bar colours)', () => {
  const j = jobs([ev('a', 4, { method: 'quantize' }), ev('b', 3, { method: 'reap' }), ev('c', 2, { method: 'reap' }), ev('d', 1, { method: null })]);
  assert.deepEqual(methodsOf(j), ['reap', 'quantize']);
});

test('timelineRows: the last jobs oldest first; the value is the duration, or the age of a running job; none for a finished job without one', () => {
  const j = jobs([ev('a', 300, {}), ev('b', 200, { duration_ms: null, verdict: 'failed' }), ev('c', 100, { verdict: 'running', duration_ms: null })]);
  const t = timelineRows(j);
  assert.deepEqual(
    t.map((r) => r.id),
    ['a', 'b', 'c'],
  );
  assert.equal(t[0].value, 40 * MIN);
  assert.equal(t[1].value, null);
  assert.ok(Math.abs(t[2].value - 100 * MIN) < 1);
  assert.equal(t[2].running, true);
  assert.deepEqual(
    timelineRows(j, { max: 2 }).map((r) => r.id),
    ['b', 'c'],
  );
});

test('sizeRows: only jobs with both sizes; the ratio from the sizes when the event has none; the jobs left out are counted (a running job is not one of them)', () => {
  const j = jobs([
    ev('a', 400, { base_bytes: 100, output_bytes: 30, ratio: 0.3 }),
    ev('b', 300, { base_bytes: 100, output_bytes: 60 }),
    ev('c', 200, { verdict: 'failed', duration_ms: 1000 }),
    ev('d', 100, { base_bytes: 0, output_bytes: 5 }),
    ev('e', 50, { verdict: 'running', duration_ms: null }),
  ]);
  const s = sizeRows(j);
  assert.deepEqual(
    s.rows.map((r) => r.id),
    ['a', 'b'],
  );
  assert.equal(s.rows[1].ratio, 0.6);
  assert.equal(s.missing, 2);
  assert.equal(s.total, 2);
  assert.equal(sizeRows(j, { max: 1 }).rows[0].id, 'b');
});

test('runningJobs: the age against the median finished job of the same method; flagged only past STUCK_FACTOR times that and STUCK_MIN_MS; no history means no flag', () => {
  const finished = [
    ev('f1', 900, { duration_ms: 40 * MIN }),
    ev('f2', 800, { duration_ms: 50 * MIN }),
    ev('f3', 700, { duration_ms: 60 * MIN, method: 'quantize' }),
    ev('f4', 600, { duration_ms: 20 * MIN, verdict: 'failed' }),
  ];
  const ok = runningJobs(jobs([...finished, ev('r', 70, { verdict: 'running', duration_ms: null })]));
  assert.equal(ok.length, 1);
  assert.equal(ok[0].typicalMs, 45 * MIN, 'the median of the finished reap jobs; a failed job and another method do not count');
  assert.equal(ok[0].stuck, false);
  const stuck = runningJobs(jobs([...finished, ev('r', 100, { verdict: 'running', duration_ms: null })]));
  assert.equal(stuck[0].stuck, true);
  assert.ok(100 * MIN > 45 * MIN * STUCK_FACTOR);
  const short = runningJobs(jobs([ev('f', 900, { duration_ms: 1 * MIN }), ev('r', 5, { verdict: 'running', duration_ms: null })]));
  assert.equal(short[0].stuck, false, 'five minutes is more than twice a one-minute job, but under STUCK_MIN_MS');
  assert.ok(5 * MIN < STUCK_MIN_MS);
  const fresh = runningJobs(jobs([ev('r', 500, { verdict: 'running', duration_ms: null })]));
  assert.equal(fresh[0].typicalMs, null);
  assert.equal(fresh[0].stuck, false, 'with nothing to compare with, an age alone says nothing');
  const other = runningJobs(jobs([ev('f', 900, { duration_ms: 10 * MIN, method: 'quantize' }), ev('r', 300, { verdict: 'running', duration_ms: null, method: 'minitron' })]));
  assert.equal(other[0].typicalMs, 10 * MIN, 'no finished job of this method: the median of all');
  assert.deepEqual(runningJobs(jobs([ev('a', 5, {})])), []);
});
