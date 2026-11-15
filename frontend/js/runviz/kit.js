// Shared pieces of the wave-3 run detail cards (addendum 9.15): the card shell, a titled chart panel that is drawn at its real width, the cache of
// per-run fetches, and the "loading / failed / not available" notes. Everything from data goes in as text (h() / svg()); no innerHTML.
import { h } from '../dom.js';
import { t } from '../i18n.js';
import { describeError } from '../errors.js';
import { btn, skeleton } from '../components/ui.js';
import { fitted } from '../charts/fit.js';
import { chartEmpty, repaint } from '../charts/parts.js';
export { fetchCache } from '../fetch-cache.js';

/** A card: <section class="card chart-card rt-chart [wide]"> with the title, an optional hint under it, then `body`. */
export function vizCard({ title, hint = null, wide = false, cls = '', id }, ...body) {
  return h(
    'section',
    { class: `card chart-card rt-chart rv-card${wide ? ' wide' : ''}${cls ? ` ${cls}` : ''}`, 'aria-label': title, id },
    h('header', { class: 'card-head' }, h('h3', title)),
    h('div', { class: 'card-body' }, hint ? h('p', { class: 'hint' }, hint) : null, ...body),
  );
}

/** A chart with a small title above it, drawn at the host's real width by `build(width)`; `empty` (a node) when build returns null. */
export function chartPanel(scope, { title, build, empty, legend = null, sub = null }) {
  return h('div', { class: 'rv-panel' }, title ? h('h4', { class: 'sc-sub' }, title) : null, sub, fitted(scope, build, empty), legend);
}

export const note = (text, kind = 'info') => h('p', { class: `note ${kind}`, role: 'status' }, text);

/** A one-line "loading" placeholder for a card whose data is on its way. */
export const loadingBlock = () => skeleton(1);

/** A failed fetch: the message (never raw markup) and a Retry button. 404 says the backend lacks the route or the run. */
export function failBlock(err, retry) {
  return h(
    'div',
    { class: 'rv-fail' },
    h('p', { class: 'note warn', role: 'status' }, err?.status === 404 ? t('rv.not_found') : describeError(err)),
    retry ? btn(t('common.retry'), { size: 'sm', icon: 'restart', onClick: retry }) : null,
  );
}

/** The id the routes take: the event's `run_id` field (a row id can carry a `#2` suffix when an id was seen twice). */
export const routeId = (row) => String(row?.fields?.run_id ?? row?.fields?.job_id ?? row?.id ?? '');
export const enc = encodeURIComponent;
export { chartEmpty };

/**
 * Replace the children of `host` with `build()` keeping the <details> the user opened open (by position) and the focus on the control that had it. A control
 * that must keep its focus across a redraw carries `data-rv-focus="<id>"`.
 */
export function repaintKeepFocus(host, build) {
  const focus = document.activeElement?.dataset?.rvFocus;
  repaint(host, build);
  if (focus) host.querySelector(`[data-rv-focus="${CSS.escape(focus)}"]`)?.focus();
}

/** A row of toggle buttons (one pressed): the same look as the seg-control of the other pages, each button keeping its focus across a redraw. */
export function pickerBar({ label, values, current, labelOf, onPick, id }) {
  return h(
    'div',
    { class: 'seg-control', role: 'group', 'aria-label': label },
    values.map((v) => {
      const b = h('button', { type: 'button', 'aria-pressed': String(current === v), dataset: { rvFocus: `${id}-${v}` } }, labelOf(v));
      b.addEventListener('click', () => onPick(v));
      return b;
    }),
  );
}
