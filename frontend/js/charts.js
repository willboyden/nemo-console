// Hand-rolled SVG charts returning strings (labels are escaped; styling is via CSS classes so a strict
// style-src CSP works). Everything numeric is formatted here; nothing untrusted is interpolated raw.
import { escapeHtml as esc } from './escape.js';

export const scaleLinear = (d0, d1, r0, r1) => (v) => (d1 === d0 ? (r0 + r1) / 2 : r0 + ((v - d0) / (d1 - d0)) * (r1 - r0));

export function niceTicks(min, max, count = 5) {
  if (!Number.isFinite(min) || !Number.isFinite(max)) return [0, 1];
  if (max === min) { max = min + 1; }
  const raw = (max - min) / Math.max(1, count);
  const mag = 10 ** Math.floor(Math.log10(raw));
  const step = [1, 2, 2.5, 5, 10].map((m) => m * mag).find((s) => s >= raw) || 10 * mag;
  const out = [];
  for (let v = Math.ceil(min / step) * step; v <= max + step * 1e-9; v += step) out.push(+v.toFixed(10));
  return out;
}

const n1 = (v) => (Math.round(v * 10) / 10).toString();
export function linePath(pts) {
  return pts.map(([x, y], i) => `${i ? 'L' : 'M'}${n1(x)} ${n1(y)}`).join('');
}

export function sparkline(values, { w = 120, h = 28, cls = 's1' } = {}) {
  const v = (values || []).filter((x) => Number.isFinite(x));
  if (v.length < 2) return `<svg class="spark" viewBox="0 0 ${w} ${h}" aria-hidden="true"></svg>`;
  const lo = Math.min(...v), hi = Math.max(...v);
  const x = scaleLinear(0, v.length - 1, 1, w - 1), y = scaleLinear(lo, hi === lo ? lo + 1 : hi, h - 2, 2);
  const pts = v.map((val, i) => [x(i), y(val)]);
  return `<svg class="spark" viewBox="0 0 ${w} ${h}" preserveAspectRatio="none" aria-hidden="true"><path class="area ${cls}" d="${linePath(pts)}L${w - 1} ${h}L1 ${h}Z"/><path class="line ${cls}" d="${linePath(pts)}"/></svg>`;
}

/**
 * series: [{name, points:[[x,y],...]}]; series i uses class s{i%6+1}.
 * opts: width,height,xFormat,yFormat,yMin,yMax,ariaLabel
 */
export function lineChart(series, opts = {}) {
  const { width = 640, height = 220, xFormat = String, yFormat = (v) => String(v), yMin, yMax, ariaLabel = 'chart' } = opts;
  const m = { l: 46, r: 12, t: 10, b: 24 };
  const all = series.flatMap((s) => s.points).filter(([x, y]) => Number.isFinite(x) && Number.isFinite(y));
  if (!all.length) return `<svg class="chart" viewBox="0 0 ${width} ${height}" role="img" aria-label="${esc(ariaLabel)}"><text class="empty" x="${width / 2}" y="${height / 2}" text-anchor="middle">–</text></svg>`;
  const xs = all.map((p) => p[0]), ys = all.map((p) => p[1]);
  const x0 = Math.min(...xs), x1 = Math.max(...xs);
  const lo = yMin ?? Math.min(0, ...ys);
  const ticks = niceTicks(lo, yMax ?? Math.max(...ys), 4);
  // niceTicks stops at or below the data max; extend by one step so the top of the data is never drawn outside the plot.
  if (ticks.length > 1 && ticks[ticks.length - 1] < Math.max(...ys)) ticks.push(+(ticks[ticks.length - 1] + (ticks[1] - ticks[0])).toFixed(10));
  const y0 = ticks[0], y1 = ticks[ticks.length - 1];
  const X = scaleLinear(x0, x1, m.l, width - m.r), Y = scaleLinear(y0, y1, height - m.b, m.t);
  let g = '';
  for (const tk of ticks) g += `<line class="gl" x1="${m.l}" x2="${width - m.r}" y1="${n1(Y(tk))}" y2="${n1(Y(tk))}"/><text class="tick" x="${m.l - 6}" y="${n1(Y(tk) + 4)}" text-anchor="end">${esc(yFormat(tk))}</text>`;
  const xt = niceTicks(x0, x1, 5);
  for (const tk of xt) g += `<text class="tick" x="${n1(X(tk))}" y="${height - 6}" text-anchor="middle">${esc(xFormat(tk))}</text>`;
  const step = Math.max(1, Math.ceil(Math.max(...series.map((s) => s.points.length)) / 60));
  let body = '';
  series.forEach((s, i) => {
    const cls = `s${(i % 6) + 1}`;
    const pts = s.points.filter(([x, y]) => Number.isFinite(x) && Number.isFinite(y)).map(([x, y]) => [X(x), Y(y), x, y]);
    if (!pts.length) return;
    body += `<path class="line ${cls}" d="${linePath(pts)}"/>`;
    // Hover targets carry native <title> tooltips (keyboard-inert, so the chart also ships a legend and data summary).
    pts.forEach((p, k) => { if (k % step === 0 || k === pts.length - 1) body += `<circle class="hit ${cls}" cx="${n1(p[0])}" cy="${n1(p[1])}" r="5"><title>${esc(s.name)}: ${esc(yFormat(p[3]))} @ ${esc(xFormat(p[2]))}</title></circle>`; });
  });
  return `<svg class="chart" viewBox="0 0 ${width} ${height}" role="img" aria-label="${esc(ariaLabel)}">${g}${body}</svg>`;
}

export function legend(series) {
  return `<ul class="legend">${series.map((s, i) => `<li><i class="sw s${(i % 6) + 1}"></i>${esc(s.name)}</li>`).join('')}</ul>`;
}


/**
 * Share bars: rows [{name, value, pct, cls?}] as horizontal bars. Every bar has an aria-label and a <title>
 * ("name: 412 requests (37.2%)") so the chart is not colour-only; the svg carries `ariaLabel` as its summary.
 */
export function shareBars(rows, { width = 380, rowH = 28, unit = 'requests', ariaLabel = 'share chart', pctFmt = (p) => `${p.toFixed(1)}%`, fmt = (v) => String(v) } = {}) {
  if (!rows.length) return `<svg class="chart" viewBox="0 0 ${width} 60" role="img" aria-label="${esc(ariaLabel)}"><text class="empty" x="${width / 2}" y="34" text-anchor="middle">–</text></svg>`;
  const max = Math.max(1e-9, ...rows.map((r) => r.value || 0));
  const lw = Math.min(130, Math.max(70, Math.max(...rows.map((r) => String(r.name).length)) * 6.2)), h = rows.length * rowH + 4, tw = 92;
  const bars = rows.map((r, i) => {
    const bw = Math.max(2, ((r.value || 0) / max) * (width - lw - tw - 8));
    const desc = `${r.name}: ${fmt(r.value)} ${unit} (${pctFmt(r.pct)})`;
    return `<text class="tick" x="${lw - 8}" y="${i * rowH + 18}" text-anchor="end">${esc(r.name)}</text>`
      + `<rect class="hbar ${esc(r.cls || `s${(i % 6) + 1}`)}" role="img" aria-label="${esc(desc)}" x="${lw}" y="${i * rowH + 6}" width="${n1(bw)}" height="${rowH - 10}" rx="3"><title>${esc(desc)}</title></rect>`
      + `<text class="tick val" x="${n1(lw + bw + 6)}" y="${i * rowH + 18}">${esc(fmt(r.value))} · ${esc(pctFmt(r.pct))}</text>`;
  }).join('');
  return `<svg class="chart" viewBox="0 0 ${width} ${h}" role="group" aria-label="${esc(ariaLabel)}">${bars}</svg>`;
}
