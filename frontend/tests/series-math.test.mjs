// The wave-2 series math (addendum 9.14): counter increase with resets, means / peaks, grids with gaps, rebinning. A missing value is null, never 0.
import test from 'node:test';
import assert from 'node:assert/strict';
import {
  counterIncrease,
  sumIncrease,
  meanOf,
  lastOf,
  peakOf,
  groupByLabel,
  gridSize,
  alignToGrid,
  mergeFactor,
  rebin,
  rebinTimes,
  arrayStats,
  baseKey,
  seriesLabels,
  subAnswer,
} from '../js/series-math.js';

const pts = (...v) => v.map(([t, a, m]) => [t, a, m ?? a]);

test('counterIncrease: the sum of the positive steps, a gap loses nothing, fewer than two points is null (not 0)', () => {
  const inc = counterIncrease(pts([0, 10], [60, 25], [120, 40], [600, 100]));
  assert.equal(inc.value, 90);
  assert.equal(inc.resets, 0);
  assert.equal(inc.seconds, 600);
  assert.equal(counterIncrease(pts([0, 5])), null);
  assert.equal(counterIncrease([]), null);
  assert.equal(counterIncrease(undefined), null);
  assert.equal(counterIncrease(pts([0, 7], [60, 7])).value, 0, 'a counter that did not move grew by 0: a real zero');
});

test('counterIncrease: a reset counts as the new value (Prometheus semantics) and is reported, so the figure is a lower bound', () => {
  const inc = counterIncrease(pts([0, 100], [60, 130], [120, 5], [180, 25]));
  assert.equal(inc.value, 30 + 5 + 20);
  assert.equal(inc.resets, 1);
  const two = counterIncrease(pts([0, 100], [60, 3], [120, 50], [180, 4]));
  assert.equal(two.resets, 2);
  assert.equal(two.value, 3 + 47 + 4);
});

test('counterIncrease: unsorted points are sorted, non-finite values are dropped', () => {
  const inc = counterIncrease([
    [120, 40, 40],
    [0, 10, 10],
    [60, NaN, NaN],
    [90, Infinity, 1],
    [60, 20, 20],
  ]);
  assert.equal(inc.value, 30);
  assert.equal(inc.n, 3);
});

test('sumIncrease adds the lines that have two points and ignores the rest; none at all is null', () => {
  const a = { points: pts([0, 1], [60, 4]) };
  const b = { points: pts([0, 10], [60, 12], [120, 1]) };
  const lone = { points: pts([0, 99]) };
  const s = sumIncrease([a, b, lone]);
  assert.equal(s.value, 3 + 2 + 1);
  assert.equal(s.lines, 2);
  assert.equal(s.resets, 1);
  assert.equal(sumIncrease([lone]), null);
  assert.equal(sumIncrease([]), null);
});

test('meanOf / lastOf / peakOf: null when there is nothing, the max column for the peak', () => {
  assert.equal(meanOf(pts([0, 2], [1, 4])), 3);
  assert.equal(meanOf([]), null);
  assert.equal(lastOf(pts([5, 1], [1, 9])), 1, 'the newest by time, not by position');
  assert.equal(lastOf([]), null);
  assert.equal(peakOf(pts([0, 2, 7], [1, 4, 5])), 7);
  assert.equal(peakOf(pts([0, 2], [1, 4])), 4);
  assert.equal(peakOf([]), null);
});

test('groupByLabel keeps insertion order and leaves a series without the label out', () => {
  const m = groupByLabel([{ label: { gpu: '1' } }, { label: { gpu: '0' } }, { label: null }, { label: { gpu: '1' } }, { label: { model: 'x' } }], 'gpu');
  assert.deepEqual([...m.keys()], ['1', '0']);
  assert.equal(m.get('1').length, 2);
});

test('gridSize, alignToGrid: a slot with no sample is null, a point lands in its nearest slot, out-of-range points are dropped', () => {
  assert.equal(gridSize(0, 600, 60), 11);
  assert.equal(gridSize(0, 0, 60), 1);
  assert.equal(gridSize(600, 0, 60), 0);
  assert.equal(gridSize(null, 600, 60), 0);
  assert.equal(gridSize(0, 600, 0), 0);
  const v = alignToGrid(pts([0, 1], [60, 2], [240, 5], [9999, 7], [-60, 8]), { from: 0, stepS: 60, n: 6 });
  assert.deepEqual(v, [1, 2, null, null, 5, null]);
  assert.deepEqual(alignToGrid([], { from: 0, stepS: 60, n: 3 }), [null, null, null]);
  assert.deepEqual(alignToGrid(pts([0, 1]), { from: 0, stepS: 0, n: 2 }), [null, null]);
});

test('mergeFactor and rebin: the mean of the values present, a merged slot with none stays null, max on request', () => {
  assert.equal(mergeFactor(336, 168), 2);
  assert.equal(mergeFactor(60, 160), 1);
  assert.equal(mergeFactor(337, 168), 3);
  assert.deepEqual(rebin([1, 3, null, null, 5, null, 9], 2), [2, null, 5, 9]);
  assert.deepEqual(rebin([1, 3, 2], 2, 'max'), [3, 2]);
  assert.deepEqual(rebin([1, 2], 1), [1, 2]);
  assert.deepEqual(rebinTimes(1000, 60, 5, 2), [1000, 1120, 1240]);
});

test('arrayStats: latest is the newest non-null value; all null gives nulls', () => {
  assert.deepEqual(arrayStats([1, null, 5, null]), { n: 2, latest: 5, min: 1, avg: 3, max: 5 });
  assert.deepEqual(arrayStats([null, null]), { n: 0, latest: null, min: null, avg: null, max: null });
  assert.deepEqual(arrayStats(undefined), { n: 0, latest: null, min: null, avg: null, max: null });
});

test('baseKey, seriesLabels, subAnswer: the route answers with the key as requested, a labelled request keeps its selector', () => {
  assert.equal(baseKey('dyn.tokens{kind=completion}'), 'dyn.tokens');
  assert.equal(baseKey('dyn.tokens'), 'dyn.tokens');
  assert.deepEqual(seriesLabels({ key: 'dyn.tokens{kind=completion}', label: null }), { kind: 'completion' });
  assert.deepEqual(seriesLabels({ key: 'a.b{x=1,y=2}', label: { y: '3' } }), { x: '1', y: '3' }, 'the parsed label wins');
  assert.deepEqual(seriesLabels({ key: 'a.b', label: { q: 'p50' } }), { q: 'p50' });
  const ans = {
    window: '1h',
    step_s: 60,
    from: 0,
    to: 60,
    truncated: false,
    series: [
      { key: 'a.b', label: { q: 'p50' }, points: [] },
      { key: 'c.d{k=1}', label: null, points: [] },
      { key: 'c.d{k=2}', label: null, points: [] },
    ],
  };
  assert.equal(subAnswer(ans, 'a.b').series.length, 1);
  assert.equal(subAnswer(ans, 'c.d').series.length, 2, 'a bare key matches every selector');
  assert.equal(subAnswer(ans, ['a.b', 'c.d'], (s) => s.key.includes('k=2')).series.length, 1);
  assert.equal(subAnswer(ans, 'a.b').step_s, 60, 'the window, step and range stay');
  assert.deepEqual(subAnswer(null, 'x').series, []);
});
