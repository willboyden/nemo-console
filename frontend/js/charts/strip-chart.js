// The up / down strip (addendum 9.16): a row of cells over a window, tall = down, a low bar = up, two thirds = partial, an outlined hatched cell = no data (the system or the console was not
// sampled: never drawn as up or down). The height carries the state as well as the colour. The root has an aria-label that sums the strip up; every cell has a <title> for hover. Geometry: viz/strip.js.
import { svg, titled } from '../viz/svg.js';
import { stripLayout } from '../viz/strip.js';
import { fmtStamp } from '../viz/fmt.js';
import { t } from '../i18n.js';

let uid = 0;

/**
 * @param opts.cells  strip-model cells [{ts, binS, state, share}];  @param opts.ariaLabel the summary;  @param opts.axis false for a compact strip with no time ticks (the Overview tile)
 * Returns an <svg>, or null when there are no cells.
 */
export function stripChart({ cells, width = 560, height = 30, ariaLabel, axis = true }) {
  if (!cells?.length) return null;
  const g = stripLayout({ cells, width, height, axis: axis ? 18 : 2 });
  const pid = `st-none-${++uid}`;
  const nodes = [
    svg(
      'defs',
      svg(
        'pattern',
        { id: pid, width: 5, height: 5, patternUnits: 'userSpaceOnUse', patternTransform: 'rotate(45)' },
        svg('rect', { class: 'heat-none-bg', width: 5, height: 5 }),
        svg('line', { class: 'heat-none-line', x1: 0, y1: 0, x2: 0, y2: 5 }),
      ),
    ),
    svg('line', { class: 'st-base', x1: g.x0, x2: g.x1, y1: height, y2: height }),
  ];
  g.bars.forEach((b, i) => {
    const c = cells[i];
    const what = t(`strip.state_${c.state}`);
    const tip = c.state === 'part' && c.share != null ? `${what} (${Math.round(c.share * 100)}%)` : what;
    nodes.push(
      titled(
        svg('rect', { class: `st-cell st-${c.state}`, x: b.x, y: b.y, width: b.w, height: b.h, fill: c.state === 'none' ? `url(#${pid})` : null, 'aria-hidden': 'true' }),
        `${fmtStamp(c.ts)} UTC · ${tip}`,
      ),
    );
  });
  if (axis) for (const tk of g.ticks) nodes.push(svg('text', { class: 'tick', x: tk.x, y: g.height - 4, 'text-anchor': 'middle' }, tk.label));
  return svg('svg', { class: 'chart rtchart stripchart', viewBox: `0 0 ${g.width} ${g.height}`, role: 'img', 'aria-label': ariaLabel }, svg('title', ariaLabel), ...nodes);
}

/** The legend keys of the strip: a swatch of the cell's own height, the colour and the word. */
export const STRIP_KEYS = ['up', 'part', 'down', 'none'];
