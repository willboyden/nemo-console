// The Audit page's Summary tab (addendum 9.16), admin only: from `GET /audit/summary?window=24h|7d|30d` the audited actions per day stacked by system, an actor x system matrix (click a cell
// to open the log for that actor and system) and the error timeline, with the counts in words above. The summary is aggregate counts only: no parameters, no paths, no content. The window
// lives in the URL hash. A day with no audited action is a real zero; a count the answer does not carry is an en dash. Words: i18n keys `audv.*`.
import { h } from '../dom.js';
import { t } from '../i18n.js';
import { fmtInt } from '../format.js';
import { isAdmin } from '../app-context.js';
import { levelOf } from '../viz/heat.js';
import { btn, emptyBox, errorBox, skeleton, stat } from '../components/ui.js';
import { createHashState, hashFor } from '../charts/hash-state.js';
import { segPicker } from '../charts/controls.js';
import { cellMatrix } from '../charts/cell-matrix.js';
import { heatLegend } from '../charts/heatmap.js';
import { fitted } from '../charts/fit.js';
import { stackCard } from '../charts/stack-card.js';
import { dataTable, swap } from '../charts/parts.js';
import { SUMMARY_WINDOWS, shapeSummary, summaryWindow, CONSOLE_SYSTEM, NO_ACTOR } from '../audit-model.js';

const PART_CLS = ['bf1', 'bf2', 'bf3', 'bf4', 'bf5', 'bf6'];
/** A system's display name: the console's own name for a managed system, the id as it is for anything else ("core": settings, "other"). */
const sysName = (id) => (id === CONSOLE_SYSTEM ? t('audv.system_console') : t(`nav.${id}`) !== `nav.${id}` ? t(`nav.${id}`) : id);
const sysTitle = (id) => (id === CONSOLE_SYSTEM ? t('audv.system_console') : t(`sys.${id}`) !== `sys.${id}` ? t(`sys.${id}`) : id);
const actorName = (a) => (a === NO_ACTOR ? t('audv.actor_none') : a);

const logHref = (q) => hashFor('audit', 'log', q);
/** A count axis shows whole numbers only: a nice scale over 0..2 has ticks at 0.5, which would print as 1 twice. */
const intTick = (v) => (Number.isInteger(v) ? String(v) : '');

export function summaryTab(scope, ctx) {
  const root = h('div', { class: 'stack-v' });
  if (!isAdmin()) {
    root.append(emptyBox(t('audit.admin_only'), t('audit.admin_only_hint')));
    return root;
  }
  const hash = createHashState({ path: 'audit', tab: 'summary', initial: ctx?.initial !== false, keys: ['window'] });
  let win = summaryWindow(hash.get('window'));
  let seq = 0;
  const winText = (w) => t(`audv.win_${w}`);
  const picker = segPicker({
    values: SUMMARY_WINDOWS,
    current: () => win,
    labelOf: (w) => w,
    titleOf: winText,
    label: t('audv.window'),
    onPick: (w) => {
      win = w;
      hash.set({ window: w === '7d' ? null : w });
      picker.redraw();
      load();
    },
  });
  const statsHost = h('div', { class: 'stats' });
  const notes = h('div', { class: 'stack-v' });
  const dayHost = h('div');
  const matrixHost = h('div');
  const errHost = h('div');
  const body = h('div', { class: 'stack-v' }, statsHost, notes, dayHost, matrixHost, errHost);
  body.hidden = true;
  const loading = skeleton(3);
  const head = h(
    'div',
    { class: 'row gap wrap between' },
    h('h2', t('audv.title')),
    h('div', { class: 'row gap wrap' }, picker, btn(t('common.refresh'), { size: 'sm', icon: 'restart', onClick: () => load() })),
  );

  // ---- cards that are rebuilt only when their parts change (a stack card has its parts fixed at creation)
  let dayCard = null,
    daySig = '';
  function drawDays(m) {
    const parts = m.days.parts;
    if (!parts.length) {
      daySig = '';
      dayCard = null;
      swap(dayHost, emptyBox(t('audv.days_empty'), t('audv.days_empty_hint', { window: winText(m.window).toLowerCase() })));
      return;
    }
    const sig = parts.join('|');
    if (sig !== daySig || !dayCard) {
      daySig = sig;
      dayCard = stackCard(scope, {
        title: t('audv.days_title'),
        hint: t('audv.days_hint'),
        parts: parts.map((p, i) => ({ key: p, name: sysName(p), cls: PART_CLS[i % PART_CLS.length] })),
        valueFmt: (v) => fmtInt(v),
        axisFmt: intTick,
        rowsTitle: t('audv.days_total'),
        timeTitle: t('audv.days_per_day'),
        wide: true,
      });
    }
    const totals = Object.fromEntries(m.days.series.map((s) => [s.key, s.values.reduce((a, b) => a + b, 0)]));
    dayCard.update({
      rows: [{ id: 'all', label: winText(m.window), values: totals, totalText: fmtInt(m.total), note: null }],
      time: [{ label: '', ts: m.days.ts, bucketS: m.days.bucketS, series: m.days.series }],
    });
    const cols = [t('audv.col_day'), ...parts.map(sysName), t('audv.col_total')];
    // the per-day table is the text alternative of the per-day chart: it lives inside the card, after the stack card's own table
    let tableSlot = dayCard.el.querySelector('.audv-days-table');
    if (!tableSlot) {
      tableSlot = h('div', { class: 'audv-days-table' });
      dayCard.el.querySelector('.card-body').append(tableSlot);
    }
    swap(
      tableSlot,
      dataTable(t('audv.days_table'), cols, () =>
        m.days.ts.map((d, i) => [new Date(d * 1000).toISOString().slice(0, 10), ...m.days.series.map((s) => s.values[i]), m.days.series.reduce((a, s) => a + s.values[i], 0)]),
      ),
    );
    swap(dayHost, dayCard.el);
  }

  function drawMatrix(m) {
    if (!m.actors.length) {
      swap(matrixHost, emptyBox(t('audv.matrix_empty'), t('audv.days_empty_hint', { window: winText(m.window).toLowerCase() })));
      return;
    }
    const cols = m.matrixCols.map((s) => ({ id: s, label: sysName(s), title: sysTitle(s) }));
    const rows = m.actors.map((a) => ({ id: a.actor, label: actorName(a.actor) }));
    const domain = { min: 0, max: Math.max(1, m.matrixMax), curve: 'sqrt' };
    const open = (ri, ci) => {
      // the console's own calls and a call with no alias are not filters the log has: those cells open the log for the other half
      location.hash = logHref({ actor: rows[ri].id === NO_ACTOR ? null : rows[ri].id, system: cols[ci].id === CONSOLE_SYSTEM ? null : cols[ci].id, range: m.window });
    };
    const alt = t('audv.matrix_alt', { actors: rows.length, systems: cols.length, total: m.total });
    const chart = fitted(
      scope,
      (w) =>
        cellMatrix({
          cols,
          rows,
          width: w,
          ariaLabel: alt,
          noDataLabel: t('audv.cell_none'),
          minCellW: 40,
          onCell: open,
          cell: (ri, ci) => {
            const n = m.matrix(rows[ri].id, cols[ci].id);
            if (n == null) return null;
            const level = levelOf(n, domain);
            return { cls: `heatc fh${level}`, level, text: fmtInt(n), label: t('audv.cell', { actor: rows[ri].label, system: cols[ci].title, count: n }) };
          },
        }),
      null,
    );
    swap(
      matrixHost,
      h(
        'section',
        { class: 'card chart-card rt-chart wide', 'aria-label': t('audv.matrix_title') },
        h('header', { class: 'card-head' }, h('h3', t('audv.matrix_title'))),
        h(
          'div',
          { class: 'card-body stack-v' },
          h('p', { class: 'hint' }, t('audv.matrix_hint')),
          m.actorsHidden ? h('p', { class: 'muted small' }, t('audv.actors_hidden', { count: m.actorsHidden, shown: rows.length })) : null,
          chart,
          heatLegend({ domain, fmt: (v) => String(Math.round(v)), noData: true }),
          dataTable(t('audv.matrix_table'), [t('audit.actor'), ...cols.map((c) => c.title)], () => rows.map((r) => [r.label, ...cols.map((c) => m.matrix(r.id, c.id))])),
          h(
            'ul',
            { class: 'audv-links' },
            rows
              .filter((r) => r.id !== NO_ACTOR)
              .slice(0, 6)
              .map((r) => h('li', h('a', { href: logHref({ actor: r.id, range: m.window }) }, t('audv.open_log_for', { actor: r.label })))),
          ),
        ),
      ),
    );
  }

  let errCard = null;
  function drawErrors(m) {
    const e = m.errors;
    if (!e.total) {
      errCard = null;
      swap(
        errHost,
        h(
          'section',
          { class: 'card chart-card', 'aria-label': t('audv.errors_title') },
          h('header', { class: 'card-head' }, h('h3', t('audv.errors_title'))),
          h('div', { class: 'card-body' }, h('p', { class: 'note ok', role: 'status' }, t('audv.errors_none', { window: winText(m.window).toLowerCase() }))),
        ),
      );
      return;
    }
    if (!errCard) {
      errCard = stackCard(scope, {
        title: t('audv.errors_title'),
        hint: t('audv.errors_hint'),
        parts: [{ key: 'errors', name: t('audv.errors_part'), cls: 'bf-bad' }],
        valueFmt: (v) => fmtInt(v),
        axisFmt: intTick,
        rowsTitle: t('audv.errors_total'),
        timeTitle: t('audv.errors_over_time'),
        wide: true,
      });
    }
    errCard.update({
      rows: [{ id: 'e', label: winText(m.window), values: { errors: e.total }, totalText: fmtInt(e.total), note: null }],
      time: [{ label: bucketText(e.bucketS), ts: e.ts, bucketS: e.bucketS, series: [{ key: 'errors', values: e.values }] }],
    });
    swap(errHost, errCard.el);
  }
  const bucketText = (s) => (s >= 86400 ? t('audv.bucket_day') : s >= 3600 ? t('audv.bucket_hours', { count: Math.round(s / 3600) }) : t('audv.bucket_minutes', { count: Math.round(s / 60) }));

  function draw(a) {
    const m = shapeSummary(a, { now: Date.now() / 1000 });
    swap(
      statsHost,
      stat(t('audv.stat_actions'), fmtInt(m.total), winText(m.window)),
      stat(t('audv.stat_errors'), m.errors.total ? fmtInt(m.errors.total) : '0', t('audv.stat_errors_sub')),
      stat(t('audv.stat_denied'), m.deniedTotal == null ? '–' : fmtInt(m.deniedTotal), t('audv.stat_denied_sub')),
      stat(t('audv.stat_systems'), fmtInt(m.systems.length)),
      stat(t('audv.stat_actors'), fmtInt(m.actors.length + m.actorsHidden)),
    );
    swap(
      notes,
      m.dropped ? h('p', { class: 'note info', role: 'status' }, t('audv.dropped', { count: m.dropped })) : null,
      m.truncated ? h('p', { class: 'note info', role: 'status' }, t('audv.truncated')) : null,
    );
    drawDays(m);
    drawMatrix(m);
    drawErrors(m);
  }

  async function load() {
    const my = ++seq;
    if (body.hidden) swap(root, head, loading);
    try {
      const a = await scope.api.auditSummary(win);
      if (!scope.alive || my !== seq) return;
      body.hidden = false;
      swap(root, head, body);
      draw(a);
    } catch (e) {
      if (!scope.alive || my !== seq) return;
      swap(root, head, e?.status === 404 ? emptyBox(t('audv.unsupported'), t('audv.unsupported_hint')) : errorBox(e, load));
    }
  }
  swap(root, head, loading);
  load();
  scope.every(60000, load);
  return root;
}
