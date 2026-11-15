// The request timeline of an AIPerf run as swim lanes (addendum 9.15): a lane per worker, a bar per request from its start to its end; a request that failed
// is drawn in the error colour AND with a taller, hatched look (the colour is not the only cue); the legend and the table say which is which. A run has thousands
// of requests, so a bar carries a <title> (hover) while the LANE is the element with the aria-label (its request count, its failures, its busiest span): a screen
// reader gets one line per worker, not five thousand. Geometry: viz/bars.js swimLayout (pure, unit-tested).
import { svg, titled } from '../viz/svg.js';
import { swimLayout } from '../viz/bars.js';
import { fmtOffsetMs } from '../viz/scale.js';
import { fmtMs } from '../format.js';
import { t } from '../i18n.js';

/**
 * @param lanes [{id, label, n, errors}];  @param items [{i, lane, start, end, error, latency, code}] (ms)
 * Returns an <svg>, or null when there is nothing to draw.
 */
export function swimChart({ lanes, items, width = 560, ariaLabel }) {
  const g = swimLayout({ lanes, items, width });
  if (g.empty) return null;
  const nodes = [];
  for (const tk of g.ticks)
    nodes.push(svg('line', { class: 'gl v', x1: tk.x, x2: tk.x, y1: 0, y2: g.height - 20 }), svg('text', { class: 'tick', x: tk.x, y: g.height - 6, 'text-anchor': 'middle' }, tk.label));
  const byLane = new Map();
  for (const b of g.bars) {
    if (!byLane.has(b.lane)) byLane.set(b.lane, []);
    byLane.get(b.lane).push(b);
  }
  const src = new Map(items.map((it) => [it.i, it]));
  g.lanes.forEach((ln, li) => {
    const meta = lanes[li];
    const aria = t('viz.lane_label', { worker: meta.label, n: meta.n, errors: meta.errors });
    const grp = svg('g', { class: 'sw-lane', role: 'img', 'aria-label': aria });
    grp.append(
      svg('rect', { class: 'tl-lane', x: g.x0, y: ln.y - 2, width: g.x1 - g.x0, height: ln.h + 4, rx: 2 }),
      titled(svg('text', { class: 'tick', x: g.x0 - 8, y: ln.y + ln.h / 2 + 4, 'text-anchor': 'end' }, `${t('viz.worker')} ${meta.label}`), aria),
    );
    for (const b of byLane.get(li) || []) {
      const it = src.get(b.i);
      const tip = it.error
        ? t(it.code === 0 ? 'viz.req_error_nocode' : 'viz.req_error', { i: b.i, code: it.code, start: fmtOffsetMs(b.start) })
        : t('viz.req_ok', { i: b.i, latency: it.latency == null ? '–' : fmtMs(it.latency), start: fmtOffsetMs(b.start) });
      grp.append(titled(svg('rect', { class: it.error ? 'sw-bar sw-err' : 'sw-bar sw-ok', x: b.x, y: b.y, width: b.w, height: b.h }), tip));
    }
    nodes.push(grp);
  });
  return svg('svg', { class: 'chart rtchart swimchart', viewBox: `0 0 ${width} ${g.height}`, role: 'group', 'aria-label': ariaLabel }, svg('title', ariaLabel), ...nodes);
}
