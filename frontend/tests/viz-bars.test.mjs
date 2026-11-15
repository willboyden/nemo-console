// Geometry of the wave-3 charts (addendum 9.15): quantiles and bins, horizontal bars with whiskers, dumbbells, grouped columns, swim lanes, percentile
// curves and the scatter. Pure functions: nothing here touches the DOM. A value that is missing is "no value", never a zero.
import test from 'node:test';
import assert from 'node:assert/strict';
import { quantile, sortedNums, median, mean, histogramBins } from '../js/viz/stats.js';
import { hbarLayout, dumbbellLayout, columnLayout, swimLayout, curveLayout, pointsLayout, fitTicks } from '../js/viz/bars.js';

const close = (a, b, eps = 1e-9) => assert.ok(Math.abs(a - b) < eps, `${a} vs ${b}`);

test('quantile: linear interpolation between ranks; empty is null; one value is itself; q is clamped', () => {
  assert.equal(quantile([], 0.5), null);
  assert.equal(quantile([7], 0.9), 7);
  assert.equal(quantile([1, 2, 3, 4, 5], 0.5), 3);
  close(quantile([1, 2, 3, 4], 0.5), 2.5);
  close(quantile([10, 20, 30, 40, 50], 0.95), 48);
  assert.equal(quantile([1, 2, 3], -1), 1);
  assert.equal(quantile([1, 2, 3], 2), 3);
});

test('sortedNums, median and mean ignore everything that is not a finite number', () => {
  assert.deepEqual(sortedNums([3, null, NaN, '4', 1, undefined, Infinity, 2]), [1, 2, 3]);
  assert.equal(median([5, null, 1, 3]), 3);
  assert.equal(median([]), null);
  assert.equal(median(null), null);
  assert.equal(mean([1, 2, 3, 'x']), 2);
  assert.equal(mean([]), null);
});

test('histogramBins: every value lands in exactly one bin; long tails get geometric edges; equal values get a padded range; no data is null', () => {
  assert.equal(histogramBins([]), null);
  assert.equal(histogramBins([null, NaN]), null);
  const lin = histogramBins([3, 4, 5, 6, 7, 8, 9, 10, 11, 12], { bins: 5 });
  assert.equal(lin.geometric, false);
  assert.equal(
    lin.counts.reduce((a, b) => a + b, 0),
    10,
  );
  assert.equal(lin.edges.length, 6);
  assert.equal(lin.edges[0], 3);
  assert.equal(lin.edges[5], 12);
  assert.deepEqual(lin.counts, [2, 2, 2, 2, 2]);
  const vals = [5, 6, 7, 8, 9, 10, 15, 30, 80, 400];
  const geo = histogramBins(vals, { bins: 10 });
  assert.equal(geo.geometric, true);
  assert.equal(
    geo.counts.reduce((a, b) => a + b, 0),
    vals.length,
    'nothing is lost to a rounding edge',
  );
  assert.ok(
    geo.edges.every((e, i) => i === 0 || e > geo.edges[i - 1]),
    'edges rise',
  );
  const same = histogramBins([4, 4, 4], { bins: 4 });
  assert.equal(
    same.counts.reduce((a, b) => a + b, 0),
    3,
  );
  assert.ok(same.edges[0] < 4 && same.edges[4] > 4);
});

test('hbarLayout: bars share one scale, a longer value is a longer bar, whiskers sit at their interval, a missing value has no bar', () => {
  const g = hbarLayout({
    width: 600,
    labelW: 100,
    tailW: 60,
    domain: { min: 0, max: 1 },
    rows: [
      {
        id: 'a',
        label: 'A',
        bars: [
          { key: 'x', value: 0.5, lo: 0.4, hi: 0.6, cls: 'bf1', text: 't' },
          { key: 'y', value: 0.25, cls: 'bf2', text: 't' },
        ],
      },
      { id: 'b', label: 'B', bars: [{ key: 'x', value: null, cls: 'bf1', text: 'none' }] },
    ],
  });
  assert.equal(g.empty, false);
  assert.equal(g.x0, 100);
  assert.equal(g.x1, 540);
  const [r1, r2] = g.rows;
  close(r1.bars[0].w, 220, 0.2);
  close(r1.bars[1].w, 110, 0.2);
  assert.ok(r1.bars[0].w > r1.bars[1].w);
  close(r1.bars[0].whisker.x1, 100 + 0.4 * 440, 0.2);
  close(r1.bars[0].whisker.x2, 100 + 0.6 * 440, 0.2);
  assert.equal(r1.bars[1].whisker, null);
  assert.equal(r2.bars[0].missing, true);
  assert.equal(r2.bars[0].w, 0);
  assert.equal(r2.bars[0].value, null);
  assert.ok(r2.y > r1.y + r1.h, 'rows do not overlap');
  assert.ok(g.height > r2.y + r2.h, 'the axis fits below the last row');
  assert.ok(g.ticks.length >= 2 && g.ticks[0].v === 0);
});

test('hbarLayout: the scale is a round number above the data (the whisker counts); a zero value is no bar width; no value at all is empty', () => {
  const g = hbarLayout({ width: 500, rows: [{ id: 'a', label: 'a', bars: [{ key: 'k', value: 0.7, lo: 0.6, hi: 0.93, cls: 'bf1' }] }] });
  assert.ok(g.domain.max >= 0.93);
  const z = hbarLayout({
    width: 500,
    rows: [
      {
        id: 'a',
        label: 'a',
        bars: [
          { key: 'k', value: 0, cls: 'bf1' },
          { key: 'j', value: 3, cls: 'bf1' },
        ],
      },
    ],
  });
  assert.equal(z.rows[0].bars[0].w, 0, 'a zero is a bar of no length, not a stub');
  assert.equal(z.rows[0].bars[0].missing, false);
  assert.equal(hbarLayout({ width: 500, rows: [{ id: 'a', label: 'a', bars: [{ key: 'k', value: null }] }] }).empty, true);
  assert.equal(hbarLayout({ width: 500, rows: [] }).empty, true);
});

test('hbarLayout: a value past an explicit domain is clamped to its edge (a bar never runs off the chart)', () => {
  const g = hbarLayout({ width: 600, labelW: 100, tailW: 60, domain: { min: 0, max: 1 }, rows: [{ id: 'a', label: 'a', bars: [{ key: 'k', value: 4, cls: 'bf1' }] }] });
  assert.ok(g.rows[0].bars[0].x + g.rows[0].bars[0].w <= g.x1 + 0.1);
});

test('dumbbellLayout: with minus without is the delta and its direction; a header row has no values; a half-missing row has no line', () => {
  const g = dumbbellLayout({
    width: 600,
    rows: [
      { id: 'h', label: 'agent', header: true },
      { id: 'o', label: 'overall', a: 0.5, b: 0.7 },
      { id: 'w', label: 'worse', a: 0.8, b: 0.6 },
      { id: 'f', label: 'flat', a: 0.4, b: 0.4 },
      { id: 'n', label: 'half', a: null, b: 0.9 },
    ],
  });
  const by = Object.fromEntries(g.rows.map((r) => [r.id, r]));
  assert.equal(by.h.header, true);
  assert.equal(by.h.a, null);
  close(by.o.delta, 0.2);
  assert.equal(by.o.dir, 'up');
  assert.ok(by.o.b > by.o.a);
  assert.equal(by.w.dir, 'down');
  assert.ok(by.w.b < by.w.a);
  assert.equal(by.f.dir, 'flat');
  assert.equal(by.n.delta, null);
  assert.equal(by.n.dir, null);
  assert.equal(by.n.a, null);
  assert.ok(by.n.b != null);
  assert.ok(g.rows.every((r, i) => i === 0 || r.y > g.rows[i - 1].y));
  assert.equal(dumbbellLayout({ width: 600, rows: [{ id: 'h', label: 'x', header: true }] }).empty, true);
});

test('columnLayout: grouped columns, heights follow counts, a missing count is a zero-height column, an all-zero chart is empty', () => {
  const g = columnLayout({
    cats: ['a', 'b', 'c'],
    width: 400,
    height: 200,
    series: [
      { key: 'x', values: [1, 0, 4] },
      { key: 'y', values: [2, null, 0] },
    ],
  });
  assert.equal(g.empty, false);
  assert.equal(g.groups.length, 3);
  assert.equal(g.groups[0].cols.length, 2);
  assert.ok(g.groups[2].cols[0].h > g.groups[0].cols[0].h);
  assert.equal(g.groups[1].cols[1].h, 0);
  assert.ok(g.groups[0].cols[1].x > g.groups[0].cols[0].x, 'B sits right of A');
  assert.ok(
    g.yTicks.every((t) => Number.isInteger(t.v)),
    'counts have whole-number ticks',
  );
  assert.equal(columnLayout({ cats: ['a'], width: 400, height: 200, series: [{ key: 'x', values: [0] }] }).empty, true);
});

test('swimLayout: a lane per worker, overlapping requests go to sub-rows (at most maxSub), bars sit at their times, junk rows are dropped', () => {
  const lanes = [
    { id: 'w0', label: '#0' },
    { id: 'w1', label: '#1' },
  ];
  const items = [
    { i: 0, lane: 0, start: 0, end: 100, error: false },
    { i: 1, lane: 0, start: 50, end: 150, error: false },
    { i: 2, lane: 0, start: 60, end: 160, error: true },
    { i: 3, lane: 0, start: 70, end: 170, error: false },
    { i: 4, lane: 1, start: 0, end: 1000, error: false },
    { i: 5, lane: 9, start: 0, end: 10 },
    { i: 6, lane: 1, start: 'x', end: 10 },
  ];
  const g = swimLayout({ lanes, items, width: 500, maxSub: 2 });
  assert.equal(g.empty, false);
  assert.equal(g.bars.length, 5, 'the row with an unknown lane and the one with no start are dropped');
  assert.equal(g.lanes[0].subRows, 2);
  assert.equal(g.lanes[1].subRows, 1);
  assert.ok(g.overlapped >= 1, 'past maxSub, requests overlap and are counted');
  assert.equal(g.domainMs, 1000);
  const b4 = g.bars.find((b) => b.i === 4);
  assert.ok(Math.abs(b4.x - g.x0) < 0.2 && Math.abs(b4.x + b4.w - g.x1) < 0.2, 'a request over the whole run spans the plot');
  assert.ok(
    g.bars.every((b) => b.w >= 1.5),
    'a short request stays visible',
  );
  assert.equal(g.bars.find((b) => b.i === 2).error, true);
  assert.ok(g.lanes[1].y > g.lanes[0].y);
  assert.equal(swimLayout({ lanes, items: [], width: 500 }).empty, true);
});

test('curveLayout: points at their categories, a null lifts the pen (never a drop to zero), the axis starts at 0 and covers the data', () => {
  const g = curveLayout({
    cats: ['p1', 'p50', 'p99'],
    width: 300,
    height: 180,
    lines: [
      { key: 'a', values: [10, 20, 40] },
      { key: 'b', values: [5, null, 30] },
    ],
  });
  assert.equal(g.empty, false);
  assert.equal(g.xs.length, 3);
  assert.ok(g.xs[0] < g.xs[1] && g.xs[1] < g.xs[2]);
  assert.equal(g.lines[0].pts.length, 3);
  assert.equal(g.lines[1].pts.length, 2, 'the missing percentile has no point');
  assert.equal((g.lines[1].d.match(/M/g) || []).length, 2, 'the pen is lifted across the gap');
  assert.ok(g.lines[0].pts[2].y < g.lines[0].pts[0].y, 'a larger value is higher');
  assert.ok(g.yTicks[0].v === 0 && g.yTicks.at(-1).v >= 40);
  assert.equal(curveLayout({ cats: ['p1'], width: 300, height: 180, lines: [{ key: 'a', values: [null] }] }).empty, true);
  assert.equal(curveLayout({ cats: [], width: 300, height: 180, lines: [] }).empty, true);
});

test('pointsLayout: points with a missing coordinate are skipped and counted; a log axis drops non-positive x; the group index is kept', () => {
  const pts = [
    { id: 'a', x: 100, y: 5, g: 2 },
    { id: 'b', x: 1000, y: 9 },
    { id: 'c', x: null, y: 3 },
    { id: 'd', x: 50, y: undefined },
    { id: 'e', x: 0, y: 4 },
  ];
  const lin = pointsLayout({ points: pts, width: 400, height: 240 });
  assert.equal(lin.skipped, 2);
  assert.equal(lin.dots.length, 3);
  const log = pointsLayout({ points: pts, width: 400, height: 240, logX: true });
  assert.equal(log.skipped, 3, 'x = 0 cannot be placed on a log axis');
  assert.deepEqual(
    log.dots.map((d) => d.id),
    ['a', 'b'],
  );
  assert.equal(log.dots[0].g, 2);
  assert.equal(log.dots[1].g, 0);
  assert.ok(log.dots[1].cx > log.dots[0].cx);
  assert.ok(log.dots[1].cy < log.dots[0].cy, 'a higher y is higher on screen');
  assert.ok(log.xTicks.every((t) => t.v > 0));
  assert.equal(pointsLayout({ points: [{ id: 'z', x: null, y: null }], width: 400, height: 240 }).empty, true);
  const fixed = pointsLayout({
    points: [
      { id: 'a', x: 1, y: 0.4 },
      { id: 'b', x: 2, y: 0.9 },
    ],
    width: 400,
    height: 240,
    yDomain: { min: 0, max: 1 },
  });
  assert.equal(fixed.yTicks[0].v, 0);
  assert.equal(fixed.yTicks.at(-1).v, 1);
});

test('fitTicks: a label that would touch the one before it is dropped; the ends are anchored inward; spaced ticks all stay', () => {
  const gib = (v) => `${v} GiB`;
  const crowded = [
    { v: 0, x: 100 },
    { v: 47, x: 160 },
    { v: 93, x: 220 },
    { v: 140, x: 280 },
    { v: 186, x: 340 },
  ];
  const kept = fitTicks(crowded, gib, { x0: 100, x1: 340 });
  assert.ok(kept.length < crowded.length, 'some are dropped');
  assert.equal(kept[0].v, 0);
  assert.equal(kept[0].anchor, 'start');
  for (let i = 1; i < kept.length; i++) assert.ok(kept[i].x - kept[i - 1].x >= 50, 'what is kept is spread out');
  const roomy = fitTicks(
    [
      { v: 0, x: 100 },
      { v: 0.5, x: 300 },
      { v: 1, x: 500 },
    ],
    (v) => String(v),
    { x0: 100, x1: 500 },
  );
  assert.deepEqual(
    roomy.map((t) => t.anchor),
    ['start', 'middle', 'end'],
  );
  assert.deepEqual(fitTicks([], gib, { x0: 0, x1: 10 }), []);
});

test('hbarLayout: whole-number values get whole-number ticks only (no 0 1 1 2 2)', () => {
  const L = hbarLayout({
    rows: [
      { id: 'a', label: 'a', bars: [{ key: 'x', value: 2 }] },
      { id: 'b', label: 'b', bars: [{ key: 'x', value: 1 }] },
    ],
    width: 400,
  });
  assert.ok(L.ticks.length >= 2);
  assert.ok(L.ticks.every((tk) => Number.isInteger(tk.v)));
  const F = hbarLayout({ rows: [{ id: 'a', label: 'a', bars: [{ key: 'x', value: 1.5 }] }], width: 400 });
  assert.ok(
    F.ticks.some((tk) => !Number.isInteger(tk.v)),
    'fractional data keeps fractional ticks',
  );
});
