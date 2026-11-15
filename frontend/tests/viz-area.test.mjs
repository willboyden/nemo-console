// The geometry of the wave-4 charts (addendum 9.16): the stepped stacked area, the up / down strip, the lifecycle lanes and the decision timeline. Pure arithmetic, no DOM.
import test from 'node:test';
import assert from 'node:assert/strict';
import { stackedAreaLayout } from '../js/viz/area.js';
import { stripLayout, STRIP_HEIGHT_SHARE } from '../js/viz/strip.js';
import { lifecycleLayout } from '../js/viz/lifecycle.js';
import { decisionLayout } from '../js/viz/decisions.js';
import { decisionModel } from '../js/openshell-model.js';

const near = (a, b, eps = 0.11) => assert.ok(Math.abs(a - b) <= eps, `${a} vs ${b}`);

// ---- stacked area
const ts = [0, 60, 120, 180, 240];
const base = { ts, bucketS: 60, width: 400, height: 200 };

test('area: a series is a closed band above the one below it; the axis top is at or above the tallest stack', () => {
  const g = stackedAreaLayout({
    ...base,
    series: [
      { key: 'a', values: [1, 1, 1, 1, 1] },
      { key: 'b', values: [0, 1, 2, 1, 0] },
    ],
  });
  assert.equal(g.areas.length, 2);
  assert.ok(g.areas.every((a) => a.d.startsWith('M') && a.d.endsWith('Z')));
  assert.ok(g.max >= 3);
  assert.deepEqual(g.totals, [1, 2, 3, 2, 1]);
  assert.equal(g.areas[0].any && g.areas[1].any, true);
  // the top of a is the bottom of b: y(1) appears in both paths
  const y1 = Math.round(g.y(1) * 10) / 10;
  assert.ok(g.areas[0].d.includes(` ${y1}`) && g.areas[1].d.includes(` ${y1}`));
  assert.equal(g.bands.length, 5);
  assert.ok(g.bands.every((b) => b.present && b.w > 0));
});

test('area: the steps hold between samples (no slope that was never measured): every vertex of a band is at a bucket edge', () => {
  const g = stackedAreaLayout({ ...base, series: [{ key: 'a', values: [0, 2, 2, 0, 0] }] });
  const xs = new Set(g.bands.flatMap((b) => [b.x, Math.round((b.x + b.w) * 10) / 10]));
  const verts = [...g.areas[0].d.matchAll(/[ML](-?[\d.]+) (-?[\d.]+)/g)].map((m) => Number(m[1]));
  assert.ok(verts.length > 4);
  for (const x of verts)
    assert.ok(
      [...xs].some((e) => Math.abs(e - x) < 0.11),
      `x ${x} is not a bucket edge`,
    );
});

test('area: a bucket nobody sampled is a gap (two subpaths), a null in one series adds nothing to the stack, and a series that is always 0 has nothing to draw', () => {
  const g = stackedAreaLayout({
    ...base,
    series: [
      { key: 'a', values: [1, 1, null, 1, 1] },
      { key: 'b', values: [0, 0, null, 0, 0] },
      { key: 'c', values: [null, 1, null, 1, null] },
    ],
  });
  assert.equal(g.bands[2].present, false);
  assert.equal((g.areas[0].d.match(/M/g) || []).length, 2, 'the area breaks at the gap');
  assert.equal(g.areas[1].any, false, 'all zeros: nothing to draw');
  assert.deepEqual(g.totals, [1, 2, 0, 2, 1]);
  assert.equal(stackedAreaLayout({ ...base, series: [{ key: 'a', values: [null, null, null, null, null] }] }).areas[0].d, '');
});

// ---- strip
test('strip: down is the tallest cell, up the lowest, partial in between, and a no-data cell is a full-height hatched one', () => {
  assert.ok(STRIP_HEIGHT_SHARE.down > STRIP_HEIGHT_SHARE.part && STRIP_HEIGHT_SHARE.part > STRIP_HEIGHT_SHARE.up);
  const cells = ['up', 'part', 'down', 'none', 'up'].map((state, i) => ({ ts: 1000 + i * 60, binS: 60, state }));
  const g = stripLayout({ cells, width: 300, height: 30 });
  assert.equal(g.bars.length, 5);
  const h = Object.fromEntries(g.bars.map((b) => [b.state, b.h]));
  assert.ok(h.down > h.part && h.part > h.up);
  assert.equal(h.none, h.down);
  assert.ok(
    g.bars.every((b) => b.y + b.h === 30),
    'all bars stand on the baseline',
  );
  assert.ok(g.bars.every((b, i) => i === 0 || b.x > g.bars[i - 1].x));
  assert.ok(g.bars.at(-1).x + g.bars.at(-1).w <= g.x1 + 0.5);
  assert.ok(g.height > 30, 'room for the time ticks');
  assert.equal(stripLayout({ cells: [], width: 300 }).bars.length, 0);
});

// ---- lifecycle
test('lifecycle: a bar per segment on one time axis, at least minW wide so an instant phase still shows; marks at their time', () => {
  const lanes = [
    {
      name: 'a',
      segments: [
        { from: 0, to: 100 },
        { from: 100, to: 100.1 },
        { from: 100.1, to: 1000 },
      ],
      marks: [
        { ts: 0, action: 'created' },
        { ts: 1000, action: 'deleted' },
      ],
    },
    { name: 'b', segments: [{ from: 500, to: 1000 }], marks: [] },
  ];
  const g = lifecycleLayout({ lanes, from: 0, to: 1000, width: 600, labelW: 100 });
  assert.equal(g.segs.length, 4);
  assert.equal(g.marks.length, 2);
  assert.equal(g.segs[0].x, 100);
  near(g.segs[0].w, 48.8); // a tenth of the 488 px plot
  assert.ok(g.segs[1].w >= 3, 'an instant phase is still a visible bar');
  near(g.segs[3].x, 100 + 0.5 * (600 - 12 - 100));
  assert.equal(g.marks[1].x, g.x1);
  assert.equal(g.height, 4 + 2 * 30 + 22);
  assert.equal(g.laneY(1) - g.laneY(0), g.laneH);
  assert.ok(g.ticks.length >= 2 && g.ticks.every((t) => t.x >= g.x0 - 0.1 && t.x <= g.x1 + 0.1));
});

// ---- decisions
const D = (i, category, verdict) => ({ ts: 1000 + i, category, verdict, kind: null, target: null });

test('decisions: allowed at the bottom of its column, denied on top of it, every lane on ONE height scale', () => {
  const rows = [D(0, 'network', 'allowed'), D(0, 'network', 'allowed'), D(0, 'network', 'denied'), D(50, 'ssh', 'allowed'), D(99, 'ssh', 'denied')];
  const model = decisionModel({ rows }, { bins: 10 });
  const g = decisionLayout({ model, width: 600 });
  assert.equal(g.height, 4 + model.lanes.length * g.laneH + 22);
  const net = g.cols.filter((c) => c.lane === 0);
  const first = net[0];
  assert.equal(first.a, 2);
  assert.equal(first.d, 1);
  near(first.ha, 2 * first.hd, 0.2);
  assert.equal(first.yd + first.hd, first.ya, 'denied sits right on top of allowed');
  const ssh = g.cols.filter((c) => c.lane === 1);
  const one = ssh.find((c) => c.a === 1 && c.d === 0);
  near(one.ha, first.hd, 0.2); // one decision is the same height in every lane
  assert.ok(g.cols.every((c) => c.x >= g.x0 && c.x + c.w <= g.x1 + 0.5));
  assert.equal(g.cols.length, 3, 'empty slots draw nothing');
});

test('decisions: a slot with only a denied decision is still visible (a minimum height), and the time axis spans the log', () => {
  const rows = [D(0, 'a', 'allowed'), ...Array.from({ length: 200 }, () => D(1, 'a', 'allowed')), D(99, 'a', 'denied')];
  const g = decisionLayout({ model: decisionModel({ rows }, { bins: 20 }), width: 500 });
  const lone = g.cols.find((c) => c.d === 1 && c.a === 0);
  assert.ok(lone.hd >= 3);
  assert.ok(g.ticks.length >= 2);
});
