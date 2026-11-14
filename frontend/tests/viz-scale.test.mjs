import test from 'node:test';
import assert from 'node:assert/strict';
import { linear, logScale, niceScale, niceStep, logTicks, fmtOffsetMs, offsetTicks, clockTicks, spreadIndices, binPosition, fmtDay, fmtHM } from '../js/viz/scale.js';
import { shortId, fmtDur, tokenSegments, shareOf, byKey, categoryOf, isRoutingMark, fmtStamp } from '../js/viz/fmt.js';

test('linear maps and inverts; a zero-width domain maps to the middle', () => {
  const f = linear(0, 10, 100, 200);
  assert.equal(f(5), 150);
  assert.equal(f.invert(150), 5);
  assert.equal(linear(3, 3, 0, 10)(3), 5);
});
test('logScale clamps zero and negatives to the floor instead of NaN', () => {
  const f = logScale(1, 1000, 0, 300);
  assert.equal(Math.round(f(10)), 100);
  assert.equal(f(0), f(1));
  assert.ok(Number.isFinite(f(-5)));
  assert.deepEqual(logTicks(3, 4500), [1, 10, 100, 1000, 10000]);
});
test('niceScale extends the bounds so every datum fits, and survives degenerate input', () => {
  const s = niceScale(0, 7, 4);
  assert.deepEqual([s.min, s.max], [0, 8]);
  assert.ok(s.ticks.includes(8));
  assert.ok(niceScale(0, 0).max >= 1);
  assert.deepEqual(niceScale(NaN, 3).ticks, [0, 1]);
  assert.equal(niceStep(100, 4), 50);
  assert.equal(niceStep(0), 1);
  const big = niceScale(0, 123456, 4);
  assert.ok(big.max >= 123456 && big.ticks.length <= 8);
});
test('offset labels and ticks read naturally', () => {
  assert.equal(fmtOffsetMs(0), '0');
  assert.equal(fmtOffsetMs(250), '250ms');
  assert.equal(fmtOffsetMs(1500), '1.5s');
  assert.equal(fmtOffsetMs(123000), '2m03s');
  assert.equal(fmtOffsetMs(3900000), '1h05m');
  assert.equal(fmtOffsetMs(null), '–');
  assert.deepEqual(
    offsetTicks(0, 2300, 6).map((t) => t.label),
    ['0', '500ms', '1s', '1.5s', '2s'],
  );
  assert.ok(offsetTicks(0, 86400000 * 10, 6).length <= 7);
  assert.equal(offsetTicks(5, 5).length, 1);
});
test('clock ticks align to UTC and label midnight with the day', () => {
  const t0 = Date.UTC(2026, 10, 1, 22, 10) / 1000;
  const t = clockTicks(t0, t0 + 30 * 3600, 6);
  assert.ok(t.length >= 3 && t.length <= 7);
  assert.ok(t.every((x) => x.v % 60 === 0));
  assert.ok(
    t.some((x) => x.label === '11-02' && x.major),
    JSON.stringify(t),
  );
  const days = clockTicks(t0, t0 + 20 * 86400, 6);
  assert.ok(days.every((x) => /^\d\d-\d\d$/.test(x.label)));
  assert.equal(fmtDay(t0), '11-01');
  assert.equal(fmtHM(t0), '22:10');
});
test('spreadIndices keeps both ends; binPosition interpolates inside a bin and clamps outside', () => {
  assert.deepEqual(spreadIndices(3, 6), [0, 1, 2]);
  const s = spreadIndices(25, 5);
  assert.equal(s[0], 0);
  assert.equal(s.at(-1), 24);
  assert.ok(s.length <= 5);
  assert.equal(binPosition([0, 10, 20], 5), 0.5);
  assert.equal(binPosition([0, 10, 20], 15), 1.5);
  assert.equal(binPosition([0, 10, 20], -1), 0);
  assert.equal(binPosition([0, 10, 20], 99), 2);
  assert.equal(binPosition([0, 10, 20], null), null);
});

test('formatting: short ids, durations, stamps, token segments, shares, null-last sorting, categories', () => {
  assert.equal(shortId('01a0f1e0-04e6-7fff-860d-115024be3c5d'), '01a0f1e0…3c5d');
  assert.equal(shortId('abc'), 'abc');
  assert.equal(shortId(null), '–');
  assert.equal(fmtDur(950), '950 ms');
  assert.equal(fmtDur(125000), '2m 05s');
  assert.equal(fmtDur(null), '–');
  assert.equal(fmtStamp(Date.UTC(2026, 10, 2, 8, 5, 9) / 1000), '11-02 08:05:09');
  const seg = tokenSegments(100, 50, 40);
  assert.equal(Math.round((seg.uncached + seg.cached + seg.output) * 1000), 1000);
  assert.equal(tokenSegments(100, 0, 500).cached, 1, 'cached beyond input is clamped');
  assert.equal(tokenSegments(null, null, null).total, 0);
  assert.equal(shareOf(5, 10), 0.5);
  assert.equal(shareOf(null, 10), 0);
  assert.equal(shareOf(20, 10), 1);
  const rows = [{ v: 2 }, { v: null }, { v: 1 }];
  assert.deepEqual(
    rows
      .slice()
      .sort(byKey('v', 'asc'))
      .map((r) => r.v),
    [1, 2, null],
  );
  assert.deepEqual(
    rows
      .slice()
      .sort(byKey('v', 'desc'))
      .map((r) => r.v),
    [2, 1, null],
    'nulls stay last in both directions',
  );
  assert.equal(categoryOf({ category: 'LLM' }), 'llm');
  assert.equal(categoryOf({ category: 'custom' }), 'turn');
  assert.equal(categoryOf({ category: 'whatever' }), 'other');
  assert.equal(categoryOf(null), 'other');
  assert.ok(isRoutingMark({ name: 'switchyard.routing.decision' }));
  assert.ok(isRoutingMark({ name: 'x', data: { decision: 'escalate' } }));
  assert.ok(!isRoutingMark({ name: 'llm.start', data: {} }));
});
