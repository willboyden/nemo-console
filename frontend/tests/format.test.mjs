import test from 'node:test';
import assert from 'node:assert/strict';
import { fmtBytes, fmtInt, fmtCompact, fmtPct, fmtRatio, fmtMs, fmtDuration, timeAgo, fmtDate, fmtTime, shortSha, debounce, fmtTick } from '../js/format.js';

test('numbers: bytes, ints, compact, percent, ratio; non-numbers become an en dash', () => {
  assert.equal(fmtBytes(0), '0 B'); assert.equal(fmtBytes(48e6), '45.8 MiB'); assert.equal(fmtBytes(NaN), '–');
  assert.equal(fmtInt(1234567.6), '1,234,568'); assert.equal(fmtInt(undefined), '–');
  assert.equal(fmtCompact(999), '999'); assert.equal(fmtCompact(1973335), '2.0M'); assert.equal(fmtCompact(18571), '19k'); assert.equal(fmtCompact(1500), '1.5k');
  assert.equal(fmtPct(37.25), '37.3%'); assert.equal(fmtPct(null), '–'); assert.equal(fmtRatio(0.216), '22%'); assert.equal(fmtRatio(0.216, 1), '21.6%');
});
test('milliseconds and durations', () => {
  assert.equal(fmtMs(4.25), '4.3 ms'); assert.equal(fmtMs(231.4), '231 ms'); assert.equal(fmtMs(1420), '1.42 s'); assert.equal(fmtMs(undefined), '–');
  assert.equal(fmtDuration(45), '45s'); assert.equal(fmtDuration(125), '2m 05s'); assert.equal(fmtDuration(5400), '1h 30m'); assert.equal(fmtDuration(200000), '2d 7h'); assert.equal(fmtDuration(-1), '–');
});
test('time helpers accept seconds or ms epochs and ISO strings', () => {
  const now = Date.parse('2026-09-24T12:00:00Z');
  assert.equal(timeAgo(now / 1000 - 3, now), 'just now'); assert.equal(timeAgo(now / 1000 - 90, now), '1m ago'); assert.equal(timeAgo('2026-09-24T09:00:00Z', now), '3h ago'); assert.equal(timeAgo('bogus', now), '–');
  assert.equal(fmtDate(Date.parse('2026-09-24T12:34:56Z')), '2026-09-24 12:34:56'); assert.equal(fmtTime(Date.parse('2026-09-24T12:34:56Z') / 1000), '12:34:56');
});
test('shortSha and debounce', async () => {
  assert.equal(shortSha('0123456789abcdef'), '01234567'); assert.equal(shortSha(''), '–');
  let n = 0; const d = debounce(() => n++, 10); d(); d(); d(); await new Promise((r) => setTimeout(r, 40)); assert.equal(n, 1);
});

test('fmtTick keeps fractions honest (0.25 is not 0.3)', () => { assert.equal(fmtTick(0.25), '0.25'); assert.equal(fmtTick(1), '1'); assert.equal(fmtTick(0.5), '0.5'); assert.equal(fmtTick(NaN), '–'); });
