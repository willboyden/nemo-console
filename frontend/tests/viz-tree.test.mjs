import test from 'node:test';
import assert from 'node:assert/strict';
import { buildTree, timeline, relMs, visibleRows, parentIds, windowRows, scrollToRow, barGeometry, marksByParent, icicleLayout, spanStats, sizeMs } from '../js/viz/tree.js';

const sp = (id, parent, start, end, extra = {}) => ({
  id,
  parent_id: parent,
  name: id,
  category: 'custom',
  start_ts: start,
  end_ts: end,
  duration_ms: end == null ? null : (end - start) * 1000,
  status: end == null ? 'open' : 'ok',
  ...extra,
});
const SPANS = [
  sp('root', null, 100, 110, { category: 'agent' }),
  sp('b', 'root', 104, 107),
  sp('a', 'root', 101, 103),
  sp('a1', 'a', 101.5, 102.5),
  sp('c', 'root', 108, null),
  sp('x', 'ghost', 105, 106),
];

test('buildTree orders children by start, keeps orphans as roots, assigns depth', () => {
  const t = buildTree(SPANS);
  assert.deepEqual(
    t.order.map((n) => n.id),
    ['root', 'a', 'a1', 'b', 'c', 'x'],
  );
  assert.deepEqual(
    t.order.map((n) => n.depth),
    [0, 1, 2, 1, 1, 0],
  );
  assert.deepEqual(
    t.roots.map((n) => n.id),
    ['root', 'x'],
  );
});
test('buildTree cannot lose or loop on a cycle, a self parent or a very deep chain', () => {
  const cyc = buildTree([sp('p', 'q', 1, 2), sp('q', 'p', 1, 2), sp('s', 's', 1, 2)]);
  assert.equal(cyc.order.length, 3);
  assert.equal(new Set(cyc.order.map((n) => n.id)).size, 3);
  const chain = Array.from({ length: 6000 }, (_, i) => sp(`n${i}`, i ? `n${i - 1}` : null, i, i + 1));
  const deep = buildTree(chain);
  assert.equal(deep.order.length, 6000);
  assert.equal(deep.order.at(-1).depth, 5999);
  assert.equal(buildTree([]).order.length, 0);
  assert.equal(buildTree(null).roots.length, 0);
});
test('timeline spans every timestamp and never inverts', () => {
  const tl = timeline({ start_ts: 100, end_ts: null }, SPANS, [{ ts: 111 }]);
  assert.equal(tl.t0, 100);
  assert.equal(tl.t1, 111);
  assert.equal(tl.durMs, 11000);
  assert.equal(relMs(105, tl), 5000);
  assert.equal(relMs(null, tl), null);
  assert.equal(timeline(null, [], []).empty, true);
  assert.ok(timeline({ start_ts: 5, end_ts: 5 }, [], []).durMs > 0);
});
test('collapsing hides exactly the descendants; parentIds lists spans with children', () => {
  const t = buildTree(SPANS);
  assert.deepEqual(
    visibleRows(t, new Set(['a'])).map((n) => n.id),
    ['root', 'a', 'b', 'c', 'x'],
  );
  assert.deepEqual(
    visibleRows(t, new Set(['root'])).map((n) => n.id),
    ['root', 'x'],
  );
  assert.equal(visibleRows(t, new Set()).length, 6);
  assert.deepEqual(parentIds(t).sort(), ['a', 'root']);
});
test('virtual window: rows around the viewport, with padding that keeps the scroll height', () => {
  const w = windowRows(1000, 2800, 560, 28, 5);
  assert.equal(w.start, 95);
  assert.equal(w.end, 125);
  assert.equal(w.padTop, 95 * 28);
  assert.equal(w.padBottom, (1000 - 125) * 28);
  assert.equal(w.padTop + (w.end - w.start) * 28 + w.padBottom, 1000 * 28);
  assert.deepEqual(windowRows(3, 0, 560, 28, 8), { start: 0, end: 3, padTop: 0, padBottom: 0 });
  assert.equal(scrollToRow(5, 0, 280, 28), null);
  assert.equal(scrollToRow(40, 0, 280, 28), 40 * 28 + 28 - 280);
  assert.equal(scrollToRow(1, 280, 280, 28), 28);
});
test('barGeometry: closed, open (stub + tail), no-start and zoomed spans', () => {
  const tl = timeline({ start_ts: 100, end_ts: 110 }, SPANS, []);
  const full = { from: 0, to: tl.durMs };
  const g = barGeometry(SPANS[1], tl, full);
  assert.ok(Math.abs(g.l - 0.4) < 1e-9 && Math.abs(g.w - 0.3) < 1e-9);
  assert.equal(g.tailW, 0);
  const open = barGeometry(SPANS[4], tl, full);
  assert.equal(open.open, true);
  assert.ok(Math.abs(open.tailL + open.tailW - 1) < 1e-9, 'the tail reaches the trace end');
  const nostart = barGeometry(sp('n', null, null, 105), tl, full);
  assert.equal(nostart.noStart, true);
  assert.equal(nostart.w, 0);
  const zoom = barGeometry(SPANS[1], tl, { from: 5000, to: 6000 });
  assert.equal(zoom.hidden, false);
  assert.equal(zoom.clipL, true);
  assert.equal(zoom.clipR, true);
  assert.equal(zoom.l, 0);
  assert.equal(zoom.w, 1);
  assert.equal(barGeometry(SPANS[2], tl, { from: 5000, to: 6000 }).hidden, true);
  assert.equal(barGeometry(SPANS[1], tl, { from: 5, to: 5 }).hidden, true, 'an empty range hides everything');
});
test('marks hang under their span, or under the first root when the parent is unknown', () => {
  const t = buildTree(SPANS);
  const m = marksByParent(
    [
      { id: 1, parent_id: 'a' },
      { id: 2, parent_id: 'nope' },
      { id: 3, parent_id: null },
    ],
    t,
  );
  assert.equal(m.get('a').length, 1);
  assert.equal(m.get('root').length, 2);
});
test('icicle: width follows duration, children fit their parent, tiny nodes are dropped, focus re-roots', () => {
  const tl = timeline({ start_ts: 100, end_ts: 110 }, SPANS, []);
  const t = buildTree([SPANS[0], SPANS[1], SPANS[2], SPANS[3]]);
  const lay = icicleLayout(t, tl, null);
  const by = Object.fromEntries(lay.rects.map((r) => [r.id, r]));
  assert.deepEqual([by.root.x0, by.root.x1, by.root.depth], [0, 1, 0]);
  assert.ok(Math.abs(by.b.x1 - by.b.x0 - 0.3) < 1e-9, 'b lasts 3 of the root 10 s');
  assert.ok(Math.abs(by.a.x1 - by.a.x0 - 0.2) < 1e-9);
  assert.ok(by.a1.x0 >= by.a.x0 && by.a1.x1 <= by.a.x1);
  assert.equal(lay.parallel, false);
  const focus = icicleLayout(t, tl, 'a');
  assert.equal(focus.rects[0].id, 'a');
  assert.equal(focus.rects[0].x1, 1);
  assert.deepEqual(
    focus.path.map((n) => n.id),
    ['root', 'a'],
  );
  const tiny = icicleLayout(t, tl, null, { minFrac: 0.25 });
  assert.ok(!tiny.rects.some((r) => r.id === 'a'), 'a is 20% wide, below the cut');
  // parallel children are normalised to fit, and flagged
  const par = buildTree([sp('r', null, 0, 10), sp('p1', 'r', 0, 8), sp('p2', 'r', 1, 9)]);
  const pl = icicleLayout(par, timeline(null, [sp('r', null, 0, 10)], []), null);
  assert.equal(pl.parallel, true);
  assert.ok(pl.rects.every((r) => r.x1 <= 1 + 1e-9));
});
test('icicle over several roots uses a virtual root and still covers the width', () => {
  const t = buildTree([sp('r1', null, 0, 4), sp('r2', null, 5, 6)]);
  const lay = icicleLayout(t, timeline(null, [sp('r1', null, 0, 4), sp('r2', null, 5, 6)], []), null);
  assert.equal(lay.rects.length, 2);
  assert.ok(Math.abs(lay.rects[0].x1 - 0.8) < 1e-9);
  assert.equal(icicleLayout(buildTree([]), { t0: 0, t1: 1, durMs: 1000 }, null).rects.length, 0);
});
test('spanStats and sizeMs', () => {
  const st = spanStats([{ category: 'llm', status: 'ok' }, { category: 'tool', status: 'error' }, { category: 'agent', status: 'open' }, { category: 'custom' }, { category: 'zzz' }]);
  assert.deepEqual(st, { total: 5, llm: 1, tool: 1, agent: 1, turn: 1, other: 1, errors: 1, open: 1 });
  assert.equal(sizeMs({ duration_ms: 12 }, { t1: 0 }), 12);
  assert.equal(sizeMs({ start_ts: 5, end_ts: null, duration_ms: null }, { t1: 8 }), 3000);
  assert.equal(sizeMs({}, { t1: 8 }), 0);
});
