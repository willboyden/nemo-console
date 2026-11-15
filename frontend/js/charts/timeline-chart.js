// The XID and diagnostics swim lane (addendum 9.14): a lane per GPU, a mark per event, click (or Enter / Space) = details. The shape and
// the glyph carry the meaning as well as the colour: an XID is a diamond with "!", a diagnostic a circle with ✓ (pass), ! (warn) or
// ✕ (fail / error). Every mark has an aria-label and a <title>; the page lists the same events in a table. Geometry: viz/timeline.js.
import { svg, titled } from '../viz/svg.js';
import { timelineLayout } from '../viz/timeline.js';
import { fmtStamp } from '../viz/fmt.js';
import { t } from '../i18n.js';

const GLYPH = { ok: '✓', warn: '!', bad: '✕', muted: '?' };

/**
 * @param model   {items: [{id, ts, kind, gpu, severity, summary}], lanes: [gpu ids]} from dcgm-model.timelineModel
 * @param opts    {from, to, width, selected (an item id), onPick(item), ariaLabel}
 * Returns an <svg>, or null when there is no lane.
 */
export function timelineChart(model, { from, to, width = 640, selected = null, onPick, ariaLabel }) {
  if (!model.lanes.length) return null;
  const g = timelineLayout({ items: model.items.map((i) => ({ ...i, lane: i.gpu })), lanes: model.lanes, from, to, width });
  const nodes = [];
  model.lanes.forEach((lane, i) => {
    const y = g.laneY(i);
    nodes.push(
      svg('rect', { class: 'tl-lane', x: g.plot.x0, y: y + 2, width: g.plot.x1 - g.plot.x0, height: g.laneH - 4, rx: 3 }),
      svg('text', { class: 'tick tl-label', x: g.plot.x0 - 8, y: y + g.laneH / 2 + 4, 'text-anchor': 'end' }, lane === '?' ? t('gpu.tl.unknown_gpu') : t('gpu.tl.lane', { gpu: lane })),
    );
  });
  for (const tk of g.ticks) {
    nodes.push(svg('line', { class: 'gl', x1: tk.x, x2: tk.x, y1: g.laneY(0), y2: g.height - 20 }), svg('text', { class: 'tick', x: tk.x, y: g.height - 6, 'text-anchor': 'middle' }, tk.label));
  }
  for (const m of g.marks) {
    const it = m.item;
    const label = `${it.kind === 'xid' ? t('gpu.tl.xid') : t('gpu.tl.diag')}: ${it.summary} · ${fmtStamp(it.ts)} UTC`;
    const shape =
      it.kind === 'xid'
        ? svg('path', { class: `tl-shape ${it.severity}`, d: `M${m.x} ${m.y - 8}L${m.x + 8} ${m.y}L${m.x} ${m.y + 8}L${m.x - 8} ${m.y}Z` })
        : svg('circle', { class: `tl-shape ${it.severity}`, cx: m.x, cy: m.y, r: 7.5 });
    const mark = svg(
      'g',
      { class: `tl-mark${selected === it.id ? ' selected' : ''}`, role: 'button', tabindex: 0, 'aria-label': label, 'aria-pressed': String(selected === it.id) },
      shape,
      svg('text', { class: 'tl-glyph', x: m.x, y: m.y + 3.6, 'text-anchor': 'middle' }, it.kind === 'xid' ? '!' : GLYPH[it.severity]),
    );
    titled(mark, label);
    mark.addEventListener('click', () => onPick?.(it));
    mark.addEventListener('keydown', (e) => {
      if (e.key === 'Enter' || e.key === ' ') {
        e.preventDefault();
        onPick?.(it);
      }
    });
    nodes.push(mark);
  }
  return svg('svg', { class: 'chart rtchart tlchart', viewBox: `0 0 ${width} ${g.height}`, role: 'group', 'aria-label': ariaLabel }, svg('title', ariaLabel), ...nodes);
}
