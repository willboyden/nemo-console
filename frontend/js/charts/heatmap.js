// A rows x time heatmap of values (addendum 9.14): the fleet's GPUs over time, the call rate of each model over time. One colour ramp
// (HEAT_LEVELS steps, a luminance ramp, so it is not colour-only), a hatched cell for NO DATA (a gap is never drawn as the lowest
// step), a legend with the scale, a readout line that shows the value and time under the pointer (and under the keyboard cursor:
// focus a row, then Left / Right), and each row is an element with an aria-label that sums it up. Geometry: viz/heat.js (pure,
// unit-tested). Runs of equal cells are drawn as one rect, so a 64 x 160 matrix is a few hundred elements, not ten thousand.
import { h } from '../dom.js';
import { svg } from '../viz/svg.js';
import { t } from '../i18n.js';
import { levelOf, levelStart, runLengths, matrixLayout, HEAT_LEVELS } from '../viz/heat.js';
import { clockTicks } from '../viz/scale.js';
import { fmtStamp } from '../viz/fmt.js';
import { arrayStats } from '../series-math.js';

let uid = 0;
const clamp = (v, lo, hi) => Math.min(hi, Math.max(lo, v));

/** The legend: the ramp as swatches with the scale's ends and middle, and the "no data" swatch. `fmt(v)` formats a domain value. */
export function heatLegend({ domain, fmt, noData = true }) {
  const mid = domain.curve === 'sqrt' ? levelStart(Math.floor(HEAT_LEVELS / 2), domain) : (domain.min + domain.max) / 2;
  return h(
    'div',
    { class: 'heat-legend', role: 'group', 'aria-label': t('heat.legend', { min: fmt(domain.min), max: fmt(domain.max) }) },
    h('span', { class: 'muted small' }, fmt(domain.min)),
    h(
      'span',
      { class: 'heat-ramp', 'aria-hidden': 'true' },
      Array.from({ length: HEAT_LEVELS }, (_, i) => h('i', { class: `heat-sw heatc fh${i}` })),
    ),
    h('span', { class: 'muted small' }, fmt(domain.max)),
    domain.curve === 'sqrt' ? h('span', { class: 'muted small' }, t('heat.sqrt_note')) : h('span', { class: 'muted small' }, t('heat.mid', { value: fmt(mid) })),
    noData ? h('span', { class: 'heat-nodata-key' }, h('i', { class: 'heat-sw heat-none-sw', 'aria-hidden': 'true' }), h('span', { class: 'muted small' }, t('heat.no_data'))) : null,
  );
}

/** One line for a row, for its aria-label and the readout when the row is focused: "latest 71%, average 63%, highest 98%" or "no data". */
function rowSummary(row, fmt) {
  const s = row.stats || arrayStats(row.values);
  return s.n ? t('heat.row_stats', { latest: fmt(s.latest), avg: fmt(s.avg), max: fmt(s.max) }) : t('heat.row_none');
}

/**
 * @param scope   a Scope (the ResizeObserver is released with it)
 * @param opts.build   () => the current model {rows: [{id, label, group?, down?, values, stats?}], ts, bucketS}, null when there is nothing to draw
 * @param opts.domain  () => {min, max, curve}
 * @param opts.fmt     () => (v) => text
 * @param opts.ariaLabel  () => the chart's summary
 * Returns {el, redraw()}: call redraw() after the model changed.
 */
export function matrixHeatmap(scope, { build, domain, fmt, ariaLabel, cellH = 18 }) {
  const readout = h('p', { class: 'heat-readout muted small', role: 'status' }, t('heat.hover_hint'));
  const wrap = h('div', { class: 'chart-wrap-lg heat-wrap' });
  const legendHost = h('div');
  const el = h('div', { class: 'heatmap' }, readout, wrap, legendHost);
  let model = null,
    layout = null,
    cursor = null,
    kb = { row: -1, col: -1 };

  function show(r, c) {
    if (!model || r < 0 || r >= model.rows.length) return;
    const row = model.rows[r];
    const f = fmt();
    if (c < 0) {
      readout.textContent = `${row.label} · ${rowSummary(row, f)}`;
      cursor?.setAttribute('visibility', 'hidden');
      return;
    }
    const v = row.values[c];
    const when = model.ts[c] != null ? `${fmtStamp(model.ts[c])} UTC` : '–';
    readout.textContent = `${row.label} · ${when} · ${v == null ? t('heat.no_data') : f(v)}${row.down ? ` · ${t('heat.host_down')}` : ''}`;
    if (cursor && layout) {
      cursor.setAttribute('x', String(layout.colX(c)));
      cursor.setAttribute('y', String(layout.rowY[r]));
      cursor.setAttribute('width', String(Math.max(2, layout.cw)));
      cursor.setAttribute('height', String(layout.cellH));
      cursor.setAttribute('visibility', 'visible');
    }
  }

  function draw() {
    model = build();
    const width = Math.max(300, Math.round(wrap.clientWidth || 640));
    if (!model || !model.rows.length || !model.ts.length) {
      wrap.replaceChildren();
      legendHost.replaceChildren();
      return;
    }
    const f = fmt(),
      d = domain();
    const rows = model.rows;
    const labelW = clamp(Math.max(...rows.map((r) => r.label.length)) * 6.4 + 14, 90, 210);
    const rowGap = rows.map((r, i) => (i > 0 && r.group !== rows[i - 1].group ? 8 : 0));
    layout = matrixLayout({ rows: rows.length, cols: model.ts.length, width, labelW, cellH, rowGap, top: 4, bottom: 22 });
    const pid = `heat-nodata-${++uid}`;
    const nodes = [
      svg(
        'defs',
        svg(
          'pattern',
          { id: pid, width: 6, height: 6, patternUnits: 'userSpaceOnUse', patternTransform: 'rotate(45)' },
          svg('rect', { class: 'heat-none-bg', width: 6, height: 6 }),
          svg('line', { class: 'heat-none-line', x1: 0, y1: 0, x2: 0, y2: 6 }),
        ),
      ),
    ];
    rows.forEach((row, r) => {
      const y = layout.rowY[r];
      const levels = row.values.map((v) => levelOf(v, d));
      const cells = runLengths(levels).map((run) =>
        svg('rect', {
          class: run.level == null ? 'heat-none' : `heatc fh${run.level}`,
          fill: run.level == null ? `url(#${pid})` : null,
          x: layout.colX(run.c0).toFixed(1),
          y,
          width: Math.max(0.5, (run.c1 - run.c0) * layout.cw - (layout.cw > 3 ? 0.5 : 0)).toFixed(1),
          height: cellH - 1,
        }),
      );
      const label = `${row.label}: ${rowSummary(row, f)}${row.down ? `. ${t('heat.host_down')}` : ''}`;
      const g = svg(
        'g',
        { class: `heat-row${row.down ? ' down' : ''}`, role: 'img', tabindex: 0, 'aria-label': label, 'data-row': r },
        svg('text', { class: `tick heat-label${row.down ? ' down' : ''}`, x: layout.x0 - 6, y: y + cellH / 2 + 4, 'text-anchor': 'end' }, row.label),
        ...cells,
      );
      g.addEventListener('focus', () => {
        kb = { row: r, col: -1 };
        show(r, -1);
      });
      nodes.push(g);
    });
    const t0 = model.ts[0],
      t1 = model.ts[model.ts.length - 1] + model.bucketS;
    const xOf = (v) => layout.x0 + ((v - t0) / Math.max(1, t1 - t0)) * (layout.x1 - layout.x0);
    for (const tk of clockTicks(t0, t1, width < 480 ? 4 : 6)) nodes.push(svg('text', { class: 'tick', x: xOf(tk.v), y: layout.height - 6, 'text-anchor': 'middle' }, tk.label));
    cursor = svg('rect', { class: 'heat-cursor', visibility: 'hidden', x: 0, y: 0, width: 1, height: 1, 'pointer-events': 'none' });
    nodes.push(cursor);
    const summary = ariaLabel();
    const root = svg('svg', { class: 'chart rtchart heatchart', viewBox: `0 0 ${width} ${layout.height}`, role: 'group', 'aria-label': summary }, svg('title', summary), ...nodes);
    const at = (e) => {
      const box = root.getBoundingClientRect();
      const k = width / Math.max(1, box.width);
      return [(e.clientX - box.left) * k, (e.clientY - box.top) * k];
    };
    root.addEventListener('pointermove', (e) => {
      const [x, y] = at(e);
      const r = layout.rowAt(y),
        c = layout.colAt(x);
      if (r >= 0 && c >= 0) show(r, c);
    });
    root.addEventListener('pointerleave', () => cursor?.setAttribute('visibility', 'hidden'));
    root.addEventListener('keydown', (e) => {
      if (kb.row < 0 || (e.key !== 'ArrowLeft' && e.key !== 'ArrowRight')) return;
      e.preventDefault();
      const cols = model.ts.length;
      kb.col = kb.col < 0 ? (e.key === 'ArrowLeft' ? cols - 1 : 0) : clamp(kb.col + (e.key === 'ArrowLeft' ? -1 : 1), 0, cols - 1);
      show(kb.row, kb.col);
    });
    wrap.replaceChildren(root);
    legendHost.replaceChildren(heatLegend({ domain: d, fmt: f }));
  }

  let lastW = 0;
  if (typeof ResizeObserver !== 'undefined') {
    const ro = new ResizeObserver(() => {
      const w = Math.round(wrap.clientWidth);
      if (w < 80 || Math.abs(w - lastW) < 6) return;
      lastW = w;
      if (model) draw();
    });
    ro.observe(wrap);
    scope.own(() => ro.disconnect());
  }
  return {
    el,
    redraw() {
      readout.textContent = t('heat.hover_hint');
      draw();
    },
  };
}
