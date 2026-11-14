// Histogram, heatmap, scatter and flow (Sankey) geometry. Pure functions.
import { linear, logScale, logTicks, niceScale, binPosition, spreadIndices, isNum } from './scale.js';
import { plotBox } from './series.js';
const r1 = (v) => Math.round(v * 10) / 10;

/**
 * Histogram from {edges:[n+1], counts:[n]}. Bins are equal-width slots (the edges are usually geometric, so this is a log
 * axis); markers {key: value} are placed by interpolating the value among the edges. Returns bars, ticks and markers.
 */
export function histogramLayout({ edges, counts, width, height, margin = { l: 44, r: 12, t: 12, b: 34 }, markers = {}, labelCount = 6 }) {
  const plot = plotBox(width, height, margin);
  const n = Array.isArray(counts) ? counts.length : 0;
  if (!n || !Array.isArray(edges) || edges.length !== n + 1) return { plot, bars: [], xTicks: [], yTicks: [], markers: [], total: 0, empty: true };
  const total = counts.reduce((a, c) => a + (isNum(c) ? c : 0), 0);
  const axis = niceScale(0, Math.max(...counts.map((c) => c || 0), 1), 3);
  const y = linear(axis.min, axis.max, plot.y0, plot.y1);
  const slot = plot.w / n;
  const bars = counts.map((c, i) => {
    const v = isNum(c) ? c : 0;
    return { i, count: v, lo: edges[i], hi: edges[i + 1], x: r1(plot.x0 + i * slot + (slot > 3 ? 0.5 : 0)), w: Math.max(0.5, r1(slot - (slot > 3 ? 1 : 0))), y: r1(y(v)), h: r1(plot.y0 - y(v)) };
  });
  const xTicks = spreadIndices(n + 1, labelCount).map((i) => ({ i, v: edges[i], x: r1(plot.x0 + i * slot) }));
  const mk = Object.entries(markers)
    .map(([key, value]) => {
      const pos = binPosition(edges, value);
      return pos == null ? null : { key, value, x: r1(plot.x0 + pos * slot) };
    })
    .filter(Boolean);
  return { plot, bars, xTicks, yTicks: axis.ticks.map((v) => ({ v, y: r1(y(v)) })), markers: mk, total, empty: total === 0 };
}

/** Heat level 0..4 for a count: 0 only for zero, otherwise a square-root ramp so sparse cells stay visible. */
export function heatLevel(v, max) {
  if (!isNum(v) || v <= 0 || !(max > 0)) return 0;
  return Math.min(4, Math.max(1, Math.ceil(Math.sqrt(v / max) * 4)));
}

/** Heatmap grid: counts[row][col]; row 0 is drawn at the top (or bottom when `flip`). Cells carry level and value. */
export function heatmapLayout({ counts, width, cellH = 16, margin = { l: 52, r: 10, t: 6, b: 22 }, flip = false }) {
  const rows = counts.length,
    cols = rows ? Math.max(...counts.map((r) => r.length)) : 0;
  const w = width - margin.l - margin.r;
  const cw = cols ? w / cols : 0;
  const height = margin.t + rows * cellH + margin.b;
  const max = Math.max(0, ...counts.flat().filter(isNum));
  const cells = [];
  counts.forEach((row, r) => {
    const rr = flip ? rows - 1 - r : r;
    for (let c = 0; c < cols; c++) {
      const v = isNum(row[c]) ? row[c] : 0;
      cells.push({ r, c, v, level: heatLevel(v, max), x: r1(margin.l + c * cw), y: margin.t + rr * cellH, w: r1(Math.max(0.5, cw - 1)), h: cellH - 1 });
    }
  });
  return { width, height, margin, rows, cols, cw, cellH, max, cells, rowY: (r) => margin.t + (flip ? rows - 1 - r : r) * cellH + cellH / 2 };
}

/**
 * The latency heatmap's counts may be [time][bucket] or [bucket][time] (the contract does not fix it): orient to
 * [bucket][time] by the shapes of ts and edges. Square shapes are taken as [time][bucket].
 */
export function orientLatency({ ts = [], edges_ms: edges = [], counts = [] }) {
  const nb = Math.max(0, edges.length - 1),
    nt = ts.length;
  const rows = counts.length,
    cols = counts[0]?.length ?? 0;
  const byTime = rows === nt && cols === nb;
  const byBucket = rows === nb && cols === nt;
  if (byBucket && !byTime) return counts;
  // [time][bucket] -> [bucket][time]
  return Array.from({ length: cols }, (_, b) => Array.from({ length: rows }, (_, t) => counts[t][b] ?? 0));
}

/**
 * Scatter with log axes by default. points need numeric x,y; zero or negative values sit on the axis floor. Returns dots
 * with cx, cy, the axis ticks and the plot box.
 */
export function scatterLayout({ points, width, height, margin = { l: 56, r: 14, t: 10, b: 34 }, logX = true, logY = true }) {
  const plot = plotBox(width, height, margin);
  const ok = points.filter((p) => isNum(p.x) && isNum(p.y));
  if (!ok.length) return { plot, dots: [], xTicks: [], yTicks: [], empty: true };
  const bounds = (vals, log) => {
    const pos = vals.filter((v) => v > 0);
    const hi = Math.max(...vals, 1);
    if (!log) return { lo: 0, hi: niceScale(0, hi, 4).max };
    const lo = pos.length ? Math.min(...pos) : 1;
    const t = logTicks(lo, hi);
    return { lo: t[0], hi: t[t.length - 1] };
  };
  const bx = bounds(
    ok.map((p) => p.x),
    logX,
  );
  const by = bounds(
    ok.map((p) => p.y),
    logY,
  );
  const X = logX ? logScale(bx.lo, bx.hi, plot.x0, plot.x1) : linear(bx.lo, bx.hi, plot.x0, plot.x1);
  const Y = logY ? logScale(by.lo, by.hi, plot.y0, plot.y1) : linear(by.lo, by.hi, plot.y0, plot.y1);
  const tk = (b, log) => (log ? logTicks(b.lo, b.hi) : niceScale(b.lo, b.hi, 4).ticks);
  return {
    plot,
    dots: ok.map((p) => ({ ...p, cx: r1(X(p.x)), cy: r1(Y(p.y)) })),
    xTicks: tk(bx, logX).map((v) => ({ v, x: r1(X(v)) })),
    yTicks: tk(by, logY).map((v) => ({ v, y: r1(Y(v)) })),
    empty: false,
  };
}

/**
 * Three-column Sankey from flows [{source, target, value}]. Layers come from the graph: names that are never a target
 * are layer 0, targets of layer-0 edges are layer 1, and so on (cycles and self loops are ignored). Nodes are stacked per
 * layer, heights proportional to throughput, links are filled bands. Returns {nodes, links, width, height} where a link's
 * `d` is the closed band path.
 */
export function flowLayout({ flows, width = 640, height = 260, nodeW = 14, gap = 8, margin = { l: 6, r: 6, t: 6, b: 6 } }) {
  const edges = (flows || []).filter((f) => f && isNum(f.value) && f.value > 0 && f.source != null && f.target != null && f.source !== f.target);
  if (!edges.length) return { nodes: [], links: [], width, height, layers: 0 };
  const targets = new Set(edges.map((e) => e.target));
  const layerOf = new Map();
  for (const e of edges) if (!targets.has(e.source)) layerOf.set(e.source, 0);
  if (!layerOf.size) for (const e of edges) layerOf.set(e.source, 0); // a pure cycle: anchor it at the sources
  for (let pass = 0; pass < 8; pass++) {
    let changed = false;
    for (const e of edges) {
      if (!layerOf.has(e.source)) continue;
      const l = layerOf.get(e.source) + 1;
      if (!layerOf.has(e.target) || layerOf.get(e.target) < l) {
        if (l <= 6) {
          layerOf.set(e.target, l);
          changed = true;
        }
      }
    }
    if (!changed) break;
  }
  const layers = Math.max(...layerOf.values()) + 1;
  const names = new Map();
  for (const e of edges) {
    for (const nm of [e.source, e.target]) if (!names.has(nm)) names.set(nm, { name: nm, layer: layerOf.get(nm) ?? 0, in: 0, out: 0 });
    names.get(e.source).out += e.value;
    names.get(e.target).in += e.value;
  }
  const nodes = [...names.values()].map((n) => ({ ...n, value: Math.max(n.in, n.out) }));
  const innerH = height - margin.t - margin.b,
    innerW = width - margin.l - margin.r;
  // One scale for every layer: the tallest layer fills the height, so equal flows look equal across columns.
  const layerTotals = Array.from({ length: layers }, (_, l) => nodes.filter((n) => n.layer === l));
  const k = Math.min(
    ...layerTotals.map(
      (ns) =>
        (innerH - Math.max(0, ns.length - 1) * gap) /
        Math.max(
          1,
          ns.reduce((a, n) => a + n.value, 0),
        ),
    ),
  );
  layerTotals.forEach((ns, l) => {
    ns.sort((a, b) => b.value - a.value || String(a.name).localeCompare(String(b.name)));
    const used = ns.reduce((a, n) => a + n.value * k, 0) + Math.max(0, ns.length - 1) * gap;
    let y = margin.t + (innerH - used) / 2;
    for (const n of ns) {
      n.x = r1(margin.l + (layers === 1 ? 0 : (l * (innerW - nodeW)) / (layers - 1)));
      n.y = r1(y);
      n.h = Math.max(2, r1(n.value * k));
      n.w = nodeW;
      n.outY = n.y;
      n.inY = n.y;
      y += n.h + gap;
    }
  });
  const byName = new Map(nodes.map((n) => [n.name, n]));
  const links = edges
    .slice()
    .sort((a, b) => byName.get(a.target).y - byName.get(b.target).y || b.value - a.value)
    .map((e) => {
      const s = byName.get(e.source),
        t = byName.get(e.target);
      const th = Math.max(1, e.value * k);
      const y0 = s.outY,
        y1 = t.inY;
      s.outY += th;
      t.inY += th;
      const x0 = s.x + s.w,
        x1 = t.x,
        mx = (x0 + x1) / 2;
      const d = `M${r1(x0)} ${r1(y0)}C${r1(mx)} ${r1(y0)} ${r1(mx)} ${r1(y1)} ${r1(x1)} ${r1(y1)}L${r1(x1)} ${r1(y1 + th)}C${r1(mx)} ${r1(y1 + th)} ${r1(mx)} ${r1(y0 + th)} ${r1(x0)} ${r1(y0 + th)}Z`;
      return { source: e.source, target: e.target, value: e.value, d, sourceLayer: s.layer };
    });
  return { nodes, links, width, height, layers, nodeW };
}
