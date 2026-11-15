// The Overview's problem counts by severity across systems (addendum 9.16): the totals in words (a glyph and a count each, never colour alone) and a stacked bar per system that has a problem
// (errors, warnings, info on one scale), worst first. Updated in place on every overview poll. Counts only: the problems themselves (with their fix commands) stay on the tiles below.
import { h } from '../dom.js';
import { t } from '../i18n.js';
import { badge } from '../components/ui.js';
import { stackCard } from '../charts/stack-card.js';
import { swap } from '../charts/parts.js';
import { problemCounts } from '../overview-model.js';

const SEV = [
  ['error', 'bf-bad', '✕'],
  ['warn', 'bf3', '!'],
  ['info', 'bf1', 'i'],
];

/** Returns {el, update(systems)}. */
export function problemsCard(scope) {
  const totals = h('p', { class: 'ovw-problem-totals', role: 'status' });
  const bars = stackCard(scope, {
    title: t('ovw.problems_title'),
    hint: t('ovw.problems_hint'),
    parts: SEV.map(([k, cls]) => ({ key: k, name: t(`ovw.sev_${k}`), cls })),
    valueFmt: (v) => String(Math.round(v)),
    axisFmt: (v) => (Number.isInteger(v) ? String(v) : ''),
    emptyText: t('ovw.problems_none'),
    wide: true,
    labelMax: 150,
  });
  // the totals sit above the bars inside the same card
  const body = bars.el.querySelector('.card-body');
  body.insertBefore(totals, body.firstChild?.nextSibling ?? null);
  return {
    el: bars.el,
    update(systems) {
      const c = problemCounts(systems);
      swap(
        totals,
        ...SEV.flatMap(([k, , glyph]) => [badge(`${glyph} ${t(`ovw.count_${k}`, { count: c.totals[k] })}`, k === 'error' ? 'bad' : k === 'warn' ? 'warn' : 'info'), ' ']),
        h('span', { class: 'muted small' }, t('ovw.problems_across', { with: c.systems, total: c.systems + c.clean })),
      );
      bars.update({
        rows: c.rows.map((r) => ({ id: r.system, label: t(`sys.${r.system}`), values: { error: r.error, warn: r.warn, info: r.info }, totalText: String(r.total), note: null })),
        time: [],
      });
    },
  };
}
