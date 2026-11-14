// The reusable chart card: a titled card whose SVG is drawn at the host's real width (charts/fit.js), with an optional
// legend, extra controls and a text-alternative table. Moved out of js/rt/analytics-tab.js (which now calls it with its
// own per-render scope) so any view can build one.
import { h } from '../dom.js';
import { fitted } from './fit.js';
import { legendList, chartEmpty } from './parts.js';

/**
 * @param scope  a Scope: charts redrawn on resize are released with it
 * @param build  (width) => SVG node, or null when there is nothing to draw (then `empty` is shown)
 * Returns a <section class="card chart-card rt-chart">.
 */
export function chartCard(scope, { title, hint, build, legend, table, empty, wide = false, id, extra }) {
  const chart = fitted(scope, build, null);
  return h(
    'section',
    { class: `card chart-card rt-chart${wide ? ' wide' : ''}`, id, 'aria-label': title },
    h('header', { class: 'card-head' }, h('h3', title)),
    h(
      'div',
      { class: 'card-body' },
      hint ? h('p', { class: 'hint' }, hint) : null,
      chart ?? chartEmpty(empty),
      chart && legend ? legendList(legend) : null,
      chart ? extra : null,
      chart && table ? table : null,
    ),
  );
}
