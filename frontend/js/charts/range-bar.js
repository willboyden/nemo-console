// A tiny range bar for a table cell (addendum 9.15, the Data Designer column profile): a track from the column's minimum to its maximum with a mark at the
// median and one at the 95th percentile. It is decorative and also carries an aria-label with the four figures, which are printed beside it as text.
// Positions go in through the CSSOM (a style attribute would break the CSP). A missing figure has no mark; no range at all is an en dash.
import { h } from '../dom.js';

const isNum = (n) => typeof n === 'number' && Number.isFinite(n);

/** @param r {min, p50, p95, max} (any may be null);  @param label the aria-label (the caller words it and formats the figures) */
export function rangeBar(r, label) {
  if (!r || !isNum(r.min) || !isNum(r.max)) return h('span', { class: 'muted' }, '–');
  const span = r.max - r.min;
  const at = (v) => (span > 0 ? Math.min(100, Math.max(0, ((v - r.min) / span) * 100)) : 50);
  const track = h('span', { class: 'rbar', role: 'img', 'aria-label': label }, h('i', { class: 'rb-line' }));
  const mark = (v, cls) => {
    if (!isNum(v)) return;
    const m = h('i', { class: `rb-mark ${cls}` });
    m.style.left = `${at(v).toFixed(1)}%`;
    track.append(m);
  };
  mark(r.min, 'rb-end');
  mark(r.max, 'rb-end');
  mark(r.p95, 'rb-p95');
  mark(r.p50, 'rb-p50');
  return track;
}
