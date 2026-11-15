// Small presentational pieces shared by the explorer tabs. Every string that came from a capture goes in as text.
import { h, copyText } from '../dom.js';
import { t } from '../i18n.js';
import { badge, btn, emptyBox } from '../components/ui.js';
import { toast } from '../components/toast.js';
import { tokenSegments, shareOf, fmtTok, fmtDur, categoryOf } from '../viz/fmt.js';
import { dash } from '../charts/parts.js';

// Chart parts shared with every other page live in js/charts/parts.js; the explorer keeps importing them from here.
export { legendList, dataTable, chartEmpty, dash, swap, inlineBar, clip } from '../charts/parts.js';
import { fmtInt } from '../format.js';

const STATUS_KIND = { ok: 'ok', error: 'bad', open: 'info' };
const STATUS_GLYPH = { ok: '✓', error: '✕', open: '◌' };
/** Status chip: colour AND a glyph AND the word, so it never relies on colour alone. */
export const statusChip = (status) =>
  badge(`${STATUS_GLYPH[status] ?? '?'} ${t(`rt.status.${status}`) === `rt.status.${status}` ? String(status) : t(`rt.status.${status}`)}`, STATUS_KIND[status] ?? 'muted');
export const kindChip = (kind) => (kind ? badge(String(kind), 'muted') : h('span', { class: 'muted' }, '–'));
/** A refused query (422): the server's own words, and a way out that does not repeat the same request. */
export const refusedBox = (e, onReset) => emptyBox(t('rt.filter.refused'), e?.message || '', btn(t('rt.filter.reset'), { size: 'sm', onClick: onReset }));
/** "in · out · cached" in a table cell: a single dash when the span carried no token counts at all. */
export const tripletShort = (input, output, cached) =>
  input == null && output == null && cached == null ? '–' : t('rt.tokens.short', { input: fmtTok(input), output: fmtTok(output), cached: fmtTok(cached) });

export function routingChip(r) {
  if (!r) return dash();
  const text = `${r.route ?? '?'} → ${r.decision ?? '?'}`;
  const tip = t('rt.routing.chip_tip', { route: r.route ?? '–', decision: r.decision ?? '–', model: r.served_model ?? '–', overhead: fmtDur(r.overhead_ms), errors: r.errors ?? 0 });
  return h('span', { class: `badge ${r.errors ? 'warn' : 'info'} route-chip`, title: tip }, r.errors ? '⚠ ' : '', text);
}
export function modelChips(models, max = 2) {
  const list = models || [];
  if (!list.length) return dash();
  return h(
    'span',
    { class: 'chips' },
    list.slice(0, max).map((m) => h('span', { class: 'chip' }, m)),
    list.length > max ? h('span', { class: 'chip more', title: list.slice(max).join(', ') }, `+${list.length - max}`) : null,
  );
}
const CAT_LABEL = { agent: 'rt.cat.agent', turn: 'rt.cat.turn', llm: 'rt.cat.llm', tool: 'rt.cat.tool', other: 'rt.cat.other' };
export const catChip = (span) => h('span', { class: `cat-chip cat-${categoryOf(span)}` }, t(CAT_LABEL[categoryOf(span)]));

/** A tiny stacked bar [uncached input | cached input | output]; the text alternative carries the numbers. */
export function tokenBar(input, output, cached) {
  const seg = tokenSegments(input, output, cached);
  const label = seg.total ? t('rt.tokens.bar', { input: fmtInt(input ?? 0), cached: fmtInt(cached ?? 0), output: fmtInt(output ?? 0) }) : t('rt.tokens.none');
  const bar = h('span', { class: 'tokbar', role: 'img', 'aria-label': label, title: label });
  for (const [cls, f] of [
    ['tk-in', seg.uncached],
    ['tk-cached', seg.cached],
    ['tk-out', seg.output],
  ]) {
    if (f <= 0) continue;
    const s = h('i', { class: cls });
    s.style.width = `${(f * 100).toFixed(1)}%`;
    bar.append(s);
  }
  return bar;
}
export function durBar(ms, max, label) {
  const bar = h('span', { class: 'durbar', role: 'img', 'aria-label': label ?? fmtDur(ms) });
  const i = h('i');
  i.style.width = `${(shareOf(ms, max) * 100).toFixed(1)}%`;
  return bar.appendChild(i) && bar;
}
export function copyBtn(text, label = t('common.copy')) {
  return btn('', {
    icon: 'copy',
    size: 'sm',
    kind: 'ghost',
    title: label,
    ariaLabel: label,
    onClick: async () => {
      const ok = await copyText(text);
      toast(ok ? t('common.copied') : t('common.copy_failed'), { kind: ok ? 'ok' : 'bad', timeout: 1800 });
    },
  });
}

/**
 * A multi-line text to copy (a config snippet, two environment lines): a label, the text in a <pre>, and a Copy button. The text goes in
 * as text, never as markup; the copy is exactly the text shown.
 */
export function snippetBlock(text, label) {
  const b = btn(t('common.copy'), {
    icon: 'copy',
    size: 'sm',
    onClick: async () => {
      const ok = await copyText(text);
      toast(ok ? t('common.copied') : t('common.copy_failed'), { kind: ok ? 'ok' : 'bad', timeout: 1800 });
    },
  });
  return h('div', { class: 'snippet' }, h('div', { class: 'row gap wrap between snippet-head' }, h('span', { class: 'fix-label' }, label), b), h('pre', { class: 'snippet-code' }, text));
}

export const kv = (k, v) => h('div', { class: 'kv' }, h('dt', k), h('dd', v ?? dash()));
export const kvList = (rows) =>
  h(
    'dl',
    { class: 'facts rt-facts' },
    rows.filter(Boolean).map(([k, v]) => kv(k, v)),
  );

/** A sortable column header: a button inside <th aria-sort>. state = {sort, order}. */
export function sortTh(label, key, state, onSort, cls = '', sub = null) {
  const on = state.sort === key;
  const th = h('th', { scope: 'col', class: cls, 'aria-sort': on ? (state.order === 'asc' ? 'ascending' : 'descending') : 'none' });
  th.append(
    h(
      'button',
      { type: 'button', class: 'sort-btn', onClick: () => onSort(key, on && state.order === 'desc' ? 'asc' : 'desc') },
      label,
      h('span', { class: 'sort-mark', 'aria-hidden': 'true' }, on ? (state.order === 'asc' ? '▲' : '▼') : ''),
    ),
  );
  if (sub) th.append(h('span', { class: 'th-sub' }, sub));
  return th;
}

/** A plain column header; the tokens column carries its "in · out · cached" key under the name. */
export const colTh = (key, cls = '') => h('th', { scope: 'col', class: cls }, t(key), key === 'rt.col.tokens' ? h('span', { class: 'th-sub' }, t('rt.col.tokens_sub')) : null);

/** Pagination: "51-100 of 238" with previous / next. */
export function pager({ offset, limit, total, onPage }) {
  const from = total ? offset + 1 : 0,
    to = Math.min(total, offset + limit);
  return h(
    'div',
    { class: 'row gap wrap between pager' },
    h('span', { class: 'muted small', role: 'status' }, t('rt.pager.range', { from: fmtInt(from), to: fmtInt(to), total: fmtInt(total) })),
    h(
      'div',
      { class: 'row gap' },
      btn(t('rt.pager.prev'), { size: 'sm', disabled: offset <= 0, onClick: () => onPage(Math.max(0, offset - limit)) }),
      btn(t('rt.pager.next'), { size: 'sm', disabled: offset + limit >= total, onClick: () => onPage(offset + limit) }),
    ),
  );
}

export const tokText = (n) => fmtTok(n);
