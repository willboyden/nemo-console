// A sparkline (addendum 9.16): a small line of one metric over the last hour, no axes. It carries the figures in words (the aria-label and <title>: latest, lowest, highest, and the window),
// so the line is decorative. A missing sample is a gap in the line (the pen lifts), never a dive to 0. Positions only; geometry is plain arithmetic.
import { svg } from '../viz/svg.js';
import { pathWithGaps } from '../viz/series.js';

const isNum = (n) => typeof n === 'number' && Number.isFinite(n);

/**
 * @param opts.values  numbers or null per slot, oldest first;  @param opts.ariaLabel the words;  @param opts.width,height pixels
 * Returns an <svg>, or null when there is no sample at all.
 */
export function sparkline({ values, width = 160, height = 34, ariaLabel }) {
  const have = values.filter(isNum);
  if (!have.length) return null;
  const pad = 3;
  const lo = Math.min(...have),
    hi = Math.max(...have);
  const span = hi - lo;
  const n = values.length;
  const x = (i) => pad + (n > 1 ? (i / (n - 1)) * (width - pad * 2) : (width - pad * 2) / 2);
  const y = (v) => (span > 0 ? height - pad - ((v - lo) / span) * (height - pad * 2) : height / 2);
  const pts = values.map((v, i) => (isNum(v) ? { x: x(i), y: y(v) } : null));
  const d = pathWithGaps(pts);
  const lastI = values.reduce((acc, v, i) => (isNum(v) ? i : acc), -1);
  const root = svg(
    'svg',
    { class: 'chart spark', viewBox: `0 0 ${width} ${height}`, width, height, role: 'img', 'aria-label': ariaLabel },
    svg('title', ariaLabel),
    svg('line', { class: 'spark-base', x1: pad, x2: width - pad, y1: height - pad, y2: height - pad }),
    d ? svg('path', { class: 'spark-line', d }) : null,
    svg('circle', { class: 'spark-dot', cx: x(lastI), cy: y(values[lastI]), r: 2.6 }),
  );
  return root;
}
