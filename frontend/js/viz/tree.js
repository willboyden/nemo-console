// Span tree geometry for the waterfall, icicle and span table. Pure: spans in, rows and fractions out. Iterative, so a
// 5000-deep chain cannot overflow the stack; cycles and orphans cannot lose or loop a span.
import { isNum, clamp } from './scale.js';

const startOf = (s) => (isNum(s?.start_ts) ? s.start_ts : isNum(s?.end_ts) ? s.end_ts : Infinity);

/** Build the tree. node = {id, span, parent, children, depth, index}; `order` is pre-order (what the waterfall lists). */
export function buildTree(spans) {
  const nodes = new Map();
  (spans || []).forEach((span, i) => nodes.set(span.id, { id: span.id, span, parent: null, children: [], depth: 0, index: i }));
  const roots = [];
  for (const n of nodes.values()) {
    const p = n.span.parent_id != null ? nodes.get(n.span.parent_id) : null;
    if (p && p !== n) {
      n.parent = p;
      p.children.push(n);
    } else roots.push(n);
  }
  const cmp = (a, b) => startOf(a.span) - startOf(b.span) || a.index - b.index;
  for (const n of nodes.values()) if (n.children.length > 1) n.children.sort(cmp);
  roots.sort(cmp);
  const order = [];
  const seen = new Set();
  const walk = (root) => {
    const stack = [root];
    root.depth = 0;
    while (stack.length) {
      const n = stack.pop();
      if (seen.has(n.id)) continue;
      seen.add(n.id);
      order.push(n);
      for (let i = n.children.length - 1; i >= 0; i--) {
        n.children[i].depth = n.depth + 1;
        stack.push(n.children[i]);
      }
    }
  };
  for (const r of roots) walk(r);
  // Anything unreached sits in a cycle (no root above it): cut the cycle at its first span and show it as a root.
  for (const n of nodes.values()) {
    if (seen.has(n.id)) continue;
    if (n.parent) n.parent.children = n.parent.children.filter((c) => c !== n);
    n.parent = null;
    roots.push(n);
    walk(n);
  }
  return { nodes, roots, order };
}

/** The trace's time axis in epoch seconds: [t0,t1], from the trace summary and every timestamp seen in spans and marks. */
export function timeline(trace, spans, marks) {
  let t0 = Infinity,
    t1 = -Infinity;
  const see = (v) => {
    if (isNum(v)) {
      if (v < t0) t0 = v;
      if (v > t1) t1 = v;
    }
  };
  see(trace?.start_ts);
  see(trace?.end_ts);
  for (const s of spans || []) {
    see(s.start_ts);
    see(s.end_ts);
  }
  for (const m of marks || []) see(m.ts);
  if (!isNum(t0)) return { t0: 0, t1: 1, durMs: 1000, empty: true };
  if (t1 <= t0) t1 = t0 + 0.001;
  return { t0, t1, durMs: (t1 - t0) * 1000, empty: false };
}
export const relMs = (ts, tl) => (isNum(ts) ? (ts - tl.t0) * 1000 : null);

/** Rows to list: pre-order with the descendants of collapsed spans removed. */
export function visibleRows(tree, collapsed) {
  if (!collapsed || !collapsed.size) return tree.order.slice();
  const out = [];
  let skipDepth = Infinity;
  for (const n of tree.order) {
    if (n.depth > skipDepth) continue;
    skipDepth = Infinity;
    out.push(n);
    if (collapsed.has(n.id) && n.children.length) skipDepth = n.depth;
  }
  return out;
}

/** Ids of every span with children, to collapse them all. */
export const parentIds = (tree) => tree.order.filter((n) => n.children.length).map((n) => n.id);

/** Virtual scrolling: which rows to render for a scroll position (fixed row height) and the padding above and below. */
export function windowRows(total, scrollTop, viewportH, rowH, overscan = 8) {
  const first = clamp(Math.floor(scrollTop / rowH) - overscan, 0, Math.max(0, total));
  const last = clamp(Math.ceil((scrollTop + viewportH) / rowH) + overscan, 0, total);
  return { start: first, end: last, padTop: first * rowH, padBottom: Math.max(0, (total - last) * rowH) };
}

/** Index of a row to keep the given row visible: the new scrollTop, or null when it is already inside the viewport. */
export function scrollToRow(index, scrollTop, viewportH, rowH) {
  const top = index * rowH,
    bottom = top + rowH;
  if (top < scrollTop) return top;
  if (bottom > scrollTop + viewportH) return bottom - viewportH;
  return null;
}

/**
 * Bar of one span inside the zoom range `view` = {from,to} (ms from t0), as fractions 0..1 of the track.
 * Returns {l, w, tailL, tailW, hidden, clipL, clipR, noStart, open}. An open span has a solid stub (its known duration,
 * or nothing) and a tail to the trace end; a span with no start is a zero-width tick at its end.
 */
export function barGeometry(span, tl, view) {
  const range = view.to - view.from;
  const f = (ms) => (ms - view.from) / range;
  const noStart = !isNum(span.start_ts),
    open = span.status === 'open' || (!isNum(span.end_ts) && isNum(span.start_ts));
  const s = noStart ? relMs(span.end_ts, tl) : relMs(span.start_ts, tl);
  if (s == null || !(range > 0)) return { l: 0, w: 0, tailL: 0, tailW: 0, hidden: true, clipL: false, clipR: false, noStart, open };
  const e = noStart ? s : open ? s + (isNum(span.duration_ms) ? span.duration_ms : 0) : (relMs(span.end_ts, tl) ?? s);
  const tailEnd = open ? tl.durMs : e;
  const hidden = tailEnd < view.from || s > view.to;
  const l0 = clamp(f(s), 0, 1),
    l1 = clamp(f(e), 0, 1),
    t1 = clamp(f(tailEnd), 0, 1);
  return { l: l0, w: Math.max(0, l1 - l0), tailL: l1, tailW: open ? Math.max(0, t1 - l1) : 0, hidden, clipL: s < view.from, clipR: tailEnd > view.to, noStart, open };
}

/** Marks grouped by the span they hang under; marks whose parent is unknown hang under the first root. */
export function marksByParent(marks, tree) {
  const m = new Map();
  const fallback = tree.roots[0]?.id ?? null;
  for (const mk of marks || []) {
    const key = mk.parent_id != null && tree.nodes.has(mk.parent_id) ? mk.parent_id : fallback;
    if (key == null) continue;
    if (!m.has(key)) m.set(key, []);
    m.get(key).push(mk);
  }
  return m;
}

/** Duration of a span for sizing: its own, else (open) the time to the trace end, else 0. */
export function sizeMs(span, tl) {
  if (isNum(span.duration_ms)) return Math.max(0, span.duration_ms);
  if (isNum(span.start_ts) && !isNum(span.end_ts)) return Math.max(0, (tl.t1 - span.start_ts) * 1000);
  return 0;
}

/**
 * Partition icicle under `focusId` (default: the root, or all roots under a virtual one). Width = duration: each node's
 * children share its width in proportion to their durations; when children overlap in time (parallel work) the shares
 * are normalised so they still fit, and the layout reports `parallel`. Nodes narrower than minFrac are dropped with their
 * subtree. Returns {rects: [{id, node, depth, x0, x1}], focus, maxDepth, path (focus ancestors, root first)}.
 */
export function icicleLayout(tree, tl, focusId, { minFrac = 0.002, maxDepth = 64 } = {}) {
  let focus = (focusId && tree.nodes.get(focusId)) || null;
  const virtual = !focus && tree.roots.length !== 1;
  const top = focus || (virtual ? { id: '', span: null, children: tree.roots, depth: -1, virtual: true } : tree.roots[0]);
  const rects = [];
  if (!top) return { rects, focus: null, maxDepth: 0, path: [], parallel: false };
  let parallel = false;
  const baseDepth = top.virtual ? 0 : top.depth;
  const stack = [{ n: top, x0: 0, x1: 1 }];
  if (!top.virtual) rects.push({ id: top.id, node: top, depth: 0, x0: 0, x1: 1 });
  let deepest = 0;
  while (stack.length) {
    const { n, x0, x1 } = stack.pop();
    const kids = n.children.filter((c) => sizeMs(c.span, tl) > 0);
    if (!kids.length) continue;
    const sum = kids.reduce((a, c) => a + sizeMs(c.span, tl), 0);
    const own = n.virtual ? sum : Math.max(sizeMs(n.span, tl), 0) || sum;
    const denom = Math.max(own, sum);
    if (sum > own * 1.02 && !n.virtual) parallel = true;
    let x = x0;
    for (const c of kids) {
      const w = ((x1 - x0) * sizeMs(c.span, tl)) / denom;
      const depth = c.depth - baseDepth;
      if (w >= minFrac && depth <= maxDepth) {
        rects.push({ id: c.id, node: c, depth, x0: x, x1: x + w });
        deepest = Math.max(deepest, depth);
        stack.push({ n: c, x0: x, x1: x + w });
      }
      x += w;
    }
  }
  const path = [];
  for (let p = focus; p; p = p.parent) path.unshift(p);
  return { rects, focus, maxDepth: deepest, path, parallel };
}

/** Aggregate numbers for a trace's spans (the details header and tests): counts per category, errors, open. */
export function spanStats(spans) {
  const out = { total: 0, llm: 0, tool: 0, agent: 0, turn: 0, other: 0, errors: 0, open: 0 };
  for (const s of spans || []) {
    out.total++;
    const c = String(s.category ?? '').toLowerCase();
    if (c === 'llm') out.llm++;
    else if (c === 'tool') out.tool++;
    else if (c === 'agent') out.agent++;
    else if (c === 'turn' || c === 'custom') out.turn++;
    else out.other++;
    if (s.status === 'error') out.errors++;
    if (s.status === 'open') out.open++;
  }
  return out;
}
