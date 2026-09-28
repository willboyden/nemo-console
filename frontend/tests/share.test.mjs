import test from 'node:test';
import assert from 'node:assert/strict';
import { normalizeCounts, sharePercents, roundedShares, topN, errorRate } from '../js/share.js';

test('normalizeCounts accepts maps, arrays, pairs and nested objects; drops junk', () => {
  assert.deepEqual(normalizeCounts({ a: 2, b: 3 }).map((r) => [r.name, r.value]), [['a', 2], ['b', 3]]);
  assert.deepEqual(normalizeCounts([{ name: 'x', calls: 4 }, ['y', 5], { route: 'z', count: 1 }]).map((r) => [r.name, r.value]), [['x', 4], ['y', 5], ['z', 1]]);
  assert.deepEqual(normalizeCounts({ m: { calls: 7 } })[0].value, 7);
  assert.deepEqual(normalizeCounts({ a: 'x', b: -1, c: NaN }), []);
  assert.deepEqual(normalizeCounts(null), []); assert.equal(normalizeCounts({ '': 1 })[0].name, '(none)');
});
test('sharePercents sorts descending and sums to 100; zero total gives 0%', () => {
  const rows = sharePercents({ a: 1, b: 3, c: 6 });
  assert.deepEqual(rows.map((r) => r.name), ['c', 'b', 'a']); assert.equal(Math.round(rows.reduce((s, r) => s + r.pct, 0)), 100); assert.equal(rows[0].pct, 60);
  assert.ok(sharePercents({ a: 0, b: 0 }).every((r) => r.pct === 0));
});
test('roundedShares: displayed tenths always sum to exactly 100.0 (largest remainder)', () => {
  for (const data of [{ a: 1, b: 1, c: 1 }, { a: 412, b: 96, c: 21, d: 8 }, { a: 1, b: 2, c: 3, d: 4, e: 5, f: 6, g: 7 }]) {
    const total = roundedShares(data).reduce((s, r) => s + Math.round(r.label * 10), 0); assert.equal(total, 1000);
  }
  assert.deepEqual(roundedShares({ a: 1, b: 1, c: 1 }).map((r) => r.label).sort(), [33.3, 33.3, 33.4]);
  assert.deepEqual(roundedShares({ a: 0 }).map((r) => r.label), [0]);
});
test('topN folds the tail into one "other" row and keeps the total', () => {
  const rows = topN({ a: 50, b: 20, c: 10, d: 10, e: 10 }, 2);
  assert.deepEqual(rows.map((r) => r.name), ['a', 'b', 'other']); assert.equal(rows[2].value, 30); assert.equal(rows[2].pct, 30); assert.equal(topN({ a: 1 }, 3).length, 1);
});
test('errorRate guards divide-by-zero', () => { assert.equal(errorRate(1, 0), 0); assert.equal(errorRate(5, 200), 2.5); });
