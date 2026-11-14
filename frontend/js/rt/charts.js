// SVG renderers for the Analytics tab. Geometry comes from viz/*.js (pure, unit-tested); these only turn it into
// elements. Every chart is an svg role="group" with an aria-label, every mark has a <title>, an empty set is flagged, and
// series differ by dash pattern or label as well as colour. Class names (bf1.., ls1.., heat0..4) are styled in components.css.
import { svg, titled } from '../viz/svg.js';
import { t } from '../i18n.js';
import { fmtInt, fmtCompact } from '../format.js';
import { stackedBars, lineSeries } from '../viz/series.js';
import { histogramLayout, heatmapLayout, orientLatency, scatterLayout, flowLayout } from '../viz/histogram.js';
import { fmtOffsetMs, spreadIndices, clockTicks } from '../viz/scale.js';
import { fmtStamp, fmtDur } from '../viz/fmt.js';

const frame = (w, h, label, nodes, cls = '') => svg('svg', { class: `chart rtchart ${cls}`.trim(), viewBox: `0 0 ${w} ${h}`, role: 'group', 'aria-label': label }, svg('title', label), ...nodes);
const yGrid = (plot, ticks, fmt) =>
  ticks.flatMap((tk) => [svg('line', { class: 'gl', x1: plot.x0, x2: plot.x1, y1: tk.y, y2: tk.y }), svg('text', { class: 'tick', x: plot.x0 - 6, y: tk.y + 4, 'text-anchor': 'end' }, fmt(tk.v))]);
const xTimeTicks = (ticks, h) => ticks.map((tk) => svg('text', { class: 'tick', x: tk.x, y: h - 7, 'text-anchor': 'middle' }, tk.label));
export const compactNum = (v) => (Math.abs(v) >= 1000 ? fmtCompact(v) : String(Math.round(v * 10) / 10));

/** Stacked bars over time. series: [{key, label, cls (bf1..), values}]. */
export function stackedChart({ ts, bucketS, series, width = 560, height = 210, ariaLabel, yFmt = compactNum, unit = '' }) {
  const live = series.some((s) => s.values.some((v) => v > 0));
  if (!ts?.length || !live) return null;
  const g = stackedBars({ ts, bucketS, series, width, height });
  const bars = g.rects.map((r) => {
    const el = svg('rect', { class: series[r.si].cls, x: r.x, y: r.y, width: r.w, height: r.h });
    return titled(el, `${series[r.si].label}: ${fmtInt(r.v)}${unit} · ${fmtStamp(ts[r.i])} UTC`);
  });
  return frame(width, height, ariaLabel, [...yGrid(g.plot, g.yTicks, yFmt), ...bars, ...xTimeTicks(g.time.ticks, height)]);
}

/** Lines over time with gaps for missing data. series: [{key, label, cls (ls1..), dash ('dash'|'dot'|''), values}]. */
export function lineChart({ ts, bucketS, series, width = 560, height = 210, ariaLabel, yFmt = (v) => fmtOffsetMs(v) }) {
  if (!ts?.length || !series.some((s) => s.values.some((v) => v != null))) return null;
  const g = lineSeries({ ts, bucketS, series, width, height });
  const lines = g.lines.flatMap((ln, i) => {
    const s = series[i];
    return [
      ln.d ? svg('path', { class: `ln ${s.cls}${s.dash ? ` ln-${s.dash}` : ''}`, d: ln.d }) : null,
      ...ln.points.map((p) => titled(svg('circle', { class: `pt ${s.cls}`, cx: p.x, cy: p.y, r: 3 }), `${s.label}: ${yFmt(p.v)} · ${fmtStamp(ts[p.i])} UTC`)),
    ];
  });
  return frame(width, height, ariaLabel, [...yGrid(g.plot, g.yTicks, yFmt), ...lines, ...xTimeTicks(g.time.ticks, height)]);
}

/** Histogram with optional p50/p95 markers. hist = {edges, counts}; xFmt formats an edge. */
export function histogramChart({ hist, markers = {}, xFmt = compactNum, ariaLabel, width = 360, height = 176, barCls = 'bf1' }) {
  const g = histogramLayout({ edges: hist?.edges, counts: hist?.counts, width, height, markers, labelCount: width < 330 ? 4 : width < 480 ? 5 : 7 });
  if (g.empty) return null;
  const bars = g.bars.map((b) =>
    titled(svg('rect', { class: barCls, x: b.x, y: b.y, width: b.w, height: Math.max(0, b.h) }), t('rt.hist.bin', { lo: xFmt(b.lo), hi: xFmt(b.hi), count: fmtInt(b.count) })),
  );
  const mk = g.markers.flatMap((m, i) => [
    svg('line', { class: `hist-mk mk${i}`, x1: m.x, x2: m.x, y1: g.plot.y1, y2: g.plot.y0 }),
    svg('text', { class: `tick mk-l mk${i}`, x: Math.min(m.x + 3, g.plot.x1 - 24), y: g.plot.y1 + 9 + i * 11 }, `${m.key} ${xFmt(m.value)}`),
  ]);
  return frame(width, height, ariaLabel, [
    ...yGrid(g.plot, g.yTicks, compactNum),
    ...bars,
    ...mk,
    ...g.xTicks.map((tk) => svg('text', { class: 'tick', x: tk.x, y: height - 8, 'text-anchor': tk.i === 0 ? 'start' : tk.i === g.bars.length ? 'end' : 'middle' }, xFmt(tk.v))),
  ]);
}

const HEAT_STEPS = [0, 1, 2, 3, 4];
export const heatLegend = () => ({ steps: HEAT_STEPS });

/** Activity heatmap: days (rows) x hours of day (UTC). */
export function activityHeatmap(act, { width = 560, ariaLabel }) {
  const days = act?.days || [],
    counts = act?.counts || [];
  if (!days.length || !counts.flat().some((v) => v > 0)) return null;
  const g = heatmapLayout({ counts, width, cellH: 17, margin: { l: 50, r: 8, t: 4, b: 20 } });
  const cells = g.cells.map((c) =>
    titled(
      svg('rect', { class: `heat heat${c.level}`, x: c.x, y: c.y, width: c.w, height: c.h, rx: 2 }),
      t('rt.heat.act_tip', { day: days[c.r], hour: String(c.c).padStart(2, '0'), count: fmtInt(c.v) }),
    ),
  );
  const rowLabels = days.map((d, r) => svg('text', { class: 'tick', x: g.margin.l - 6, y: g.rowY(r) + 4, 'text-anchor': 'end' }, d.slice(5)));
  const colLabels = spreadIndices(g.cols, 9).map((c) => svg('text', { class: 'tick', x: g.margin.l + (c + 0.5) * g.cw, y: g.height - 6, 'text-anchor': 'middle' }, String(c).padStart(2, '0')));
  return frame(width, g.height, ariaLabel, [...cells, ...rowLabels, ...colLabels], 'heatchart');
}

/** Latency heatmap: time buckets (columns) x duration buckets (rows, fast at the bottom). */
export function latencyHeatmap(lat, { width = 560, ariaLabel }) {
  const ts = lat?.ts || [],
    edges = lat?.edges_ms || [];
  const counts = orientLatency(lat || {});
  if (!ts.length || edges.length < 2 || !counts.flat().some((v) => v > 0)) return null;
  const g = heatmapLayout({ counts, width, cellH: 15, margin: { l: 56, r: 8, t: 4, b: 22 }, flip: true });
  const cells = g.cells.map((c) =>
    titled(
      svg('rect', { class: `heat heat${c.level}`, x: c.x, y: c.y, width: c.w, height: c.h, rx: 1.5 }),
      t('rt.heat.lat_tip', { lo: fmtOffsetMs(edges[c.r]), hi: fmtOffsetMs(edges[c.r + 1]), when: fmtStamp(ts[c.c]), count: fmtInt(c.v) }),
    ),
  );
  const rowLabels = counts.map((_, r) => (r % 2 === 0 ? svg('text', { class: 'tick', x: g.margin.l - 6, y: g.rowY(r) + 4, 'text-anchor': 'end' }, fmtOffsetMs(edges[r])) : null));
  const t0 = ts[0],
    t1 = ts.at(-1) + (ts[1] - ts[0] || 60);
  const ticks = clockTicks(t0, t1, 6);
  const xOf = (v) => g.margin.l + ((v - t0) / Math.max(1, t1 - t0)) * (g.cw * g.cols);
  const colLabels = ticks.map((tk) => svg('text', { class: 'tick', x: xOf(tk.v), y: g.height - 7, 'text-anchor': 'middle' }, tk.label));
  return frame(width, g.height, ariaLabel, [...cells, ...rowLabels, ...colLabels], 'heatchart');
}

/** Scatter of LLM calls: duration (x, log) vs output tokens (y, log); colour = model class, an X mark = error. points carry trace_id/span_id. */
export function scatterChart(points, { modelClass, onOpen, width = 640, height = 300, ariaLabel }) {
  const g = scatterLayout({ points: points.map((p) => ({ ...p, x: p.duration_ms, y: p.output_tokens ?? 0 })), width, height });
  if (g.empty) return null;
  const grid = [
    ...g.yTicks.flatMap((tk) => [
      svg('line', { class: 'gl', x1: g.plot.x0, x2: g.plot.x1, y1: tk.y, y2: tk.y }),
      svg('text', { class: 'tick', x: g.plot.x0 - 6, y: tk.y + 4, 'text-anchor': 'end' }, compactNum(tk.v)),
    ]),
    ...g.xTicks.flatMap((tk) => [
      svg('line', { class: 'gl v', x1: tk.x, x2: tk.x, y1: g.plot.y1, y2: g.plot.y0 }),
      svg('text', { class: 'tick', x: tk.x, y: height - 14, 'text-anchor': 'middle' }, fmtOffsetMs(tk.v)),
    ]),
    svg('text', { class: 'axis-title', x: (g.plot.x0 + g.plot.x1) / 2, y: height - 1, 'text-anchor': 'middle' }, t('rt.scatter.x')),
    svg('text', { class: 'axis-title', x: 10, y: (g.plot.y0 + g.plot.y1) / 2, 'text-anchor': 'middle', transform: `rotate(-90 10 ${(g.plot.y0 + g.plot.y1) / 2})` }, t('rt.scatter.y')),
  ];
  const dots = g.dots.map((d) => {
    const err = d.status === 'error';
    const el = err ? svg('path', { class: 'sc-err', d: `M${d.cx - 4} ${d.cy - 4}l8 8m0-8l-8 8` }) : svg('circle', { class: `sc-dot ${modelClass(d.model)}`, cx: d.cx, cy: d.cy, r: 3.4 });
    const hit = svg(
      'g',
      {
        class: 'sc-pt',
        role: 'link',
        'aria-label': t('rt.scatter.tip', { model: d.model ?? '–', dur: fmtDur(d.duration_ms), out: fmtInt(d.output_tokens), inp: fmtInt(d.input_tokens), status: t(`rt.status.${d.status}`) }),
      },
      el,
      svg('circle', { class: 'sc-hit', cx: d.cx, cy: d.cy, r: 7 }),
    );
    titled(hit, t('rt.scatter.tip', { model: d.model ?? '–', dur: fmtDur(d.duration_ms), out: fmtInt(d.output_tokens), inp: fmtInt(d.input_tokens), status: t(`rt.status.${d.status}`) }));
    hit.addEventListener('click', () => onOpen?.(d));
    return hit;
  });
  return frame(width, height, ariaLabel, [...grid, ...dots], 'scatterchart');
}

/** Three-column flow (route -> decision -> served model) from analytics.routing.flows. */
export function flowChart(flows, { ariaLabel, width = 640, height = 250 }) {
  const f = flowLayout({ flows, width, height, margin: { l: 4, r: 4, t: 22, b: 6 } });
  if (!f.nodes.length) return null;
  const heads = f.layers === 3 ? ['rt.routing.route', 'rt.routing.decision', 'rt.routing.served'] : [];
  const lastLayer = f.layers - 1;
  const links = f.links.map((l) => titled(svg('path', { class: `flow-link fl${l.sourceLayer % 3}`, d: l.d }), t('rt.flow.link', { from: l.source, to: l.target, n: fmtInt(l.value) })));
  const nodes = f.nodes.flatMap((n) => [
    titled(svg('rect', { class: `flow-node fl${n.layer % 3}`, x: n.x, y: n.y, width: n.w, height: n.h, rx: 2 }), t('rt.flow.node', { name: n.name, n: fmtInt(n.value) })),
    svg(
      'text',
      { class: 'flow-label', x: n.layer === lastLayer && lastLayer > 0 ? n.x - 6 : n.x + n.w + 6, y: n.y + n.h / 2 + 4, 'text-anchor': n.layer === lastLayer && lastLayer > 0 ? 'end' : 'start' },
      `${n.name} · ${fmtInt(n.value)}`,
    ),
  ]);
  const headers = f.nodes.length
    ? [...new Set(f.nodes.map((n) => n.layer))]
        .sort()
        .map((l) =>
          heads[l]
            ? svg(
                'text',
                { class: 'flow-head', x: f.nodes.find((n) => n.layer === l).x + (l === lastLayer ? f.nodes.find((n) => n.layer === l).w : 0), y: 12, 'text-anchor': l === lastLayer ? 'end' : 'start' },
                t(heads[l]),
              )
            : null,
        )
    : [];
  return frame(width, height, ariaLabel, [...headers, ...links, ...nodes], 'flowchart');
}
