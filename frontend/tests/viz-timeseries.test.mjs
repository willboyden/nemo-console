// Time-series and trend geometry (js/viz/timeseries.js): segments, gaps, bands, axes, the trend across runs.
import test from 'node:test';
import assert from 'node:assert/strict';
import { timeSeriesLayout, trendLayout, segmentPath, bandPath, thinIndices, relativeTicks } from '../js/viz/timeseries.js';

const line = (key, ...segs) => ({ key, segments: segs });

test('a line with a gap is two paths; a one-point segment is a dot with no path', () => {
  const g = timeSeriesLayout({
    lines: [
      line(
        'a',
        [
          [0, 1, 1],
          [10, 2, 2],
        ],
        [[100, 3, 3]],
      ),
    ],
    from: 0,
    to: 110,
    width: 400,
    height: 200,
  });
  const segs = g.lines[0].segments;
  assert.equal(segs.length, 2);
  assert.match(segs[0].d, /^M[\d.]+ [\d.]+L[\d.]+ [\d.]+$/);
  assert.equal(segs[1].d, '');
  assert.equal(segs[1].dot, true);
  assert.equal(g.empty, false);
});

test('x grows with time and y shrinks with value (screen coordinates); the axis covers every value', () => {
  const g = timeSeriesLayout({
    lines: [
      line('a', [
        [0, 1, 1],
        [50, 10, 10],
        [100, 4, 4],
      ]),
    ],
    from: 0,
    to: 100,
    width: 500,
    height: 200,
  });
  const [p0, p1, p2] = g.lines[0].segments[0].pts;
  assert.ok(p0.x < p1.x && p1.x < p2.x);
  assert.ok(p1.y < p2.y && p2.y < p0.y);
  assert.ok(g.max >= 10, 'the top of the axis is at or above the largest value');
  assert.equal(g.min, 0, 'a non-negative series starts at zero');
  assert.ok(p0.x >= g.plot.x0 && p2.x <= g.plot.x1 && p1.y >= g.plot.y1 && p0.y <= g.plot.y0);
});

test('show max: the band and the max line exist only where a bucket has a spread, and the axis grows to hold the max', () => {
  const flat = [
    [0, 5, 5],
    [10, 6, 6],
  ];
  const spread = [
    [0, 5, 9],
    [10, 6, 12],
  ];
  const off = timeSeriesLayout({ lines: [line('a', spread)], from: 0, to: 10, width: 400, height: 200, showMax: false });
  const on = timeSeriesLayout({ lines: [line('a', spread)], from: 0, to: 10, width: 400, height: 200, showMax: true });
  assert.equal(off.lines[0].segments[0].bandD, '');
  assert.match(on.lines[0].segments[0].bandD, /Z$/);
  assert.ok(on.lines[0].segments[0].maxD.startsWith('M'));
  assert.ok(on.max >= 12 && off.max < on.max, 'the axis includes the max only when it is drawn');
  const none = timeSeriesLayout({ lines: [line('a', flat)], from: 0, to: 10, width: 400, height: 200, showMax: true });
  assert.equal(none.lines[0].segments[0].bandD, '');
  assert.equal(none.lines[0].segments[0].spread, false);
});

test('non-finite points are dropped (a null is a gap, never zero); an empty input is flagged', () => {
  const g = timeSeriesLayout({
    lines: [
      line('a', [
        [0, NaN, 1],
        [10, 2, 2],
        [20, null, 3],
        [30, 3, 3],
      ]),
    ],
    from: 0,
    to: 30,
    width: 300,
    height: 150,
  });
  assert.equal(g.lines[0].segments[0].pts.length, 2);
  const e = timeSeriesLayout({ lines: [line('a', [])], width: 300, height: 150 });
  assert.equal(e.empty, true);
  assert.equal(e.lines[0].empty, true);
  assert.equal(timeSeriesLayout({ lines: [], width: 10, height: 10 }).empty, true);
});

test('a single instant still gets a non-degenerate time axis', () => {
  const g = timeSeriesLayout({ lines: [line('a', [[5, 1, 1]])], width: 300, height: 150 });
  assert.ok(g.xDomain[1] > g.xDomain[0]);
  assert.equal(g.lines[0].segments[0].dot, true);
});

test('hover targets are thinned to the cap and always include the first and last point', () => {
  const pts = Array.from({ length: 1000 }, (_, i) => [i, i, i]);
  const g = timeSeriesLayout({ lines: [line('a', pts)], from: 0, to: 999, width: 600, height: 200, maxHits: 40 });
  assert.ok(g.lines[0].hits.length <= 40);
  assert.equal(g.lines[0].hits[0].ts, 0);
  assert.equal(g.lines[0].hits.at(-1).ts, 999);
  assert.deepEqual(thinIndices(5, 10), [0, 1, 2, 3, 4]);
  assert.deepEqual(thinIndices(0, 5), []);
});

test('path builders', () => {
  assert.equal(
    segmentPath([
      { x: 1.04, y: 2 },
      { x: 3, y: 4.26 },
    ]),
    'M1 2L3 4.3',
  );
  assert.equal(bandPath([{ x: 0, y: 0 }], [{ x: 0, y: 1 }]), '', 'a band needs two points');
  assert.equal(
    bandPath(
      [
        { x: 0, y: 0 },
        { x: 10, y: 0 },
      ],
      [
        { x: 0, y: 5 },
        { x: 10, y: 5 },
      ],
    ),
    'M0 0L10 0L10 5L0 5Z',
  );
});

test('relative ticks count back from the right edge, newest last, with a 0 offset for now', () => {
  const tk = relativeTicks(0, 900, 5);
  assert.equal(tk.at(-1).offsetS, 0);
  assert.equal(tk.at(-1).v, 900);
  assert.ok(tk.length <= 5 && tk.length >= 3);
  assert.ok(tk.every((x, i) => i === 0 || x.v > tk[i - 1].v));
  assert.deepEqual(relativeTicks(10, 10), [{ v: 10, offsetS: 0 }]);
});

test('trend layout: run order on x, missing numbers are not points, ticks include both ends', () => {
  const groups = [
    {
      key: 'g1',
      points: [
        { i: 0, v: 10, id: 'a' },
        { i: 2, v: 20, id: 'c' },
        { i: 5, v: null, id: 'f' },
      ],
    },
    { key: 'g2', points: [{ i: 1, v: 15, id: 'b' }] },
  ];
  const g = trendLayout({ groups, n: 6, width: 500, height: 200 });
  assert.equal(g.empty, false);
  assert.deepEqual(
    g.groups[0].pts.map((p) => p.id),
    ['a', 'c'],
    'a null value is not a point',
  );
  assert.ok(g.groups[0].d.startsWith('M'));
  assert.equal(g.groups[1].d, '', 'one point: a dot, no path');
  assert.equal(g.xTicks[0].i, 0);
  assert.equal(g.xTicks.at(-1).i, 5);
  const xs = g.groups[0].pts.map((p) => p.x);
  assert.ok(xs[0] < xs[1]);
  assert.equal(g.yTicks[0].v, 0, 'values far below the maximum start the axis at zero');
});

test('trend layout: values that sit close together do not start at zero; nothing to draw is flagged', () => {
  const g = trendLayout({
    groups: [
      {
        key: 'g',
        points: [
          { i: 0, v: 90, id: 'a' },
          { i: 1, v: 99, id: 'b' },
        ],
      },
    ],
    n: 2,
    width: 400,
    height: 180,
  });
  assert.ok(g.yTicks[0].v > 0 && g.yTicks[0].v <= 90);
  assert.equal(trendLayout({ groups: [{ key: 'g', points: [] }], n: 3, width: 400, height: 180 }).empty, true);
  assert.equal(trendLayout({ groups: [], n: 0, width: 400, height: 180 }).empty, true);
  const one = trendLayout({ groups: [{ key: 'g', points: [{ i: 0, v: 3, id: 'a' }] }], n: 1, width: 400, height: 180 });
  assert.equal(one.groups[0].pts.length, 1);
});

test('trend x ticks keep both ends and drop interior ticks that would touch a neighbour', async () => {
  const { spaceTicks } = await import('../js/viz/timeseries.js');
  const tk = (...xs) => xs.map((x, i) => ({ i, x }));
  assert.deepEqual(
    spaceTicks(tk(0, 30, 100, 160, 200), 46).map((t) => t.x),
    [0, 100, 200],
  );
  assert.deepEqual(
    spaceTicks(tk(0, 100), 46).map((t) => t.x),
    [0, 100],
  );
  assert.deepEqual(
    spaceTicks(tk(0, 20), 46).map((t) => t.x),
    [0, 20],
    'two ticks are kept even when close',
  );
  // clustered runs: indexes 0 and 1 are close on screen
  const g = trendLayout({ groups: [{ key: 'g', points: Array.from({ length: 30 }, (_, i) => ({ i, v: i + 1, id: `r${i}` })) }], n: 30, width: 300, height: 180, xTickCount: 6 });
  const xs = g.xTicks.map((t) => t.x);
  assert.ok(
    xs.every((x, i) => i === 0 || x - xs[i - 1] >= 40),
    `ticks ${xs}`,
  );
  assert.equal(g.xTicks[0].i, 0);
  assert.equal(g.xTicks.at(-1).i, 29);
});
