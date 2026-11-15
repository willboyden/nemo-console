// NeMo Gym cards (addendum 9.15): the reward distribution of a run (A/B overlay), rewarded-of-rollouts rows and the trends by model, from `gym.smoke`
// events. A run that recorded no rewards has none (null in the event): an empty list, never a list of zeros.
import test from 'node:test';
import assert from 'node:assert/strict';
import { rewardsOf, rewardStats, rewardBins, rolloutRows, modelTrends, MAX_REWARDS } from '../js/gym-model.js';
import { shapeRuns } from '../js/runs.js';
import { GYM } from '../js/run-descriptors.js';

const ev = (id, ts, model, over = {}) => ({
  ts,
  system: 'gym',
  kind: 'gym.smoke',
  summary: '',
  data: { run_id: id, model, limit: 5, verdict: 'succeeded', duration_ms: 1000, rewards: null, ...over },
});
const rows = (events) => shapeRuns(events, GYM);

test('rewardsOf: the finite numbers in task order, at most 20; null, missing and junk give none', () => {
  const [a, b, c, d] = rows([
    ev('a', 1, 'm', { rewards: [1, 0, 0.5, null, 'x', NaN] }),
    ev('b', 2, 'm', { rewards: null }),
    ev('c', 3, 'm', {}),
    ev('d', 4, 'm', { rewards: Array.from({ length: 30 }, (_, i) => i / 30) }),
  ]);
  assert.deepEqual(rewardsOf(a), [1, 0, 0.5]);
  assert.deepEqual(rewardsOf(b), []);
  assert.deepEqual(rewardsOf(c), []);
  assert.equal(rewardsOf(d).length, MAX_REWARDS);
  assert.deepEqual(rewardsOf(null), []);
});

test('rewardStats: counts, mean, the extremes and how many are at full reward or zero; nothing is null', () => {
  assert.deepEqual(rewardStats([1, 1, 0, 0.5]), { n: 4, mean: 0.625, min: 0, max: 1, full: 2, zero: 1 });
  assert.deepEqual(rewardStats([]), { n: 0, mean: null, min: null, max: null, full: 0, zero: 0 });
});

test('rewardBins: ten shared bins over 0..1; every reward in exactly one bin; the top edge is in the last bin; B uses the same edges', () => {
  const b = rewardBins([0, 0.05, 0.5, 0.95, 1, 1], [1, 0.5]);
  assert.equal(b.edges.length, 11);
  assert.equal(b.labels.length, 10);
  assert.deepEqual([b.lo, b.hi], [0, 1]);
  assert.equal(
    b.a.reduce((x, y) => x + y, 0),
    6,
  );
  assert.deepEqual([b.a[0], b.a[5], b.a[9]], [2, 1, 3], '1 and 1 and 0.95 are in the last bin');
  assert.equal(b.b[9], 1);
  assert.equal(b.b[5], 1);
  assert.equal(b.labels[0], '0.0–0.1');
  assert.equal(b.labels[9], '0.9–1.0');
  assert.equal(rewardBins([1], null).b, null);
  const none = rewardBins([], []);
  assert.deepEqual([none.lo, none.hi], [0, 1]);
  assert.ok(none.a.every((n) => n === 0));
});

test('rewardBins: a reward outside 0..1 widens the range to whole steps; nothing falls off the ends', () => {
  const b = rewardBins([-0.2, 0.5, 1.4]);
  assert.ok(b.lo <= -0.2 && b.hi >= 1.4);
  assert.equal(
    b.a.reduce((x, y) => x + y, 0),
    3,
  );
  assert.equal(b.edges.length, 11);
});

test('rolloutRows: rollouts with their rewarded share; a run with no rollouts is left out; the last `max` runs', () => {
  const rs = rows([
    ev('a', 1, 'm', { rollouts: 10, rewarded: 9 }),
    ev('b', 2, 'm', {}),
    ev('c', 3, 'm', { rollouts: 5 }),
    ev('d', 4, 'm', { rollouts: 5, rewarded: 7 }),
    ev('e', 5, 'm', { rollouts: 0, rewarded: 0 }),
  ]);
  const r = rolloutRows(rs);
  assert.deepEqual(
    r.map((x) => x.id),
    ['a', 'c', 'd'],
  );
  assert.deepEqual([r[0].rewarded, r[0].none], [9, 1]);
  assert.deepEqual([r[1].rewarded, r[1].none], [null, null], 'no `rewarded` in the event: unknown, not 0');
  assert.deepEqual([r[2].rewarded, r[2].none], [5, 0], 'more rewarded than rollouts is capped');
  assert.equal(rolloutRows(rs, { max: 2 }).length, 2);
  assert.equal(rolloutRows(rs, { max: 2 })[1].id, 'd');
});

test('modelTrends: one group per model for the mean reward and for pass@1; a run without the number is not a point', () => {
  const rs = rows([
    ev('a', 1, 'x', { mean_reward: 0.5, pass_at_1: 0.4 }),
    ev('b', 2, 'y', { mean_reward: 0.9 }),
    ev('c', 3, 'x', { mean_reward: 0.7, pass_at_1: 0.6 }),
    ev('d', 4, 'x', { verdict: 'failed' }),
  ]);
  const t = modelTrends(rs);
  assert.deepEqual(
    t.mean_reward.map((g) => g.key),
    ['x', 'y'],
  );
  assert.deepEqual(
    t.mean_reward[0].points.map((p) => p.id),
    ['a', 'c'],
  );
  assert.deepEqual(
    t.pass_at_1[0].points.map((p) => p.v),
    [0.4, 0.6],
  );
  assert.deepEqual(t.pass_at_1[1].points, [], 'y has no pass@1: no point (not zero)');
  assert.equal(t.mean_reward[0].count, 3);
});
