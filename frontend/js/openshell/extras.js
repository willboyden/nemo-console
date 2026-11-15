// The OpenShell Status additions (addendum 9.16), all read-only: (1) sandbox lifecycle swim lanes from the persisted `openshell.sandbox` events (a lane per sandbox, coloured phase
// segments, create -> ready -> delete), (2) the sandbox count by phase over time as a stacked area (`os.sandboxes_by_phase` history), (3) the policy decisions of one sandbox as a timeline
// by category, allowed against denied, from `GET /systems/openshell/decisions`. Nothing here changes a sandbox or a policy: deleting stays the existing destructive action on the Actions
// tab. The lanes, the area and the decision list follow the page's window picker; the chosen sandbox lives in the URL hash (`sandbox=`). Everything from the server is text.
import { h } from '../dom.js';
import { t } from '../i18n.js';
import { describeError } from '../errors.js';
import { fmtDur, fmtStamp } from '../viz/fmt.js';
import { WINDOW_SPAN_S } from '../series-math.js';
import { badge, btn, emptyBox, select } from '../components/ui.js';
import { fitted } from '../charts/fit.js';
import { lifecycleChart } from '../charts/lifecycle-chart.js';
import { decisionChart } from '../charts/decision-chart.js';
import { stackCard } from '../charts/stack-card.js';
import { liveTable, swap, windowPhrase } from '../charts/parts.js';
import { PHASES, DECISION_LIMIT, lifecycleLanes, presentSandboxes, phaseStack, decisionModel } from '../openshell-model.js';

const GAP_S = { '15m': 8, '1h': 15, '24h': 30, '7d': 60 };
const PHASE_CLS = { Provisioning: 'bf1', Ready: 'bf2', Error: 'bf-bad', Deleting: 'bf3', Other: 'bf4' };
const phaseName = (p) => t(`osv.phase.${p.toLowerCase()}`);
const legend = (items) =>
  h(
    'ul',
    { class: 'legend' },
    items.map(([cls, label]) => h('li', h('i', { class: `sw ${cls}`, 'aria-hidden': 'true' }), h('span', label))),
  );

// ---------------------------------------------------------------- lifecycle lanes
function lifecycleCard(scope, ctxApi) {
  let model = null,
    range = null;
  const chartHost = h('div', { class: 'chart-wrap-lg' });
  const noteHost = h('div');
  const table = liveTable(t('chart.data_table'), [t('osv.col_sandbox'), t('osv.col_created'), t('osv.col_ready_after'), t('osv.col_end'), t('osv.col_lifetime')]);
  const keys = legend(['Provisioning', 'Ready', 'Error', 'Deleting', 'Other'].map((p) => [`lc-sw lc-${p.toLowerCase()}`, phaseName(p)]));
  const el = h(
    'section',
    { class: 'card chart-card rt-chart wide os-lifecycle', 'aria-label': t('osv.lifecycle_title') },
    h('header', { class: 'card-head' }, h('h3', t('osv.lifecycle_title'))),
    h('div', { class: 'card-body' }, h('p', { class: 'hint' }, t('osv.lifecycle_hint')), noteHost, chartHost, keys, table.el),
  );
  function draw() {
    if (!model) return;
    const width = Math.max(300, Math.round(chartHost.clientWidth || 640));
    swap(
      noteHost,
      model.hidden ? h('p', { class: 'muted small' }, t('osv.lanes_hidden', { count: model.hidden, shown: model.lanes.length })) : null,
      model.partial ? h('p', { class: 'note info', role: 'status' }, t('osv.events_partial')) : null,
    );
    keys.hidden = !model.lanes.length;
    if (!model.lanes.length) {
      chartHost.replaceChildren(emptyBox(t('osv.lifecycle_empty'), t('osv.lifecycle_empty_hint', { window: windowPhrase(ctxApi.win()) })));
      table.set([]);
      return;
    }
    const aria = t('osv.lifecycle_alt', { count: model.lanes.length, window: windowPhrase(ctxApi.win()) });
    chartHost.replaceChildren(lifecycleChart({ lanes: model.lanes, from: range.from, to: range.to, width, selected: ctxApi.selected(), onPick: ctxApi.pick, ariaLabel: aria }));
    table.set(
      model.lanes.map((l) => [
        l.name,
        l.createdTs != null ? `${fmtStamp(l.createdTs)} UTC` : null,
        l.provisionS != null ? fmtDur(l.provisionS * 1000) : null,
        l.deletedTs != null ? `${t('osv.col_deleted')} ${fmtStamp(l.deletedTs)} UTC` : l.present ? t('osv.col_present') : t('osv.lane_gone'),
        l.lifetimeS != null ? fmtDur(l.lifetimeS * 1000) : null,
      ]),
    );
  }
  let lastW = 0;
  if (typeof ResizeObserver !== 'undefined') {
    const ro = new ResizeObserver(() => {
      const w = Math.round(chartHost.clientWidth);
      if (!model || w < 80 || Math.abs(w - lastW) < 6) return;
      lastW = w;
      draw();
    });
    ro.observe(chartHost);
    scope.own(() => ro.disconnect());
  }
  return {
    el,
    update(events, r) {
      range = r;
      model = { ...lifecycleLanes(events, { from: r.from, to: r.to, now: r.to }), partial: events.length >= 500 };
      draw();
    },
    redraw: draw,
    fail(msg) {
      swap(noteHost, h('p', { class: 'note warn', role: 'status' }, msg));
    },
  };
}

// ---------------------------------------------------------------- phase counts over time
function phasesCard(scope) {
  const card = stackCard(scope, {
    title: t('osv.phases_title'),
    hint: t('osv.phases_hint'),
    parts: PHASES.map((p) => ({ key: p, name: phaseName(p), cls: PHASE_CLS[p] })),
    valueFmt: (v) => String(Math.round(v * 10) / 10),
    axisFmt: (v) => (Number.isInteger(v) ? String(v) : ''),
    rowsTitle: t('osv.phases_now'),
    timeTitle: t('osv.phases_over_time'),
    emptyText: t('osv.phases_empty_now'),
    timeKind: 'area',
    wide: true,
  });
  return {
    el: card.el,
    update(answer) {
      const m = phaseStack(answer);
      const total = PHASES.reduce((a, p) => a + (m.latest[p] ?? 0), 0);
      const known = PHASES.some((p) => m.latest[p] != null);
      const never = m.any && !(m.peak > 0); // sampled all window, and no sandbox existed in any sample: a real zero, said in words
      card.update({
        rows: known ? [{ id: 'now', label: t('osv.now'), values: m.latest, totalText: t('osv.phases_total', { count: total }), note: never ? t('osv.phases_none_window') : null }] : [],
        time: m.any && !never ? [{ label: '', ts: m.ts, bucketS: m.bucketS, series: m.series }] : [],
      });
    },
    fail: card.fail,
  };
}

// ---------------------------------------------------------------- policy decisions
/** The decisions of a short log as a table: time, category, kind, the decision (a glyph and a word), the target. */
function sparseTable(rows) {
  return h(
    'div',
    { class: 'tablewrap' },
    h(
      'table',
      { class: 'compact', 'aria-label': t('osv.dec_table_rows') },
      h(
        'thead',
        h(
          'tr',
          ['osv.dec_col_time', 'osv.dec_col_category', 'osv.dec_col_kind', 'osv.dec_col_verdict', 'osv.dec_col_target'].map((k) => h('th', { scope: 'col' }, t(k))),
        ),
      ),
      h(
        'tbody',
        rows.map((r) =>
          h(
            'tr',
            h('td', { class: 'num' }, `${fmtStamp(r.ts)} UTC`),
            h('td', r.category),
            h('td', r.kind ?? '–'),
            h('td', badge(r.verdict === 'denied' ? `✕ ${t('osv.dec_denied')}` : `✓ ${t('osv.dec_allowed')}`, r.verdict === 'denied' ? 'bad' : 'ok')),
            h('td', r.target ? h('code', r.target) : '–'),
          ),
        ),
      ),
    ),
  );
}

function decisionsCard(scope, ctxApi) {
  let present = [],
    model = null,
    state = 'idle', // idle | loading | ok | gone | fail
    error = null,
    sandbox = '',
    seq = 0,
    lastAt = 0;
  const pickHost = h('div', { class: 'field filter os-pick' });
  const noteHost = h('div');
  const totalsHost = h('p', { class: 'os-dec-totals', role: 'status' });
  const chartHost = h('div', { class: 'chart-wrap-lg' });
  const keys = legend([
    ['dc-sw dc-allowed', t('osv.dec_allowed')],
    ['dc-sw dc-denied', `✕ ${t('osv.dec_denied')}`],
  ]);
  const targetsHost = h('div');
  const table = liveTable(t('osv.dec_table'), [t('osv.dec_col_category'), t('osv.dec_col_allowed'), t('osv.dec_col_denied')]);
  const el = h(
    'section',
    { class: 'card chart-card rt-chart wide os-decisions', 'aria-label': t('osv.dec_title') },
    h(
      'header',
      { class: 'card-head' },
      h('h3', t('osv.dec_title')),
      h('div', { class: 'row gap wrap' }, pickHost, btn(t('common.refresh'), { size: 'sm', icon: 'restart', onClick: () => load(true) })),
    ),
    h('div', { class: 'card-body stack-v' }, h('p', { class: 'hint' }, t('osv.dec_hint')), noteHost, totalsHost, chartHost, keys, targetsHost, table.el),
  );

  function fillPicker() {
    const opts = present.map((s) => ({ value: s.name, label: s.name }));
    const sel = select(opts, sandbox, (v) => ctxApi.pick(v), { id: 'os-sandbox', 'aria-label': t('osv.dec_pick') });
    swap(pickHost, h('label', { for: 'os-sandbox' }, t('osv.dec_pick')), sel);
    pickHost.hidden = !present.length;
  }

  function draw() {
    keys.hidden = true;
    swap(targetsHost);
    table.set([]);
    totalsHost.textContent = '';
    if (!present.length) {
      swap(noteHost);
      chartHost.replaceChildren(emptyBox(t('osv.dec_no_sandbox'), t('osv.dec_no_sandbox_hint')));
      return;
    }
    if (state === 'loading' && !model) {
      swap(noteHost);
      chartHost.replaceChildren(h('p', { class: 'muted' }, t('common.loading')));
      return;
    }
    if (state === 'gone') {
      swap(noteHost);
      chartHost.replaceChildren(emptyBox(t('osv.dec_gone'), t('osv.dec_gone_hint')));
      return;
    }
    if (state === 'fail' && !model) {
      swap(noteHost);
      chartHost.replaceChildren(emptyBox(t('osv.dec_failed'), error));
      return;
    }
    if (!model) return;
    swap(
      noteHost,
      state === 'fail' ? h('p', { class: 'note warn', role: 'status' }, error) : null,
      model.problem
        ? h('p', { class: 'note warn', role: 'status' }, t('osv.dec_problem', { message: model.problem.message || model.problem.code }), h('code', { class: 'muted small' }, ` ${model.problem.code}`))
        : null,
      model.truncated ? h('p', { class: 'note info', role: 'status' }, t('osv.dec_truncated', { max: DECISION_LIMIT })) : null,
      model.skipped ? h('p', { class: 'muted small' }, t('osv.dec_skipped', { count: model.skipped })) : null,
    );
    if (model.empty) {
      chartHost.replaceChildren(model.problem ? emptyBox(t('osv.dec_failed'), t('osv.dec_failed_hint')) : emptyBox(t('osv.dec_empty'), t('osv.dec_empty_hint', { name: sandbox })));
      return;
    }
    const sum = t('osv.dec_totals', { total: model.total, allowed: model.allowed, denied: model.denied, from: `${fmtStamp(model.from)} UTC`, to: `${fmtStamp(model.to)} UTC` });
    totalsHost.textContent = sum;
    const width = Math.max(300, Math.round(chartHost.clientWidth || 640));
    const aria = t('osv.dec_alt', { name: sandbox, total: model.total, allowed: model.allowed, denied: model.denied, lanes: model.lanes.length });
    if (model.sparse) {
      // a short log (often only the SSH open): the decisions themselves, not a timeline of two marks
      chartHost.replaceChildren(h('p', { class: 'note info', role: 'status' }, t('osv.dec_sparse', { count: model.total })), sparseTable(model.rows));
      totalsHost.textContent = '';
    } else {
      chartHost.replaceChildren(decisionChart({ model, width, ariaLabel: aria }));
      keys.hidden = false;
      table.set(model.lanes.map((l) => [l.category, l.allowed, l.denied]));
    }
    swap(
      targetsHost,
      model.sparse && !model.denied
        ? null
        : model.denied
          ? h(
              'div',
              { class: 'os-denied' },
              h('h4', { class: 'sc-sub' }, t('osv.dec_denied_targets')),
              h(
                'div',
                { class: 'tablewrap' },
                h(
                  'table',
                  { class: 'compact', 'aria-label': t('osv.dec_denied_targets') },
                  h(
                    'thead',
                    h(
                      'tr',
                      ['osv.dec_col_category', 'osv.dec_col_kind', 'osv.dec_col_target', 'osv.dec_col_count', 'osv.dec_col_last'].map((k, i) =>
                        h('th', { scope: 'col', class: i >= 3 ? 'al-right' : '' }, t(k)),
                      ),
                    ),
                  ),
                  h(
                    'tbody',
                    model.deniedTargets.map((r) =>
                      h(
                        'tr',
                        h('td', r.category),
                        h('td', r.kind ?? '–'),
                        h('td', r.target ? h('code', r.target) : '–'),
                        h('td', { class: 'num al-right' }, String(r.count)),
                        h('td', { class: 'num al-right' }, `${fmtStamp(r.last)} UTC`),
                      ),
                    ),
                  ),
                ),
              ),
            )
          : h('p', { class: 'note ok', role: 'status' }, t('osv.dec_none_denied')),
    );
  }

  async function load(force = false) {
    if (!sandbox || !present.some((s) => s.name === sandbox)) {
      model = null;
      state = 'idle';
      draw();
      return;
    }
    if (!force && Date.now() - lastAt < 15000) return;
    lastAt = Date.now();
    const my = ++seq;
    if (!model || model.sandbox !== sandbox) {
      model = null;
      state = 'loading';
      draw();
    }
    try {
      const a = await scope.api.decisions(sandbox, DECISION_LIMIT);
      if (!scope.alive || my !== seq) return;
      model = decisionModel(a);
      state = 'ok';
    } catch (e) {
      if (!scope.alive || my !== seq) return;
      state = e?.status === 404 ? 'gone' : 'fail';
      error = describeError(e);
      if (state === 'gone') model = null;
    }
    draw();
  }

  let lastW = 0;
  if (typeof ResizeObserver !== 'undefined') {
    const ro = new ResizeObserver(() => {
      const w = Math.round(chartHost.clientWidth);
      if (!model || w < 80 || Math.abs(w - lastW) < 6) return;
      lastW = w;
      draw();
    });
    ro.observe(chartHost);
    scope.own(() => ro.disconnect());
  }
  return {
    el,
    /** The sandboxes that exist now and the one chosen (kept when it still exists, else the newest). */
    setPresent(list, chosen) {
      const same = list.map((s) => s.name).join('\n') === present.map((s) => s.name).join('\n');
      present = list;
      const next = list.some((s) => s.name === chosen) ? chosen : (list[0]?.name ?? '');
      const changed = next !== sandbox;
      sandbox = next;
      if (!same || changed) fillPicker();
      if (changed) {
        model = null;
        load(true);
      } else if (!list.length) draw();
      return sandbox;
    },
    select(name) {
      if (name === sandbox) return;
      sandbox = name;
      model = null;
      fillPicker();
      load(true);
    },
    refresh: () => load(false),
    redraw: draw,
  };
}

/** `desc.extras(scope, shared)` of the OpenShell page: returns {el}. */
export function openshellExtras(scope, shared) {
  const hash = shared.hash;
  let chosen = hash.get('sandbox');
  let last = 0,
    busy = false,
    lanesNow = new Map(); // the lanes of the window by name, so a click on a deleted one can be answered
  const failures = new Map();
  const pickNote = h('div');
  const api = {
    win: () => shared.win(),
    selected: () => chosen,
    pick: (name) => {
      const lane = lanesNow.get(name);
      if (lane && !lane.present) {
        // a deleted sandbox has no log to read: say so instead of asking for it
        swap(pickNote, h('p', { class: 'note info', role: 'status' }, t('osv.pick_gone', { name })));
        return;
      }
      swap(pickNote);
      chosen = name;
      hash.set({ sandbox: name });
      dec.select(name);
      life.redraw();
    },
  };
  const life = lifecycleCard(scope, api);
  const phases = phasesCard(scope);
  const dec = decisionsCard(scope, api);
  const noteHost = h('div');
  const el = h(
    'div',
    { class: 'stack-v os-viz' },
    h('div', { class: 'row gap wrap between' }, h('h2', t('osv.title')), h('span', { class: 'muted small' }, t('osv.title_hint'))),
    noteHost,
    pickNote,
    life.el,
    phases.el,
    dec.el,
  );

  async function ask(name, fn) {
    try {
      const out = await fn();
      failures.delete(name);
      return out;
    } catch (e) {
      failures.set(name, describeError(e));
      return null;
    }
  }
  async function load(force = false) {
    const w = shared.win();
    if (busy || (!force && Date.now() - last < (GAP_S[w] ?? 15) * 1000)) return;
    busy = true;
    last = Date.now();
    try {
      const [evs, ans] = await Promise.all([
        ask('events', () => scope.api.events('openshell', 500)),
        ask('phases', () => scope.api.series('openshell', { keys: ['os.sandboxes_by_phase'], window: w, split: true })),
      ]);
      if (!scope.alive || w !== shared.win()) return;
      if (evs) {
        const to = ans?.to ?? Math.floor(Date.now() / 1000);
        const r = { from: to - (WINDOW_SPAN_S[w] ?? 3600), to };
        lanesNow = new Map(lifecycleLanes(evs, { from: r.from, to: r.to, now: r.to, maxLanes: 1000 }).lanes.map((l) => [l.name, l]));
        life.update(evs, r);
        chosen = dec.setPresent(presentSandboxes(evs), chosen);
        if (chosen && hash.get('sandbox') !== chosen) hash.set({ sandbox: chosen });
        life.redraw();
        dec.refresh();
      }
      if (ans) phases.update(ans);
      swap(
        noteHost,
        ...[...failures.values()].slice(0, 2).map((m) => h('p', { class: 'note warn', role: 'status' }, m)),
        ans?.truncated ? h('p', { class: 'note info', role: 'status' }, t('series.truncated')) : null,
      );
    } finally {
      busy = false;
    }
  }
  shared.onWindow(() => load(true));
  load(true);
  scope.every(10000, () => load(false));
  return { el };
}
