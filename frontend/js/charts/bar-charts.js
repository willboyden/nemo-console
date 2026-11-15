// SVG bar charts of the wave-3 pages (addendum 9.15): horizontal bar rows with optional interval whiskers (confidence intervals, run durations,
// sizes, error codes, rubric scores), a dumbbell (with vs without a skill) and grouped columns (the reward distribution). Geometry: viz/bars.js (pure,
// unit-tested); these only turn it into elements. Every bar is an element with an aria-label and a <title>; a value is also printed as text; classes
// (hb-*, bf1.., db-*) are styled in components.css; widths and positions are SVG attributes (no style attributes). Everything from data goes in as text.
import { svg, titled } from '../viz/svg.js';
import { hbarLayout, dumbbellLayout, columnLayout, fitTicks } from '../viz/bars.js';
import { t } from '../i18n.js';

const frame = (w, h, label, nodes, cls) => svg('svg', { class: `chart rtchart ${cls}`, viewBox: `0 0 ${w} ${h}`, role: 'group', 'aria-label': label }, svg('title', label), ...nodes);
const clipText = (s, n) => (String(s).length > n ? `${String(s).slice(0, n - 1)}…` : String(s));
const press = (el, fn) => {
  el.addEventListener('click', fn);
  el.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      fn();
    }
  });
};

/**
 * Horizontal bars. rows: [{id, label, bars: [{key, value|null, lo?, hi?, cls (bf1.. | hb-ok | hb-bad | hb-warn | hb-info | hb-muted, + hb-running ...), text (aria-label and tooltip), valueText?}]}].
 * A bar with no value draws nothing and prints its `valueText` (the verdict glyph beside a dash) or `missingText` (an en dash by default), never a zero-length bar that reads as zero.
 * opts: width, ariaLabel, axisFmt(v), domain {min, max}, onPick(id) (a click on a bar, Enter / Space on the label), selected {a: id, b: id} (rows marked A / B), labelW, tailW.
 * Returns an <svg>, or null when no bar has a value.
 */
export function hbarChart({ rows, width = 560, ariaLabel, axisFmt = (v) => String(v), domain = null, onPick, selected = {}, labelW, tailW = 78, missingText = '–', barH }) {
  const longest = Math.max(4, ...rows.map((r) => String(r.label).length));
  // the label column grows with the chart (a long label is cut with an ellipsis, its whole text is the tooltip)
  const cap = width < 420 ? 104 : width < 600 ? 150 : width < 800 ? 196 : 250;
  const lw = labelW ?? Math.min(cap, Math.max(56, longest * 6.4 + 22));
  const g = hbarLayout({ rows, width, labelW: lw, tailW, domain, barH: barH ?? (rows.some((r) => r.bars.length > 1) ? 11 : 14) });
  if (g.empty) return null;
  const maxChars = Math.floor((lw - 20) / 6.2);
  const nodes = [];
  for (const tk of g.ticks) nodes.push(svg('line', { class: 'gl v', x1: tk.x, x2: tk.x, y1: 0, y2: g.height - 20 }));
  for (const tk of fitTicks(g.ticks, axisFmt, { x0: g.x0, x1: g.x1 })) nodes.push(svg('text', { class: 'tick', x: tk.x, y: g.height - 6, 'text-anchor': tk.anchor }, axisFmt(tk.v)));
  nodes.push(svg('line', { class: 'hb-base', x1: g.x(Math.max(g.domain.min, Math.min(0, g.domain.max))), x2: g.x(Math.max(g.domain.min, Math.min(0, g.domain.max))), y1: 0, y2: g.height - 20 }));
  for (const r of g.rows) {
    const tag = selected.a === r.id ? 'A' : selected.b === r.id ? 'B' : null;
    const label = svg('g', {
      class: `hb-label${tag ? ` is-${tag.toLowerCase()}` : ''}${onPick ? ' hb-pick' : ''}`,
      ...(onPick ? { role: 'button', tabindex: 0, 'aria-label': t('viz.select_run', { run: r.label }) } : {}),
    });
    label.append(
      tag ? svg('text', { class: `hb-tag hb-tag-${tag.toLowerCase()}`, x: 2, y: r.mid + 4 }, tag) : null,
      titled(svg('text', { class: 'tick hb-name', x: lw - 8, y: r.mid + 4, 'text-anchor': 'end' }, clipText(r.label, maxChars)), r.label),
    );
    if (onPick) press(label, () => onPick(r.id));
    nodes.push(label);
    for (const b of r.bars) {
      if (b.missing) {
        nodes.push(titled(svg('text', { class: 'tick val muted-val', x: g.x0 + 4, y: b.y + b.h - 2, role: 'img', 'aria-label': b.text }, b.valueText ?? missingText), b.text));
        continue;
      }
      const rect = svg('rect', { class: `hb-bar ${b.cls || 'bf1'}${onPick ? ' hb-pick' : ''}`, x: b.x, y: b.y, width: b.w, height: b.h, rx: 2, role: 'img', 'aria-label': b.text });
      titled(rect, b.text);
      if (onPick) rect.addEventListener('click', () => onPick(r.id));
      nodes.push(rect);
      if (b.whisker) {
        const w = b.whisker;
        nodes.push(
          svg('line', { class: 'hb-whisker', x1: w.x1, x2: w.x2, y1: w.y, y2: w.y }),
          svg('line', { class: 'hb-whisker', x1: w.x1, x2: w.x1, y1: w.y - 3, y2: w.y + 3 }),
          svg('line', { class: 'hb-whisker', x1: w.x2, x2: w.x2, y1: w.y - 3, y2: w.y + 3 }),
        );
      }
      if (b.valueText) nodes.push(svg('text', { class: 'tick val', x: b.end + 6, y: b.y + b.h - 2 }, b.valueText));
    }
  }
  return frame(width, g.height, ariaLabel, nodes, 'hbchart');
}

/**
 * Dumbbell rows: a hollow circle for the value without the skill, a filled diamond for the value with it, a line between them coloured by the
 * direction (and a glyph: up = better, down = worse). rows: [{id, label, header?, a (without), b (with), delta, valueText?}]; `deltaFmt(delta)` words the change.
 * Every row is an element with an aria-label. Returns an <svg>, or null when no row has a value.
 */
export function dumbbellChart({ rows, width = 560, ariaLabel, axisFmt = (v) => String(v), valueFmt = axisFmt, deltaFmt = (d) => String(d), labelW }) {
  const longest = Math.max(4, ...rows.filter((r) => !r.header).map((r) => String(r.label).length));
  const lw = labelW ?? Math.min(width < 420 ? 96 : 150, Math.max(56, Math.min(longest, width < 420 ? 14 : 22) * 6.4 + 22));
  const g = dumbbellLayout({ rows, width, labelW: lw, tailW: width < 420 ? 70 : 96 });
  if (g.empty) return null;
  const maxChars = Math.floor((lw - 14) / 6.2);
  const nodes = [];
  for (const tk of g.ticks) nodes.push(svg('line', { class: 'gl v', x1: tk.x, x2: tk.x, y1: 0, y2: g.height - 20 }));
  for (const tk of fitTicks(g.ticks, axisFmt, { x0: g.x0, x1: g.x1 })) nodes.push(svg('text', { class: 'tick', x: tk.x, y: g.height - 6, 'text-anchor': tk.anchor }, axisFmt(tk.v)));
  for (const r of g.rows) {
    const src = rows.find((x) => x.id === r.id);
    if (r.header) {
      nodes.push(
        svg('rect', { class: 'db-head', x: 0, y: r.y + 2, width: g.width, height: 22, rx: 3 }),
        titled(svg('text', { class: 'db-head-text', x: 8, y: r.cy + 4 }, clipText(r.label, Math.floor(g.width / 7))), r.label),
      );
      continue;
    }
    const aria = t('viz.db_row', { name: r.label ?? '', without: r.a == null ? '–' : valueFmt(src.a), with: r.b == null ? '–' : valueFmt(src.b), delta: r.delta == null ? '–' : deltaFmt(r.delta) });
    const grp = svg('g', { class: `db-row db-${r.dir ?? 'none'}`, role: 'img', 'aria-label': aria });
    titled(grp, aria);
    grp.append(svg('text', { class: 'tick db-name', x: lw - 8, y: r.cy + 4, 'text-anchor': 'end' }, clipText(r.label, maxChars)));
    if (r.a != null && r.b != null) grp.append(svg('line', { class: 'db-line', x1: r.a, x2: r.b, y1: r.cy, y2: r.cy }));
    if (r.a != null) grp.append(svg('circle', { class: 'db-without', cx: r.a, cy: r.cy, r: 5.5 }));
    if (r.b != null) grp.append(svg('path', { class: 'db-with', d: `M${r.b} ${r.cy - 7}L${r.b + 6} ${r.cy}L${r.b} ${r.cy + 7}L${r.b - 6} ${r.cy}Z` }));
    const glyph = r.dir === 'up' ? '▲' : r.dir === 'down' ? '▼' : r.dir === 'flat' ? '=' : '';
    grp.append(svg('text', { class: 'tick val db-delta', x: g.x1 + 14, y: r.cy + 4 }, r.delta == null ? '–' : `${glyph} ${deltaFmt(r.delta)}`));
    nodes.push(grp);
  }
  return frame(width, g.height, ariaLabel, nodes, 'dbchart');
}

/**
 * Grouped columns: one group per category, a column per series (A and B side by side). cats: [label]; series: [{key, name, cls, values}].
 * Every column is an element with an aria-label ("0.9–1.0 · A: 4 tasks"). Returns an <svg>, or null when every value is 0 / missing.
 */
export function columnChart({ cats, series, width = 560, height = 190, ariaLabel, countText = (n) => String(n), yTitle = null }) {
  const g = columnLayout({ cats, series, width, height });
  if (g.empty) return null;
  const nodes = [];
  for (const tk of g.yTicks)
    nodes.push(svg('line', { class: 'gl', x1: g.plot.x0, x2: g.plot.x1, y1: tk.y, y2: tk.y }), svg('text', { class: 'tick', x: g.plot.x0 - 6, y: tk.y + 4, 'text-anchor': 'end' }, String(tk.v)));
  const thin = cats.length > 6 && g.plot.w / cats.length < 44;
  for (const grp of g.groups) {
    for (const c of grp.cols) {
      const s = series[c.si];
      const text = `${cats[grp.i]} · ${s.name}: ${countText(c.v)}`;
      nodes.push(titled(svg('rect', { class: `${s.cls} cc-col${c.v === 0 ? ' cc-zero' : ''}`, x: c.x, y: c.y, width: c.w, height: c.h, role: 'img', 'aria-label': text }), text));
    }
    // a long label is turned; an odd one is dropped when the groups are narrow
    if (thin && grp.i % 2) continue;
    nodes.push(svg('text', { class: 'tick', x: grp.x + grp.w / 2, y: height - 20, 'text-anchor': 'middle' }, cats[grp.i]));
  }
  if (yTitle) nodes.push(svg('text', { class: 'axis-title', x: 10, y: (g.plot.y0 + g.plot.y1) / 2, 'text-anchor': 'middle', transform: `rotate(-90 10 ${(g.plot.y0 + g.plot.y1) / 2})` }, yTitle));
  return frame(width, height, ariaLabel, nodes, 'colchart');
}
