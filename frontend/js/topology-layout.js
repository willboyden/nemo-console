// Layered left-to-right layout for the topology graph. Pure: nodes/edges in, coordinates out.
// rank(node) = longest path from any source (nodes with no incoming edge); cycles are broken by ignoring back edges.
export function computeRanks(nodes, edges) {
  const ids = new Set(nodes.map((n) => n.id));
  const es = edges.filter((e) => ids.has(e.src) && ids.has(e.dst) && e.src !== e.dst);
  const out = new Map(nodes.map((n) => [n.id, []]));
  for (const e of es) out.get(e.src).push(e.dst);
  const rank = new Map(nodes.map((n) => [n.id, 0]));
  // Bellman-Ford style relaxation, bounded by |V| passes, which also terminates on cycles.
  for (let pass = 0; pass < nodes.length; pass++) {
    let changed = false;
    for (const e of es) {
      const r = rank.get(e.src) + 1;
      if (r > rank.get(e.dst) && r < nodes.length) { rank.set(e.dst, r); changed = true; }
    }
    if (!changed) break;
  }
  return rank;
}

export function layoutTopology(nodes, edges, { nodeW = 150, nodeH = 54, gapX = 60, gapY = 20, pad = 12 } = {}) {
  const rank = computeRanks(nodes, edges);
  const cols = [];
  for (const n of nodes) { const r = rank.get(n.id); (cols[r] ||= []).push(n); }
  const dense = cols.filter(Boolean);
  const maxRows = Math.max(1, ...dense.map((c) => c.length));
  const height = pad * 2 + maxRows * nodeH + (maxRows - 1) * gapY;
  const width = pad * 2 + dense.length * nodeW + (dense.length - 1) * gapX;
  const pos = new Map();
  dense.forEach((col, ci) => {
    const colH = col.length * nodeH + (col.length - 1) * gapY;
    const y0 = (height - colH) / 2;
    col.forEach((n, ri) => pos.set(n.id, { id: n.id, x: pad + ci * (nodeW + gapX), y: y0 + ri * (nodeH + gapY), w: nodeW, h: nodeH, rank: ci }));
  });
  const laid = nodes.map((n) => ({ ...n, ...pos.get(n.id) }));
  const round = (v) => Math.round(v * 10) / 10;
  const laidEdges = edges.filter((e) => pos.has(e.src) && pos.has(e.dst)).map((e) => {
    const a = pos.get(e.src), b = pos.get(e.dst);
    const x1 = a.x + a.w, y1 = a.y + a.h / 2, x2 = b.x, y2 = b.y + b.h / 2;
    const forward = x2 > x1;
    const dx = forward ? (x2 - x1) / 2 : 40;
    const path = `M${round(x1)} ${round(y1)}C${round(x1 + dx)} ${round(y1)} ${round(x2 - dx)} ${round(y2)} ${round(x2)} ${round(y2)}`;
    // Label sits at the edge midpoint (the inter-column gap, so it never sits under a node) on a background chip.
    return { ...e, path, lx: round(x1 + (x2 - x1) * 0.5), ly: round(y1 + (y2 - y1) * 0.5) };
  });
  return { nodes: laid, edges: laidEdges, width, height };
}

/** Plain-text alternative: one sentence per edge, plus nodes without edges. */
export function describeTopology(nodes, edges, { active = 'active', inactive = 'inactive' } = {}) {
  const label = new Map(nodes.map((n) => [n.id, n.label]));
  return edges.map((e) => `${label.get(e.src) ?? e.src} -> ${label.get(e.dst) ?? e.dst}${e.label ? ` (${e.label})` : ''}: ${e.active === false ? inactive : active}`);
}

/** Wrap a node label into at most `maxLines` lines of about `width` chars; the last line is ellipsised. Full text stays available via <title>. */
export function wrapLabel(label, width = 20, maxLines = 2) {
  const words = String(label ?? '').split(/\s+/).filter(Boolean), lines = [];
  let cur = '';
  for (const w of words) {
    if (!cur) cur = w; else if ((cur + ' ' + w).length <= width) cur += ' ' + w; else { lines.push(cur); cur = w; }
  }
  if (cur) lines.push(cur);
  const cut = (x) => (x.length > width ? `${x.slice(0, width - 1)}…` : x);
  if (lines.length > maxLines) { const head = lines.slice(0, maxLines - 1), tail = lines.slice(maxLines - 1).join(' '); return [...head.map(cut), cut(tail)]; }
  return lines.map(cut);
}
