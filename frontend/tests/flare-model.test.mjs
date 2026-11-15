// The Flare models (addendum 9.16): the target grid from status, and the per-target charts from one series answer split by target.
import test from 'node:test';
import assert from 'node:assert/strict';
import { parseTargetDetail, targetState, targetGrid, targetsWithData, rateLines, tokenMix, loadByTarget, FLARE_KEYS, TARGET_STATES, MIX_KEYS } from '../js/flare-model.js';

const comp = (name, state, detail) => ({ kind: 'container', name, state, detail });
const SERVING = 'sm121 gb10 (spark-a spark-b), modes tp4 · serving glm-flash, context 524288 · image lab/flare:902c88e loaded · 1 package(s) · GPU busy: 1 CUDA process(es) on the GPU';
const BUSY = 'sm120 gb202 (local), modes tp1 tp2 · container absent · image lab/flare:902c88e loaded · 2 package(s) · GPU busy: GPU 0 holds 81611 MiB (> 3072)';
const FREE = 'sm120 gb202 (gpu-box), modes tp1 · container absent · image not loaded · 1 package(s) · GPU free';
const LOADING = 'sm121 gb10 (spark-c), modes tp1 · container running · image lab/flare:902c88e loaded · 0 package(s) · GPU busy: 1 CUDA process(es) on the GPU';
const EXITED = 'sm121 gb10 (spark-d), modes tp1 · container exited · image lab/flare:902c88e loaded · 1 package(s) · GPU free';
const UNREACH = 'sm121 gb10 (spark-e), modes tp1 · check failed: ssh timed out';

test('the one-line description of a target (flare.rs describe) is read part by part; a missing part is null', () => {
  const s = parseTargetDetail(SERVING);
  assert.deepEqual([s.chip, s.family, s.hosts, s.modes], ['sm121', 'gb10', ['spark-a', 'spark-b'], ['tp4']]);
  assert.deepEqual([s.serving, s.model, s.context, s.packages, s.imageLoaded, s.gpuBusy], [true, 'glm-flash', 524288, 1, true, true]);
  const b = parseTargetDetail(BUSY);
  assert.deepEqual([b.serving, b.container, b.gpuBusy, b.busyWhy, b.packages], [false, 'absent', true, 'GPU 0 holds 81611 MiB (> 3072)', 2]);
  assert.equal(b.hosts[0], 'local');
  assert.equal(parseTargetDetail(FREE).imageLoaded, false);
  assert.equal(parseTargetDetail(FREE).gpuFree, true);
  assert.equal(parseTargetDetail(UNREACH).error, 'ssh timed out');
  const none = parseTargetDetail('');
  assert.deepEqual([none.chip, none.packages, none.imageLoaded, none.model, none.hosts], [null, null, null, null, []]);
  assert.equal(parseTargetDetail(undefined).serving, false);
  assert.equal(parseTargetDetail('sm120 gb202 (x), modes tp1 · serving ? · image lab/flare:a loaded').model, '?');
});

test('the state of a target: serving, loading, busy (not ours), failed, stopped, absent, unknown', () => {
  const st = (c, detail, starting = false) => targetState(c, parseTargetDetail(detail), starting);
  assert.equal(st(comp('flare @ a', 'running', SERVING), SERVING), 'serving');
  assert.equal(st(comp('flare @ c', 'running', LOADING), LOADING), 'loading');
  assert.equal(st(comp('flare @ x', 'absent', FREE), FREE, true), 'loading', 'a start under way is loading');
  assert.equal(st(comp('flare @ l', 'absent', BUSY), BUSY), 'busy');
  assert.equal(st(comp('flare @ d', 'failed', EXITED), EXITED), 'failed');
  assert.equal(st(comp('flare @ g', 'absent', FREE), FREE), 'absent');
  assert.equal(st(comp('flare @ e', 'unknown', UNREACH), UNREACH), 'unknown');
  assert.equal(st(comp('flare @ s', 'stopped', FREE.replace('container absent', 'container created')), FREE.replace('container absent', 'container created')), 'stopped');
  assert.ok(TARGET_STATES.includes('busy'));
});

test('the grid: targets in the launcher order grouped by chip, the package count from the metric wins, counts by state', () => {
  const status = {
    components: [
      { kind: 'process', name: 'flare-local', state: 'running', detail: 'the lab launcher' },
      comp('flare @ local', 'absent', BUSY),
      comp('flare @ gpu-box', 'absent', FREE),
      comp('flare @ spark-all', 'running', SERVING),
      comp('flare @ spark-c', 'running', LOADING),
      comp('start on spark-c', 'running', 'flare-local up spark-c pkg'),
    ],
  };
  const points = [
    { key: 'flare.packages', value: 7, labels: { target: 'local' } },
    { key: 'flare.packages', value: 0, labels: { target: 'spark-c' } },
    { key: 'flare.other', value: 9, labels: { target: 'local' } },
  ];
  const g = targetGrid(status, points);
  assert.deepEqual(
    g.targets.map((t) => t.target),
    ['local', 'gpu-box', 'spark-all', 'spark-c'],
  );
  assert.equal(g.targets[0].packages, 7, 'the metric wins over the text (2)');
  assert.equal(g.targets[1].packages, 1, 'with no metric the text is used');
  assert.equal(g.targets[3].packages, 0, 'a real 0 is kept');
  assert.deepEqual(
    g.groups.map((x) => [x.chip, x.targets.length]),
    [
      ['sm120', 2],
      ['sm121', 2],
    ],
  );
  assert.deepEqual([g.counts.busy, g.counts.absent, g.counts.serving, g.counts.loading], [1, 1, 1, 1]);
  assert.equal(g.launcher.state, 'running');
  assert.equal(g.empty, false);
  assert.deepEqual(g.targets[2].hosts, ['spark-a', 'spark-b']);
  assert.equal(g.targets[2].model, 'glm-flash');
});

test('no targets (not installed, or the first report is still running): empty, no crash on garbage', () => {
  assert.equal(targetGrid({ components: [{ name: 'flare-local', state: 'absent' }] }).empty, true);
  for (const bad of [null, undefined, {}, { components: 'x' }, { components: [null, { name: 3 }] }]) assert.equal(targetGrid(bad, 'nope').empty, true);
  assert.equal(targetGrid({ components: [comp('flare @ a', 'absent', '')] }).targets[0].packages, null, 'no package figure anywhere: an en dash, not 0');
});

// ---- the series

const T0 = 10_000;
const N = 60;
const grid = (f) => Array.from({ length: N }, (_, i) => [T0 + i * 60, f(i), f(i)]);
const counter = (key, target, perSec, reset = null) => ({
  key,
  label: { target },
  points: grid((i) => (reset != null && i >= reset ? (i - reset) * 60 * perSec : i * 60 * perSec + 1000)),
});
const gauge = (key, target, v) => ({ key, label: { target }, points: grid(() => v) });
const answer = (series) => ({ window: '1h', step_s: 60, from: T0, to: T0 + (N - 1) * 60, series, truncated: false });
const A = answer([
  counter('flare.generated_total', 'local', 30),
  counter('flare.generated_total', 'spark-all', 60),
  counter('flare.prefill_total', 'local', 150),
  counter('flare.cache_reused_total', 'local', 80),
  counter('flare.requests_completed_total', 'local', 0.05),
  gauge('flare.active_requests', 'local', 2),
  gauge('flare.pending_requests', 'local', 0),
  gauge('flare.active_requests', 'spark-all', 3),
  { key: 'flare.generated_total', label: null, points: grid((i) => i) }, // no target label: never a line
]);

test('the keys of the one request are at most 12 and the targets with data are the ones with a counter', () => {
  assert.ok(FLARE_KEYS.length <= 12);
  assert.deepEqual(targetsWithData(A), ['local', 'spark-all']);
  assert.deepEqual(targetsWithData(answer([])), []);
  assert.deepEqual(targetsWithData(null), []);
});

test('a counter becomes a rate per target (per second, or per minute when slow); lines are named by target', () => {
  const g = rateLines(A, 'flare.generated_total');
  assert.equal(g.per, 's');
  assert.deepEqual(
    g.lines.map((l) => l.name),
    ['local', 'spark-all'],
  );
  const last = g.lines[0].segments.at(-1).at(-1);
  assert.ok(Math.abs(last[1] - 30) < 1e-6, `rate ${last[1]}`);
  const slow = rateLines(A, 'flare.requests_completed_total');
  assert.equal(slow.per, 'min', 'a busiest rate under 0.1/s is shown per minute');
  assert.ok(Math.abs(slow.lines[0].segments[0][0][1] - 3) < 1e-6);
  assert.deepEqual(rateLines(A, 'flare.requests_failed_total').lines, []);
});

test('a counter that restarts leaves a gap and the stack counts what accumulated since; one restart is one, not one per counter', () => {
  const R = answer([counter('flare.generated_total', 'local', 30, 30), counter('flare.prefill_total', 'local', 100, 30), counter('flare.cache_reused_total', 'local', 50, 30)]);
  const m = tokenMix(R);
  assert.equal(m.rows[0].resets, 1);
  const segs = rateLines(R, 'flare.generated_total').lines[0].segments;
  assert.equal(segs.length, 2, 'the line is cut at the restart');
  assert.ok(m.rows[0].values.generated > 0);
  assert.ok(
    segs.flat().every((p) => p[1] >= 0),
    'never a negative rate',
  );
});

test('token mix: the increase of each counter over the window per target, and the rate over time on one grid; a counter a target lacks is null, not 0', () => {
  const m = tokenMix(A);
  assert.deepEqual(m.targets, ['local', 'spark-all']);
  assert.deepEqual(MIX_KEYS, ['prefill', 'generated', 'reused']);
  const local = m.rows.find((r) => r.target === 'local');
  assert.ok(Math.abs(local.values.generated - 30 * 59 * 60) < 1e-6);
  assert.ok(Math.abs(local.values.prefill - 150 * 59 * 60) < 1e-6);
  const sp = m.rows.find((r) => r.target === 'spark-all');
  assert.equal(sp.values.prefill, null);
  assert.equal(sp.values.reused, null);
  const tl = m.time.find((x) => x.target === 'spark-all');
  assert.deepEqual(
    tl.series.map((s) => s.key),
    ['generated'],
    'only the counters it has',
  );
  const lt = m.time.find((x) => x.target === 'local');
  assert.equal(lt.ts.length, N);
  assert.equal(lt.series.length, 3);
  assert.equal(lt.series[0].values[0], null, 'the first bucket has no rate');
  assert.ok(Math.abs(lt.series[1].values[5] - 30) < 1e-6);
});

test('active vs pending: the newest figure and the highest, on one grid; pending that was never reported is null', () => {
  const l = loadByTarget(A);
  assert.deepEqual(l.targets, ['local', 'spark-all']);
  const local = l.rows.find((r) => r.target === 'local');
  assert.deepEqual([local.values.active, local.values.pending, local.peakActive], [2, 0, 2]);
  const sp = l.rows.find((r) => r.target === 'spark-all');
  assert.equal(sp.values.pending, null);
  assert.equal(sp.peakPending, null);
  assert.deepEqual(
    l.time.find((x) => x.target === 'spark-all').series.map((s) => s.key),
    ['active'],
  );
  assert.deepEqual(loadByTarget(answer([])).rows, []);
  assert.deepEqual(tokenMix(null).rows, []);
});
