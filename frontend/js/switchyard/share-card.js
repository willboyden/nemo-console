// A card of horizontal share bars with a data table: the Routing tab's by-tier / by-route / by-model cards, and the flow tab's client
// outcomes. `data` is {name: count} (or anything share.js accepts); the SVG comes from charts.js, which escapes every name.
import { h, setTrustedHtml } from '../dom.js';
import { t } from '../i18n.js';
import { fmtInt, fmtPct } from '../format.js';
import { roundedShares, topN } from '../share.js';
import { shareBars } from '../charts.js';
import { card, emptyBox } from '../components/ui.js';

export function shareCard(title, data, unit, { note = null } = {}) {
  const rows = topN(
    roundedShares(data).map((r) => ({ ...r, pct: r.pct })),
    8,
  );
  const wrap = h('div');
  setTrustedHtml(wrap, shareBars(rows, { unit, ariaLabel: t('routing.share_aria', { title }), pctFmt: (p) => fmtPct(p, 1), fmt: fmtInt }));
  return card(
    title,
    rows.length
      ? h(
          'div',
          {},
          note ? h('p', { class: 'muted small' }, note) : null,
          wrap,
          h(
            'details',
            { class: 'chart-data' },
            h('summary', t('chart.data_table')),
            h(
              'div',
              { class: 'tablewrap' },
              h(
                'table',
                { class: 'compact' },
                h(
                  'thead',
                  h(
                    'tr',
                    h('th', { scope: 'col' }, t('routing.name')),
                    h('th', { scope: 'col', class: 'al-right' }, t('routing.count')),
                    h('th', { scope: 'col', class: 'al-right' }, t('routing.share')),
                  ),
                ),
                h(
                  'tbody',
                  rows.map((r) => h('tr', h('td', r.name), h('td', { class: 'num al-right' }, fmtInt(r.value)), h('td', { class: 'num al-right' }, fmtPct(r.pct, 1)))),
                ),
              ),
            ),
          ),
        )
      : emptyBox(t('routing.no_data')),
  );
}
