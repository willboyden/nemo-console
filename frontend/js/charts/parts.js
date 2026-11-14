// Small presentational pieces every chart card uses: legend, text-alternative table, empty note, in-place swap.
// Moved out of js/rt/ui.js (which re-exports them) so any view can build a chart card. Everything is text via h().
import { h } from '../dom.js';
import { t } from '../i18n.js';
import { shareOf } from '../viz/fmt.js';

/** Legend with a swatch, a shape cue and the name: series are told apart by label, not colour alone. */
export function legendList(items) {
  return h(
    'ul',
    { class: 'legend' },
    items.map((it) => h('li', h('i', { class: `sw ${it.cls}${it.line ? ` ln-${it.line}` : ''}`, 'aria-hidden': 'true' }), h('span', it.label))),
  );
}

/** A collapsed table as the text alternative of a chart; rows are built when it is first opened. */
export function dataTable(title, columns, getRows) {
  const body = h('div', { class: 'tablewrap tall' });
  const d = h('details', { class: 'chart-data' }, h('summary', title ?? t('chart.data_table')), body);
  let built = false;
  d.addEventListener('toggle', () => {
    if (!d.open || built) return;
    built = true;
    const rows = getRows();
    body.append(
      h(
        'table',
        { class: 'compact' },
        h(
          'thead',
          h(
            'tr',
            columns.map((c, i) => h('th', { scope: 'col', class: i ? 'al-right' : '' }, c)),
          ),
        ),
        h(
          'tbody',
          rows.map((r) =>
            h(
              'tr',
              r.map((v, i) => h('td', { class: i ? 'num al-right' : '' }, v == null ? '–' : String(v))),
            ),
          ),
        ),
      ),
    );
  });
  return d;
}

export const chartEmpty = (msg = t('common.no_data')) => h('p', { class: 'chart-empty muted', role: 'note' }, msg);
export const dash = () => h('span', { class: 'muted' }, '–');

/** Replace the children of a host in one step (no flash between the old and new content). */
export const swap = (host, ...nodes) => {
  host.replaceChildren(...nodes.flat().filter(Boolean));
  return host;
};

/** A tiny proportional bar inside a table cell (decorative: the number next to it is the content). */
export function inlineBar(value, max, cls = '') {
  const bar = h('span', { class: `inbar ${cls}`, 'aria-hidden': 'true' });
  const i = h('i');
  i.style.width = `${(shareOf(value, max) * 100).toFixed(1)}%`;
  bar.append(i);
  return bar;
}

/** Shorten a string for a chip or a table cell. */
export const clip = (s, n = 64) => {
  const x = String(s ?? '');
  return x.length > n ? `${x.slice(0, n - 1)}…` : x;
};
