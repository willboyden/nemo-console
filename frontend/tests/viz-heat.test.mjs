// Heat ramp, matrix geometry and swim-lane geometry (addendum 9.14): pure, no DOM.
import test from 'node:test';
import assert from 'node:assert/strict';
import { levelOf, levelStart, runLengths, matrixLayout, HEAT_LEVELS } from '../js/viz/heat.js';
import { timelineLayout } from '../js/viz/timeline.js';

test('levelOf: equal parts of the domain, clamped at the ends, no data is null and never the lowest step', () => {
  const d = { min: 0, max: 90 };
  assert.equal(levelOf(0, d), 0);
  assert.equal(levelOf(89.99, d), HEAT_LEVELS - 1);
  assert.equal(levelOf(90, d), HEAT_LEVELS - 1);
  assert.equal(levelOf(500, d), HEAT_LEVELS - 1, 'above the domain: the top step');
  assert.equal(levelOf(-5, d), 0, 'below: the bottom step');
  assert.equal(levelOf(45, d), 4);
  assert.equal(levelOf(null, d), null);
  assert.equal(levelOf(undefined, d), null);
  assert.equal(levelOf(NaN, d), null);
  assert.equal(levelOf('7', d), null, 'a string is not a value');
  assert.equal(levelOf(3, { min: 5, max: 5 }), 0);
  assert.equal(levelOf(6, { min: 5, max: 5 }), HEAT_LEVELS - 1);
});

test('levelOf with the sqrt curve spreads the low end; levelStart is its inverse at the step edges', () => {
  const d = { min: 0, max: 1, curve: 'sqrt' };
  assert.ok(levelOf(0.05, d) >= 1, 'a quiet cell is still visible');
  assert.ok(levelOf(0.05, d) > levelOf(0.05, { min: 0, max: 1 }), 'where a linear ramp would show it as the lowest step');
  for (let i = 0; i < HEAT_LEVELS; i++) assert.equal(levelOf(levelStart(i, d) + 1e-9, d), i);
  assert.equal(levelStart(0, { min: 30, max: 90 }), 30);
});

test('runLengths merges neighbours with the same step, and no-data cells with each other', () => {
  assert.deepEqual(runLengths([1, 1, 2, null, null, 2, 2, 0]), [
    { c0: 0, c1: 2, level: 1 },
    { c0: 2, c1: 3, level: 2 },
    { c0: 3, c1: 5, level: null },
    { c0: 5, c1: 7, level: 2 },
    { c0: 7, c1: 8, level: 0 },
  ]);
  assert.deepEqual(runLengths([]), []);
});

test('matrixLayout: rows stack with the gaps, colAt / rowAt map a pixel back to its cell, -1 outside', () => {
  const m = matrixLayout({ rows: 3, cols: 10, width: 300, labelW: 100, cellH: 20, rowGap: [0, 0, 10], top: 4, bottom: 20, right: 10 });
  assert.equal(m.x0, 100);
  assert.equal(m.x1, 290);
  assert.equal(m.cw, 19);
  assert.deepEqual(m.rowY, [4, 24, 54]);
  assert.equal(m.height, 4 + 20 * 3 + 10 + 20);
  assert.equal(m.colAt(100), 0);
  assert.equal(m.colAt(118.9), 0);
  assert.equal(m.colAt(119), 1);
  assert.equal(m.colAt(289.9), 9);
  assert.equal(m.colAt(99), -1);
  assert.equal(m.colAt(290), -1);
  assert.equal(m.rowAt(5), 0);
  assert.equal(m.rowAt(25), 1);
  assert.equal(m.rowAt(46), -1, 'in the gap above row 2');
  assert.equal(m.rowAt(60), 2);
  assert.equal(m.colX(3), 100 + 3 * 19);
  const empty = matrixLayout({ rows: 0, cols: 0, width: 200 });
  assert.equal(empty.colAt(150), -1);
  assert.deepEqual(empty.rowY, []);
});

test('timelineLayout: marks sit at their time, close neighbours in a lane alternate sub-rows, lanes are separate', () => {
  const items = [
    { id: 'a', ts: 0, lane: '0' },
    { id: 'b', ts: 1, lane: '0' },
    { id: 'c', ts: 2, lane: '0' },
    { id: 'd', ts: 500, lane: '0' },
    { id: 'e', ts: 250, lane: '1' },
  ];
  const g = timelineLayout({ items, lanes: ['0', '1'], from: 0, to: 1000, width: 700 });
  const by = Object.fromEntries(g.marks.map((m) => [m.item.id, m]));
  assert.ok(by.a.x < by.d.x && by.d.x < 700);
  assert.deepEqual([by.a.sub, by.b.sub, by.c.sub], [0, 1, 0], 'marks that would overlap alternate');
  assert.equal(by.d.sub, 0);
  assert.ok(by.e.y > by.a.y + g.laneH / 2, 'lane 1 is below lane 0');
  assert.equal(g.height, 4 + 2 * g.laneH + 22);
  assert.ok(g.ticks.length >= 2);
  const none = timelineLayout({ items: [], lanes: [], from: 0, to: 10, width: 300 });
  assert.deepEqual(none.marks, []);
});
