// The up / down strip model (addendum 9.16): cells are up / down / partial / no data, a gap is never down, the uptime is over what was sampled.
import test from 'node:test';
import assert from 'node:assert/strict';
import { stripFromPoints, stripFromAnswer, downSeconds, STRIP_STATES } from '../js/strip-model.js';

const T0 = 1_000_000;
const pts = (vals, step = 60) => vals.map((v, i) => (v == null ? null : [T0 + i * step, v, v])).filter(Boolean);

test('every sample up: all cells up, uptime 1, no down run', () => {
  const m = stripFromPoints(pts([1, 1, 1, 1]), { from: T0, to: T0 + 180, stepS: 60, maxCells: 60 });
  assert.deepEqual(
    m.cells.map((c) => c.state),
    ['up', 'up', 'up', 'up'],
  );
  assert.equal(m.uptime, 1);
  assert.equal(m.downCells, 0);
  assert.deepEqual(m.downRuns, []);
  assert.equal(m.longestDown, null);
  assert.equal(m.empty, false);
});

test('a gap is "no data", never down: it is left out of the uptime too', () => {
  const m = stripFromPoints(pts([1, null, null, 0]), { from: T0, to: T0 + 180, stepS: 60 });
  assert.deepEqual(
    m.cells.map((c) => c.state),
    ['up', 'none', 'none', 'down'],
  );
  assert.equal(m.known, 2);
  assert.equal(m.uptime, 0.5);
  assert.equal(m.cells[1].share, null);
});

test('down runs: neighbouring down cells are one run, a partial or no-data cell ends it, the longest is kept', () => {
  const v = [1, 0, 0, 1, 0, 1, 0.5, 0, 0, 0];
  const m = stripFromPoints(pts(v), { from: T0, to: T0 + 9 * 60, stepS: 60 });
  assert.deepEqual(
    m.cells.map((c) => c.state),
    ['up', 'down', 'down', 'up', 'down', 'up', 'part', 'down', 'down', 'down'],
  );
  assert.deepEqual(
    m.downRuns.map((r) => r.cells),
    [2, 1, 3],
  );
  assert.equal(m.longestDown.cells, 3);
  assert.equal(m.longestDown.from, T0 + 7 * 60);
  assert.equal(m.longestDown.to, T0 + 10 * 60);
  assert.equal(m.downCells, 6);
  assert.equal(downSeconds(m), 6 * 60);
});

test('a bucket that was up for part of the minute is a partial cell and counts by its share', () => {
  const m = stripFromPoints(pts([1, 0.25]), { from: T0, to: T0 + 60, stepS: 60 });
  assert.equal(m.cells[1].state, 'part');
  assert.equal(m.cells[1].share, 0.25);
  assert.ok(Math.abs(m.uptime - 0.625) < 1e-9);
});

test('more buckets than cells are merged: a cell with both up and down samples is partial; the merged width is the bin', () => {
  const v = Array.from({ length: 120 }, (_, i) => (i === 7 ? 0 : 1));
  const m = stripFromPoints(pts(v), { from: T0, to: T0 + 119 * 60, stepS: 60, maxCells: 60 });
  assert.equal(m.cells.length, 60);
  assert.equal(m.binS, 120);
  assert.equal(m.cells[3].state, 'part'); // buckets 6 and 7: one down
  assert.equal(m.cells.filter((c) => c.state === 'up').length, 59);
});

test('no samples at all: empty, uptime null (not 0 and not 1)', () => {
  const m = stripFromPoints([], { from: T0, to: T0 + 120, stepS: 60 });
  assert.equal(m.empty, true);
  assert.equal(m.uptime, null);
  assert.ok(m.cells.every((c) => c.state === 'none'));
  assert.equal(stripFromPoints([], {}).cells.length, 0);
});

test('values outside 0..1 are clamped; states are the documented four', () => {
  const m = stripFromPoints(pts([5, -2]), { from: T0, to: T0 + 60, stepS: 60 });
  assert.deepEqual(
    m.cells.map((c) => c.state),
    ['up', 'down'],
  );
  assert.deepEqual(STRIP_STATES, ['up', 'down', 'part', 'none']);
});

test('stripFromAnswer reads one key of a series answer and is null when the key is absent', () => {
  const a = {
    window: '1h',
    step_s: 60,
    from: T0,
    to: T0 + 120,
    series: [
      { key: 'gr.up', label: null, points: pts([1, 1, 0]) },
      { key: 'gr.configs', label: null, points: pts([3, 3, 3]) },
    ],
    truncated: false,
  };
  assert.equal(stripFromAnswer(a, 'gr.up').cells.length, 3);
  assert.equal(stripFromAnswer(a, 'gr.up').cells[2].state, 'down');
  assert.equal(stripFromAnswer(a, 'os.gateway_up'), null);
  assert.equal(stripFromAnswer(null, 'gr.up'), null);
});

test('partial cells are counted: a dip shorter than a cell is not claimed as "no trouble"', () => {
  const v = Array.from({ length: 120 }, (_, i) => (i === 7 ? 0 : 1));
  const m = stripFromPoints(pts(v), { from: T0, to: T0 + 119 * 60, stepS: 60, maxCells: 60 });
  assert.equal(m.partCells, 1);
  assert.equal(m.downRuns.length, 0);
  assert.ok(m.uptime < 1);
  assert.equal(stripFromPoints(pts([1, 1, 1]), { from: T0, to: T0 + 120, stepS: 60 }).partCells, 0);
});
