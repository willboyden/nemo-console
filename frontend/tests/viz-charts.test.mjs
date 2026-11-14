import test from 'node:test';
import assert from 'node:assert/strict';
import { plotBox, timeBands, stackedBars, lineSeries, pathWithGaps, packLanes, contextGrowth } from '../js/viz/series.js';
import { histogramLayout, heatLevel, heatmapLayout, orientLatency, scatterLayout, flowLayout } from '../js/viz/histogram.js';

const TS = Array.from({ length: 6 }, (_, i) => 1700000000 + i * 600);

test('plotBox and timeBands cover the plot exactly', () => {
  const p = plotBox(600, 200);
  assert.equal(p.w, 600 - 50 - 12);
  const tb = timeBands(TS, 600, p);
  assert.equal(tb.bands.length, 6);
  assert.ok(Math.abs(tb.bands[0].x - p.x0) < 1e-9);
  assert.ok(Math.abs(tb.bands[5].x + tb.bands[5].w - p.x1) < 1e-9);
  assert.deepEqual(timeBands([], 60, p).bands, []);
});
test('stacked bars: heights add up, nulls count as zero, the axis tops the tallest stack', () => {
  const r = stackedBars({
    ts: TS,
    bucketS: 600,
    width: 600,
    height: 200,
    series: [
      { key: 'a', values: [1, 2, null, 4, 0, 1] },
      { key: 'b', values: [2, 2, 2, 2, 0, 0] },
    ],
  });
  assert.deepEqual(r.totals, [3, 4, 2, 6, 0, 1]);
  assert.ok(r.max >= 6);
  assert.ok(!r.rects.some((x) => x.i === 4), 'an all-zero bucket draws nothing');
  const bar0 = r.rects.filter((x) => x.i === 0);
  assert.equal(bar0.length, 2);
  assert.ok(Math.abs(bar0[0].y - (bar0[1].y + bar0[1].h)) < 0.3, 'the second segment sits on the first');
  assert.ok(r.rects.every((x) => x.y >= r.plot.y1 - 0.2 && x.y + x.h <= r.plot.y0 + 0.2));
  const empty = stackedBars({ ts: [], bucketS: 60, width: 300, height: 100, series: [] });
  assert.deepEqual(empty.rects, []);
});
test('line series lift the pen at gaps and keep single points', () => {
  assert.equal(pathWithGaps([{ x: 1, y: 2 }, null, { x: 3, y: 4 }, { x: 5, y: 6 }]), 'M1 2M3 4L5 6');
  assert.equal(pathWithGaps([null, null]), '');
  const r = lineSeries({ ts: TS, bucketS: 600, width: 600, height: 200, series: [{ key: 'p50', values: [10, null, null, 30, 40, null] }] });
  assert.equal(r.lines[0].points.length, 3);
  assert.equal((r.lines[0].d.match(/M/g) || []).length, 2);
  assert.ok(r.max >= 40);
});
test('swim lanes: sequential work shares a lane, overlap opens another', () => {
  const r = packLanes([
    { start: 0, end: 5 },
    { start: 2, end: 4 },
    { start: 6, end: 8 },
    { start: 3, end: 9 },
  ]);
  assert.deepEqual(
    r.items.map((x) => x.lane),
    [0, 1, 0, 2],
  );
  assert.equal(r.lanes, 3);
  assert.equal(r.overlapped, 0);
  assert.equal(packLanes([]).lanes, 0);
});
test('swim lanes: a lane cap puts the extra items into the lane that frees up first and counts them', () => {
  const many = Array.from({ length: 50 }, (_, i) => ({ start: i * 0.1, end: 10 + i }));
  const r = packLanes(many, 0, 4);
  assert.equal(r.lanes, 4);
  assert.equal(r.overlapped, 46);
  assert.ok(r.items.every((x) => x.lane >= 0 && x.lane < 4));
  // the 5th item (end 14) goes into lane 0, which ended first (end 10)
  assert.equal(r.items[4].lane, 0);
  // under the cap nothing changes
  assert.deepEqual(
    packLanes(many.slice(0, 3), 0, 4).items.map((x) => x.lane),
    [0, 1, 2],
  );
});
test('context growth: cached sits under uncached, output has its own panel, index mode spreads evenly', () => {
  const calls = [
    { ts: 100, input_tokens: 1000, cached_tokens: 0, output_tokens: 100 },
    { ts: 200, input_tokens: 4000, cached_tokens: 1000, output_tokens: 400 },
    { ts: 5000, input_tokens: 9000, cached_tokens: 9500, output_tokens: 50 },
  ];
  const g = contextGrowth({ calls, width: 600, height: 300 });
  assert.equal(g.bars.length, 3);
  const b1 = g.bars[1];
  assert.ok(Math.abs(b1.uncached.y + b1.uncached.h - b1.cached.y) < 0.3);
  assert.ok(g.bars[2].uncached.h === 0 || Math.abs(g.bars[2].uncached.h) < 0.2, 'cached beyond input is clamped');
  assert.ok(g.bars[2].cx > g.bars[1].cx + 100, 'time mode keeps the idle gap');
  const gi = contextGrowth({ calls, width: 600, height: 300, mode: 'index' });
  assert.ok(Math.abs(gi.bars[1].cx - gi.bars[0].cx - (gi.bars[2].cx - gi.bars[1].cx)) < 0.01);
  assert.equal(gi.xTicks.length, 0);
  assert.ok(g.top.box.y0 < g.bottom.box.y1, 'the two panels do not overlap');
  assert.equal(contextGrowth({ calls: [], width: 300, height: 200 }).bars.length, 0);
});
test('histogram: bars per bin, markers interpolated, an all-zero set is flagged empty', () => {
  const h = histogramLayout({ edges: [0, 10, 100, 1000], counts: [3, 9, 1], width: 400, height: 160, markers: { p50: 55, p95: 900 } });
  assert.equal(h.bars.length, 3);
  assert.equal(h.total, 13);
  assert.ok(h.bars[1].h > h.bars[0].h && h.bars[1].h > h.bars[2].h);
  const p50 = h.markers.find((m) => m.key === 'p50');
  assert.ok(p50.x > h.bars[1].x && p50.x < h.bars[1].x + h.bars[1].w + 1);
  assert.equal(histogramLayout({ edges: [0, 1], counts: [0], width: 300, height: 100 }).empty, true);
  assert.equal(histogramLayout({ edges: [0, 1], counts: [1, 2], width: 300, height: 100 }).bars.length, 0, 'edges must be one longer than counts');
  assert.equal(histogramLayout({}).empty, true);
});
test('heat levels: zero stays zero, any non-zero is visible, the maximum is the top level', () => {
  assert.equal(heatLevel(0, 10), 0);
  assert.equal(heatLevel(1, 1000), 1);
  assert.equal(heatLevel(10, 10), 4);
  assert.equal(heatLevel(null, 10), 0);
  assert.equal(heatLevel(5, 0), 0);
  const h = heatmapLayout({
    counts: [
      [0, 1, 4],
      [2, 0, 0],
    ],
    width: 300,
  });
  assert.equal(h.cells.length, 6);
  assert.equal(h.max, 4);
  assert.equal(h.cells.find((c) => c.r === 0 && c.c === 2).level, 4);
  assert.ok(h.cells.every((c) => c.x >= h.margin.l && c.x + c.w <= 300));
  assert.equal(heatmapLayout({ counts: [], width: 200 }).cells.length, 0);
});
test('latency heatmap orientation: either shape is read as [bucket][time]', () => {
  const ts = [1, 2, 3],
    edges = [0, 10, 100];
  const byTime = [
    [1, 2],
    [3, 4],
    [5, 6],
  ];
  assert.deepEqual(orientLatency({ ts, edges_ms: edges, counts: byTime }), [
    [1, 3, 5],
    [2, 4, 6],
  ]);
  const byBucket = [
    [1, 3, 5],
    [2, 4, 6],
  ];
  assert.deepEqual(orientLatency({ ts, edges_ms: edges, counts: byBucket }), byBucket);
  assert.deepEqual(orientLatency({}), []);
  // pinned time-first: a square matrix is [time][bucket], so it is transposed to [bucket][time]
  assert.deepEqual(
    orientLatency({
      ts: [1, 2],
      edges_ms: [0, 10, 100],
      counts: [
        [1, 2],
        [3, 4],
      ],
    }),
    [
      [1, 3],
      [2, 4],
    ],
  );
});
test('scatter: log axes keep zeros on the floor, empty input is flagged', () => {
  const s = scatterLayout({
    points: [
      { x: 100, y: 0 },
      { x: 10000, y: 500 },
      { x: 'bad', y: 1 },
    ],
    width: 400,
    height: 200,
  });
  assert.equal(s.dots.length, 2);
  assert.ok(s.dots.every((d) => Number.isFinite(d.cx) && Number.isFinite(d.cy)));
  assert.ok(s.dots[0].cy >= s.dots[1].cy, 'a zero is drawn lowest');
  assert.ok(s.dots.every((d) => d.cx >= s.plot.x0 - 0.1 && d.cx <= s.plot.x1 + 0.1));
  assert.equal(scatterLayout({ points: [], width: 100, height: 100 }).empty, true);
  const lin = scatterLayout({ points: [{ x: 1, y: 1 }], width: 300, height: 200, logX: false, logY: false });
  assert.equal(lin.dots.length, 1);
});
test('flow: three layers, link bands start and end on the node edges, totals are conserved', () => {
  const flows = [
    { source: 'route-a', target: 'escalate', value: 6 },
    { source: 'route-a', target: 'stay', value: 3 },
    { source: 'route-b', target: 'stay', value: 5 },
    { source: 'escalate', target: 'big', value: 6 },
    { source: 'stay', target: 'small', value: 8 },
  ];
  const f = flowLayout({ flows });
  assert.equal(f.layers, 3);
  const layer = (n) => f.nodes.find((x) => x.name === n).layer;
  assert.deepEqual([layer('route-a'), layer('escalate'), layer('big')], [0, 1, 2]);
  assert.equal(f.links.length, 5);
  assert.ok(f.links.every((l) => /^M[\d. -]+C.*Z$/.test(l.d)));
  const stay = f.nodes.find((x) => x.name === 'stay');
  assert.equal(stay.value, 8);
  assert.ok(f.nodes.every((n) => n.y >= 0 && n.y + n.h <= f.height + 0.5));
  assert.deepEqual(flowLayout({ flows: [] }).nodes, []);
  assert.deepEqual(
    flowLayout({
      flows: [
        { source: 'a', target: 'a', value: 1 },
        { source: 'a', target: 'b', value: 0 },
      ],
    }).nodes,
    [],
    'self loops and zero flows are ignored',
  );
  const cyc = flowLayout({
    flows: [
      { source: 'a', target: 'b', value: 1 },
      { source: 'b', target: 'a', value: 1 },
    ],
  });
  assert.equal(cyc.nodes.length, 2, 'a pure cycle still lays out');
});
