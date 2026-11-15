// The two charts of a session: the swim lane (its traces on one timeline) and the context growth chart (input tokens,
// cached vs uncached, and output tokens per LLM call). Both are SVG built from pure layouts in viz/series.js; every mark
// has a <title>, the SVG a text alternative, and the page lists the same data in tables.
import { svg, titled } from '../viz/svg.js';
import { t } from '../i18n.js';
import { fmtInt } from '../format.js';
import { packLanes, contextGrowth } from '../viz/series.js';
import { linear, clockTicks } from '../viz/scale.js';
import { fmtDur, fmtStamp, fmtTok, fmtPercent } from '../viz/fmt.js';

/** Swim lane: traces [{trace_id,start_ts,end_ts,status,root_name,...}] packed into the fewest lanes over the session's time span. */
export function swimLane(traces, { onOpen, now = Date.now() / 1000, width = 1000, maxLanes = 12 } = {}) {
  const W = width,
    LANE = 22,
    m = { l: 8, r: 8, t: 6, b: 24 };
  const items = traces.map((tr) => ({ tr, start: tr.start_ts, end: tr.end_ts ?? Math.max(tr.start_ts + 1, Math.min(now, tr.start_ts + 60)) }));
  const t0 = Math.min(...items.map((i) => i.start)),
    t1 = Math.max(...items.map((i) => i.end), t0 + 1);
  const x = linear(t0, t1, m.l, W - m.r);
  const minW = 6;
  const packed = packLanes(
    items.map((i) => ({ ...i, end: Math.max(i.end, i.start + (minW / (W - m.l - m.r)) * (t1 - t0)) })),
    ((minW + 1) / (W - m.l - m.r)) * (t1 - t0),
    maxLanes,
  );
  // a long session of short traces would need hundreds of lanes at this scale: cap them and say how many bars overlap
  if (packed.overlapped) m.t = 22;
  const H = m.t + packed.lanes * LANE + m.b;
  const ticks = clockTicks(t0, t1, 7);
  const focusable = traces.length <= 60;
  const bars = packed.items.map((it) => {
    const tr = it.tr;
    const w = Math.max(minW, x(it.end) - x(it.start));
    const r = svg('rect', {
      class: `lane-bar st-${tr.status}`,
      x: x(it.start).toFixed(1),
      y: m.t + it.lane * LANE + 2,
      width: w.toFixed(1),
      height: LANE - 5,
      rx: 2,
      role: 'link',
      tabindex: focusable ? '0' : null,
      'aria-label': t('rt.lane.bar_label', {
        name: tr.root_name ?? '–',
        when: fmtStamp(tr.start_ts),
        dur: tr.status === 'open' ? t('rt.status.open') : fmtDur(tr.duration_ms),
        status: t(`rt.status.${tr.status}`),
      }),
    });
    titled(
      r,
      t('rt.lane.bar_title', {
        name: tr.root_name ?? '–',
        when: fmtStamp(tr.start_ts),
        dur: tr.status === 'open' ? t('rt.status.open') : fmtDur(tr.duration_ms),
        status: t(`rt.status.${tr.status}`),
        tokens: fmtTok(tr.total_tokens),
      }),
    );
    r.addEventListener('click', () => onOpen?.(tr));
    r.addEventListener('keydown', (e) => (e.key === 'Enter' || e.key === ' ') && (e.preventDefault(), onOpen?.(tr)));
    return r;
  });
  const crowded = packed.overlapped ? t('rt.lane.crowded', { n: fmtInt(packed.overlapped) }) : null;
  const alt = t('rt.lane.alt', { n: fmtInt(traces.length), lanes: packed.lanes, from: fmtStamp(t0), to: fmtStamp(t1) }) + (crowded ? `. ${crowded}` : '');
  return svg(
    'svg',
    { class: 'chart swimlane', viewBox: `0 0 ${W} ${H}`, role: 'group', 'aria-label': alt },
    svg('title', alt),
    ...ticks.map((tk) => svg('line', { class: 'gl', x1: x(tk.v), x2: x(tk.v), y1: m.t, y2: H - m.b })),
    ...Array.from({ length: packed.lanes }, (_, i) => svg('line', { class: 'lane-rule', x1: m.l, x2: W - m.r, y1: m.t + (i + 1) * LANE, y2: m.t + (i + 1) * LANE })),
    ...bars,
    ...(crowded ? [svg('text', { class: 'tick', x: W - m.r, y: 14, 'text-anchor': 'end' }, crowded)] : []),
    ...ticks.map((tk) => svg('text', { class: 'tick', x: x(tk.v), y: H - 8, 'text-anchor': 'middle' }, tk.label)),
  );
}

/** Context growth. calls: SessionCall[]; mode 'time' | 'index'. onOpen(call) opens the trace and span of the call. */
export function contextChart(calls, { mode = 'time', onOpen, maxInput = null, width = 760, height = 340 } = {}) {
  const g = contextGrowth({ calls, mode, width, height });
  const tb = g.top.box,
    bb = g.bottom.box;
  const alt = t('rt.ctx.alt', {
    n: fmtInt(calls.length),
    first: fmtTok(calls[0]?.input_tokens),
    last: fmtTok(calls.at(-1)?.input_tokens),
    max: fmtTok(Math.max(0, ...calls.map((c) => c.input_tokens ?? 0))),
  });
  const nodes = [svg('title', alt)];
  for (const tk of g.top.yTicks)
    nodes.push(svg('line', { class: 'gl', x1: tb.x0, x2: tb.x1, y1: tk.y, y2: tk.y }), svg('text', { class: 'tick', x: tb.x0 - 6, y: tk.y + 4, 'text-anchor': 'end' }, fmtTok(tk.v)));
  for (const tk of g.bottom.yTicks)
    nodes.push(svg('line', { class: 'gl', x1: bb.x0, x2: bb.x1, y1: tk.y, y2: tk.y }), svg('text', { class: 'tick', x: bb.x0 - 6, y: tk.y + 4, 'text-anchor': 'end' }, fmtTok(tk.v)));
  nodes.push(svg('text', { class: 'panel-title', x: tb.x0, y: tb.y1 - 9 }, t('rt.ctx.top')), svg('text', { class: 'panel-title', x: bb.x0, y: bb.y1 - 9 }, t('rt.ctx.bottom')));
  if (maxInput) {
    const yy = g.yT(maxInput);
    if (yy >= tb.y1 && yy <= tb.y0)
      nodes.push(
        svg('line', { class: 'ctx-max', x1: tb.x0, x2: tb.x1, y1: yy, y2: yy }),
        svg('text', { class: 'tick ctx-max-l', x: tb.x1, y: yy - 3, 'text-anchor': 'end' }, t('rt.ctx.max', { n: fmtTok(maxInput) })),
      );
  }
  g.bars.forEach((b, i) => {
    const c = b.call;
    const tip = t('rt.ctx.tip', {
      n: i + 1,
      model: c.model ?? '–',
      input: fmtTok(c.input_tokens),
      cached: fmtTok(c.cached_tokens),
      hit: c.input_tokens ? fmtPercent((c.cached_tokens ?? 0) / c.input_tokens) : '–',
      output: fmtTok(c.output_tokens),
      dur: fmtDur(c.duration_ms),
      status: t(`rt.status.${c.status}`),
    });
    const grp = svg('g', { class: `ctx-call${c.status === 'error' ? ' err' : ''}`, role: 'link', 'aria-label': tip });
    titled(grp, tip);
    grp.append(
      svg('rect', { class: 'ctx-hit', x: b.x - 1, y: tb.y1, width: b.w + 2, height: bb.y0 - tb.y1 }),
      b.cached.h > 0 ? svg('rect', { class: 'ctx-cached', x: b.x, y: b.cached.y, width: b.w, height: Math.max(0.5, b.cached.h) }) : null,
      b.uncached.h > 0 ? svg('rect', { class: 'ctx-in', x: b.x, y: b.uncached.y, width: b.w, height: Math.max(0.5, b.uncached.h) }) : null,
      b.out.h > 0 ? svg('rect', { class: 'ctx-out', x: b.x, y: b.out.y, width: b.w, height: Math.max(0.5, b.out.h) }) : null,
      c.status === 'error' ? svg('path', { class: 'ctx-err', d: `M${b.cx - 3} ${tb.y1 + 6}l6 6m0-6l-6 6` }) : null,
    );
    grp.addEventListener('click', () => onOpen?.(c));
    nodes.push(grp);
  });
  for (const tk of g.xTicks) nodes.push(svg('text', { class: 'tick', x: tk.x, y: height - 6, 'text-anchor': 'middle' }, tk.label));
  if (mode === 'index')
    nodes.push(
      svg('text', { class: 'tick', x: bb.x0, y: height - 6 }, '1'),
      svg('text', { class: 'tick', x: bb.x1, y: height - 6, 'text-anchor': 'end' }, String(calls.length)),
      svg('text', { class: 'tick', x: (bb.x0 + bb.x1) / 2, y: height - 6, 'text-anchor': 'middle' }, t('rt.ctx.call_no')),
    );
  return svg('svg', { class: 'chart ctxchart', viewBox: `0 0 ${width} ${height}`, role: 'group', 'aria-label': alt }, ...nodes);
}
