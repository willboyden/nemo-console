// Geometry of the wave-3 charts (addendum 9.15): horizontal bar rows with optional interval whiskers (confidence intervals,
// durations, sizes, error codes), dumbbell rows (with vs without a skill), grouped columns (a reward distribution), swim lanes
// (requests by worker), the line chart of a percentile curve, and the scatter of runs. Pure functions, no DOM; the SVG is built
// in js/charts/*.js. A value that is not a finite number is "no value" and is never drawn as zero. Unit-tested in tests/viz-bars.test.mjs.
import { linear, niceScale, offsetTicks, logScale, logTicks, isNum } from './scale.js';
import { plotBox, packLanes } from './series.js';

const r1 = (v) => Math.round(v * 10) / 10;

/**
 * The axis ticks whose labels fit: a label is estimated at `charW` px per character and anchored at its tick (start at the left end, end at the right end, middle
 * between); a tick whose label would touch the one kept before it (within `gap` px) is dropped, so "140 GiB" and "186 GiB" never print over each other.
 * ticks: [{v, x}]; textOf(v) the label; returns the kept ticks with their `anchor`.
 */
export function fitTicks(ticks, textOf, { x0, x1, charW = 6.3, gap = 8 }) {
  const out = [];
  let right = -Infinity;
  for (const tk of ticks) {
    const w = String(textOf(tk.v)).length * charW;
    const anchor = tk.x < x0 + 12 ? 'start' : tk.x > x1 - 12 ? 'end' : 'middle';
    const left = anchor === 'start' ? tk.x : anchor === 'end' ? tk.x - w : tk.x - w / 2;
    if (left < right + gap) continue;
    out.push({ ...tk, anchor });
    right = left + w;
  }
  return out;
}

/**
 * Horizontal bars, one row per item, one or more bars per row (A and B side by side). All bars share one scale, so a longer bar
 * is a bigger number. A bar may carry an interval {lo, hi} (a whisker over the bar).
 * @param rows   [{id, label, bars: [{key, value|null, lo?, hi?, cls, text}]}]
 * @param domain optional {min, max}; default 0 .. a round number above the data (the interval's top counts)
 * Returns {width, height, x0, x1, x(v), domain, ticks: [{v, x}], rows: [{id, label, y, h, mid, bars: [{key, x, w, y, h, value, cls, text, missing, whisker: {x1, x2, y} | null}]}], empty}.
 */
export function hbarLayout({ rows, width, labelW = 120, tailW = 72, barH = 12, barGap = 3, rowGap = 9, top = 4, bottom = 22, domain = null, tickCount = 4 }) {
  const x0 = labelW,
    x1 = Math.max(labelW + 60, width - tailW);
  const vals = rows.flatMap((r) => r.bars.flatMap((b) => [b.value, b.hi, b.lo])).filter(isNum);
  if (!vals.length) return { width, height: top + bottom, x0, x1, x: () => x0, domain: { min: 0, max: 1 }, ticks: [], rows: [], empty: true };
  const lo = domain?.min ?? Math.min(0, ...vals);
  const hi = domain?.max ?? Math.max(...vals);
  const axis = domain
    ? { min: domain.min, max: domain.max, ticks: niceScale(domain.min, domain.max, tickCount).ticks.filter((v) => v >= domain.min - 1e-9 && v <= domain.max + 1e-9) }
    : niceScale(lo, hi, tickCount);
  // counts (every value a whole number) get whole-number ticks only: 0, 0.5, 1, 1.5, 2 would print as 0 1 1 2 2
  if (vals.every((v) => Number.isInteger(v))) axis.ticks = axis.ticks.filter((v) => Number.isInteger(v));
  const x = linear(axis.min, axis.max, x0, x1);
  const base = x(Math.max(axis.min, Math.min(0, axis.max)));
  let y = top;
  const out = rows.map((r) => {
    const h = r.bars.length * barH + Math.max(0, r.bars.length - 1) * barGap;
    const row = {
      id: r.id,
      label: r.label,
      y,
      h,
      mid: y + h / 2,
      bars: r.bars.map((b, i) => {
        const by = y + i * (barH + barGap);
        const has = isNum(b.value);
        const cx = has ? x(Math.min(axis.max, Math.max(axis.min, b.value))) : base;
        const left = Math.min(base, cx);
        const w = has ? Math.max(b.value === 0 ? 0 : 1.5, Math.abs(cx - base)) : 0;
        const whisker = isNum(b.lo) && isNum(b.hi) && has ? { x1: r1(x(Math.max(axis.min, b.lo))), x2: r1(x(Math.min(axis.max, b.hi))), y: r1(by + barH / 2) } : null;
        return {
          key: b.key,
          x: r1(left),
          w: r1(w),
          y: by,
          h: barH,
          value: has ? b.value : null,
          cls: b.cls,
          text: b.text,
          valueText: b.valueText,
          missing: !has,
          whisker,
          end: r1(Math.max(left + w, whisker?.x2 ?? 0)),
        };
      }),
    };
    y += h + rowGap;
    return row;
  });
  return { width, height: y - rowGap + bottom, x0, x1, x, domain: { min: axis.min, max: axis.max }, ticks: axis.ticks.map((v) => ({ v, x: r1(x(v)) })), rows: out, empty: false };
}

/**
 * Dumbbell rows: two values per row joined by a line (the value without a skill and with it). Header rows (an agent's name) carry no values.
 * @param rows [{id, label, header?, a, b}]   (a = without, b = with; either may be null)
 * Returns {width, height, x0, x1, x, domain, ticks, rows: [{id, label, header, y, cy, a: x|null, b: x|null, delta, dir: 'up'|'down'|'flat'|null}], empty}.
 */
export function dumbbellLayout({ rows, width, labelW = 140, tailW = 84, rowH = 26, headerH = 26, top = 4, bottom = 22, tickCount = 4 }) {
  const x0 = labelW,
    x1 = Math.max(labelW + 60, width - tailW);
  const vals = rows.flatMap((r) => (r.header ? [] : [r.a, r.b])).filter(isNum);
  if (!vals.length) return { width, height: top + bottom, x0, x1, x: () => x0, domain: { min: 0, max: 1 }, ticks: [], rows: [], empty: true };
  const axis = niceScale(Math.min(0, ...vals), Math.max(...vals), tickCount);
  const x = linear(axis.min, axis.max, x0, x1);
  let y = top;
  const out = rows.map((r) => {
    const h = r.header ? headerH : rowH;
    const has = !r.header;
    const delta = has && isNum(r.a) && isNum(r.b) ? r.b - r.a : null;
    const row = {
      id: r.id,
      label: r.label,
      header: !!r.header,
      y,
      cy: y + h / 2,
      a: has && isNum(r.a) ? r1(x(r.a)) : null,
      b: has && isNum(r.b) ? r1(x(r.b)) : null,
      delta,
      dir: delta == null ? null : Math.abs(delta) < 1e-9 ? 'flat' : delta > 0 ? 'up' : 'down',
    };
    y += h;
    return row;
  });
  return { width, height: y + bottom, x0, x1, x, domain: { min: axis.min, max: axis.max }, ticks: axis.ticks.map((v) => ({ v, x: r1(x(v)) })), rows: out, empty: false };
}

/**
 * Grouped columns: one group per category, one column per series in it (a histogram of A next to the histogram of B).
 * @param cats   category labels;  @param series [{key, cls, values: [count per category]}]
 * `minMax` keeps the y axis from being drawn up to 1 for a count of 1 (tick labels stay whole numbers).
 * Returns {plot, yTicks: [{v, y}], max, groups: [{i, x, w, cols: [{si, x, y, w, h, v}]}], empty}.
 */
export function columnLayout({ cats, series, width, height, margin = { l: 40, r: 10, t: 10, b: 38 }, minMax = 3 }) {
  const plot = plotBox(width, height, margin);
  const all = series.flatMap((s) => s.values).filter(isNum);
  if (!cats.length || !all.length || Math.max(...all) <= 0) return { plot, yTicks: [], max: 0, groups: [], empty: true };
  const axis = niceScale(0, Math.max(minMax, ...all), 3);
  const y = linear(axis.min, axis.max, plot.y0, plot.y1);
  const gw = plot.w / cats.length;
  const inner = Math.max(2, gw * 0.8);
  const cw = inner / Math.max(1, series.length);
  const groups = cats.map((_, i) => {
    const gx = plot.x0 + i * gw + (gw - inner) / 2;
    return {
      i,
      x: r1(plot.x0 + i * gw),
      w: r1(gw),
      cols: series.map((s, si) => {
        const v = isNum(s.values[i]) ? s.values[i] : 0;
        return { si, v, x: r1(gx + si * cw + (cw > 4 ? 0.5 : 0)), w: Math.max(1, r1(cw - (cw > 4 ? 1 : 0))), y: r1(y(v)), h: Math.max(0, r1(plot.y0 - y(v))) };
      }),
    };
  });
  return { plot, yTicks: axis.ticks.map((v) => ({ v, y: r1(y(v)) })), max: axis.max, groups, empty: false };
}

/**
 * Swim lanes: one lane per worker, a bar per request from its start to its end. Requests that overlap in a lane go into sub-rows (at
 * most `maxSub`; past that they overlap, and `overlapped` counts them).
 * @param lanes [{id, label}];  @param items [{i, lane (index into lanes), start, end, error}] (ms)
 * Returns {width, height, x0, x1, x, ticks: [{v, x, label}], lanes: [{id, label, y, h, subRows}], bars: [{i, lane, x, w, y, h, error, start, end}], overlapped, domainMs, empty}.
 */
export function swimLayout({ lanes, items, width, labelW = 64, subH = 8, subGap = 2, lanePad = 6, maxSub = 3, top = 4, bottom = 22, right = 10, minBarW = 1.5 }) {
  const x0 = labelW,
    x1 = Math.max(labelW + 60, width - right);
  const good = items.filter((it) => isNum(it.start) && isNum(it.end) && it.lane >= 0 && it.lane < lanes.length);
  if (!good.length) return { width, height: top + bottom, x0, x1, x: () => x0, ticks: [], lanes: [], bars: [], overlapped: 0, domainMs: 0, empty: true };
  const domainMs = Math.max(1, ...good.map((it) => Math.max(it.start, it.end)));
  const x = linear(0, domainMs, x0, x1);
  const bars = [];
  const outLanes = [];
  let y = top,
    overlapped = 0;
  lanes.forEach((lane, li) => {
    const mine = good.filter((it) => it.lane === li);
    const packed = packLanes(
      mine.map((it) => ({ start: it.start, end: Math.max(it.end, it.start) })),
      0,
      maxSub,
    );
    overlapped += packed.overlapped;
    const subRows = Math.max(1, packed.lanes);
    const h = subRows * subH + (subRows - 1) * subGap;
    mine.forEach((it, k) => {
      const sub = packed.items[k].lane;
      const bx = x(it.start);
      bars.push({
        i: it.i,
        lane: li,
        x: r1(bx),
        w: Math.max(minBarW, r1(x(Math.max(it.end, it.start)) - bx)),
        y: r1(y + sub * (subH + subGap)),
        h: subH,
        error: !!it.error,
        start: it.start,
        end: it.end,
      });
    });
    outLanes.push({ id: lane.id, label: lane.label, y, h, subRows });
    y += h + lanePad;
  });
  return {
    width,
    height: y - lanePad + bottom,
    x0,
    x1,
    x,
    ticks: offsetTicks(0, domainMs, width < 480 ? 4 : 6).map((tk) => ({ ...tk, x: r1(x(tk.v)) })),
    lanes: outLanes,
    bars,
    overlapped,
    domainMs,
    empty: false,
  };
}

/**
 * A line chart over categories (the percentile curve: p1, p5 ... p99 spaced evenly, so the tail stays readable). One value per
 * category per line, null = a gap (the pen is lifted, never drawn to zero).
 * @param cats  [label];  @param lines [{key, values: [number|null]}]
 * Returns {plot, yTicks, xs: [x per category], lines: [{key, d, pts: [{i, x, y, v}]}], empty}.
 */
export function curveLayout({ cats, lines, width, height, margin = { l: 50, r: 12, t: 10, b: 36 }, yTickCount = 4 }) {
  const plot = plotBox(width, height, margin);
  const vals = lines.flatMap((l) => l.values).filter(isNum);
  if (!cats.length || !vals.length) return { plot, yTicks: [], xs: [], lines: [], empty: true };
  const axis = niceScale(Math.min(0, ...vals), Math.max(...vals), yTickCount);
  const y = linear(axis.min, axis.max, plot.y0, plot.y1);
  const n = cats.length;
  const xs = cats.map((_, i) => r1(n === 1 ? (plot.x0 + plot.x1) / 2 : plot.x0 + 8 + (i * (plot.w - 16)) / (n - 1)));
  const out = lines.map((l) => {
    let d = '',
      pen = false;
    const pts = [];
    l.values.forEach((v, i) => {
      if (!isNum(v)) {
        pen = false;
        return;
      }
      const p = { i, x: xs[i], y: r1(y(v)), v };
      pts.push(p);
      d += `${pen ? 'L' : 'M'}${p.x} ${p.y}`;
      pen = true;
    });
    return { key: l.key, d, pts };
  });
  return { plot, yTicks: axis.ticks.map((v) => ({ v, y: r1(y(v)) })), xs, lines: out, empty: false };
}

/**
 * Scatter of runs (or results): points {id, x, y, g (a group index)}; a log x axis by default (tokens, latencies are long-tailed).
 * Points with a missing coordinate are left out and counted in `skipped`.
 * Returns {plot, xTicks: [{v, x}], yTicks: [{v, y}], dots: [{...the point, g, cx, cy}], skipped, empty}.
 */
export function pointsLayout({ points, width, height, margin = { l: 56, r: 14, t: 12, b: 38 }, logX = false, yDomain = null }) {
  const plot = plotBox(width, height, margin);
  const ok = points.filter((p) => isNum(p.x) && isNum(p.y) && (!logX || p.x > 0));
  const skipped = points.length - ok.length;
  if (!ok.length) return { plot, xTicks: [], yTicks: [], dots: [], skipped, empty: true };
  const xs = ok.map((p) => p.x),
    ys = ok.map((p) => p.y);
  let X, xTicks;
  if (logX) {
    const ticks = logTicks(Math.min(...xs), Math.max(...xs));
    X = logScale(ticks[0], ticks[ticks.length - 1], plot.x0 + 6, plot.x1 - 6);
    xTicks = ticks;
  } else {
    const ax = niceScale(Math.min(...xs), Math.max(...xs), 4);
    X = linear(ax.min, ax.max, plot.x0 + 6, plot.x1 - 6);
    xTicks = ax.ticks;
  }
  const ay = yDomain ? { min: yDomain.min, max: yDomain.max, ticks: niceScale(yDomain.min, yDomain.max, 4).ticks } : niceScale(Math.min(...ys), Math.max(...ys), 4);
  const Y = linear(ay.min, ay.max, plot.y0, plot.y1);
  return {
    plot,
    xTicks: xTicks.map((v) => ({ v, x: r1(X(v)) })),
    yTicks: ay.ticks.map((v) => ({ v, y: r1(Y(v)) })),
    dots: ok.map((p) => ({ ...p, g: p.g ?? 0, cx: r1(X(p.x)), cy: r1(Y(p.y)) })),
    skipped,
    empty: false,
  };
}
