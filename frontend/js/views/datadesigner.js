// Data Designer page: status + problems (shared header), dataset scripts as config files (shared editor),
// actions (preview / create / report / check models / show config), and runs with a server-paginated
// records table. Follows the SkillEvaluator page structure; every string goes through i18n (dd.*).
import { NcView } from '../components/base.js';
import { h, clear } from '../dom.js';
import { t } from '../i18n.js';
import { fmtInt, fmtMs, fmtBytes, fmtDate } from '../format.js';
import { valueOf } from '../metrics.js';
import { badge, btn, card, kv, stat, errorBox, emptyBox, skeleton } from '../components/ui.js';
import { systemPage } from '../components/system-page.js';
import { componentsTable, endpointsList } from '../components/status-panel.js';
import { eventsPanel, pushEvent } from '../components/events-panel.js';
import { actionsPanel } from '../components/actions-panel.js';
import { configEditor } from '../components/config-editor.js';
import { announce } from '../components/live.js';
import { RECORDS_LIMIT, runsFromEvents, liveRuns, verdictKind, pageMeta, visibleColumns, toggleHidden, cellText, truncateCell, rowJson } from '../dd-records.js';

const RUN_KIND = 'dd.run';
const SYS = 'datadesigner';

/** Live-run banner (SSE): one note per create run that is still "running", with the elapsed minutes. */
function liveBanner(scope) {
  const host = h('div', { class: 'stack-v', 'aria-live': 'polite' });
  let events = [];
  const render = () => {
    clear(host);
    const live = liveRuns(runsFromEvents(events));
    for (const r of live) host.append(h('p', { class: 'note info', role: 'status' }, badge(t('comp.running'), 'info'), ' ',
      h('strong', t('dd.live_line', { name: r.dataset_name || r.script || r.id })), ' ',
      h('span', { class: 'muted small' }, t('dd.live_elapsed', { mins: Math.max(0, Math.round((Date.now() / 1000 - r.ts) / 60)) }))));
    if (!live.length) host.append(h('p', { class: 'hint' }, t('dd.live_none')));
  };
  scope.api.events(SYS, 50).then((list) => {
    if (!scope.alive) return;
    for (const e of [...list].sort((a, b) => a.ts - b.ts)) if (e.kind === RUN_KIND) events = pushEvent(events, e);
    render();
  }).catch(() => {});
  scope.stream(`/systems/${SYS}/events/stream`, { onEvent: (ev) => { if (ev.data && ev.data.kind === RUN_KIND) { events = pushEvent(events, ev.data); render(); } } });
  scope.every(15000, render);
  return host;
}

function statusTab(scope, ctx) {
  const compHost = h('div'), statsHost = h('div', { class: 'stats' }), note = h('div');
  ctx.onStatus((st) => {
    clear(compHost).append(componentsTable(st.components), st.endpoints?.length ? h('h3', { class: 'sub' }, t('status.endpoints')) : null, endpointsList(st.endpoints));
  });
  async function loadStats() {
    try {
      const m = await scope.api.metrics(SYS);
      if (!scope.alive) return;
      if (!m.available) { clear(statsHost).append(h('p', { class: 'note warn' }, m.error || t('dd.metrics_unavailable'))); return; }
      const v = (k) => valueOf(m, k);
      clear(statsHost).append(stat(t('dd.scripts_stat'), fmtInt(v('dd.scripts'))), stat(t('dd.runs_stat'), fmtInt(v('dd.runs'))),
        stat(t('dd.last_records'), fmtInt(v('dd.last_records'))), stat(t('dd.live_stat'), v('dd.live_running') == null ? '–' : t(v('dd.live_running') ? 'common.yes' : 'common.no')));
    } catch (e) { if (scope.alive) clear(note).append(errorBox(e, loadStats)); }
  }
  loadStats(); scope.every(10000, loadStats);
  return h('div', { class: 'stack-v' },
    card(t('dd.state'), compHost),
    card(t('dd.overview'), h('div', { class: 'stack-v' }, statsHost, liveBanner(scope), h('p', { class: 'hint' }, t('dd.hint')))),
    card(t('dd.recent_events'), eventsPanel(scope, SYS, { kinds: [RUN_KIND] })));
}

function scriptsTab(scope) {
  return h('div', { class: 'stack-v' },
    h('p', { class: 'note warn', role: 'note' }, h('strong', t('dd.scripts_note_title')), ' ', t('dd.scripts_note')),
    configEditor(scope, SYS));
}

/** Server-paginated records table for one run: limit/offset pages, column show/hide, JS cell truncation
 *  with the full text in the title attribute, and a raw-JSON toggle per row. */
function recordsCard(scope, runId) {
  const st = { total: 0, columns: [], rows: [], offset: 0, truncated: false, hidden: new Set(), raw: new Set() };
  const controls = h('div', { class: 'row gap wrap between' }), colsHost = h('div', { class: 'row gap wrap' }), tableHost = h('div');
  const body = h('div', { class: 'stack-v' }, controls, tableHost);

  async function load() {
    clear(tableHost).append(skeleton(2));
    try {
      const r = await scope.api.get(`/systems/${SYS}/runs/${encodeURIComponent(runId)}/records`, { limit: RECORDS_LIMIT, offset: st.offset });
      if (!scope.alive) return;
      st.total = r.total || 0; st.columns = r.columns || []; st.rows = r.rows || []; st.truncated = !!r.truncated;
      render();
    } catch (e) { if (scope.alive) clear(tableHost).append(errorBox(e, load)); }
  }

  function render() {
    clear(controls); clear(colsHost); clear(tableHost);
    const pm = pageMeta(st.total, RECORDS_LIMIT, st.offset);
    if (!st.total) { tableHost.append(emptyBox(t('dd.records_empty'), t('dd.records_cap'))); return; }
    const cols = visibleColumns(st.columns, st.hidden);
    for (const c of st.columns) {
      const cb = h('input', { type: 'checkbox', checked: !st.hidden.has(c), onChange: () => { st.hidden = toggleHidden(st.hidden, c); render(); } });
      colsHost.append(h('label', { class: 'check' }, cb, h('span', c)));
    }
    controls.append(
      h('div', { class: 'row gap wrap' }, h('span', { class: 'muted small' }, t('dd.columns')), colsHost),
      h('div', { class: 'row gap' },
        st.truncated ? h('span', { class: 'muted small' }, t('dd.records_cap')) : null,
        btn(t('dd.page_prev'), { size: 'sm', disabled: !pm.hasPrev, onClick: () => { st.offset = Math.max(0, st.offset - RECORDS_LIMIT); load(); } }),
        h('span', { class: 'muted small num' }, t('dd.page_status', { from: pm.from, to: pm.to, total: fmtInt(pm.total) })),
        btn(t('dd.page_next'), { size: 'sm', disabled: !pm.hasNext, onClick: () => { st.offset = st.offset + RECORDS_LIMIT; load(); } })));
    const tbody = h('tbody');
    st.rows.forEach((row, i) => {
      const n = pm.offset + i + 1;
      const cells = cols.map((c) => {
        const { text, full } = truncateCell(cellText(row[c]));
        return h('td', { class: 'wrap-anywhere', title: full === text ? null : full }, text);
      });
      const open = st.raw.has(n);
      tbody.append(h('tr', cells, h('td', btn(open ? t('dd.raw_hide') : t('dd.raw_show'), { size: 'sm', 'aria-expanded': String(open), onClick: () => { if (open) st.raw.delete(n); else st.raw.add(n); render(); } }))));
      if (open) tbody.append(h('tr', h('td', { colspan: String(cols.length + 1) },
        h('pre', { class: 'cmd', tabindex: '0', 'aria-label': t('dd.raw_label', { n }) }, rowJson(row)))));
    });
    tableHost.append(h('div', { class: 'tablewrap tall' }, h('table', { class: 'compact', 'aria-label': t('dd.records') },
      h('thead', h('tr', cols.map((c) => h('th', { scope: 'col' }, c)), h('th', { scope: 'col' }, t('dd.col_raw')))), tbody)));
  }

  load();
  return card(t('dd.records'), body);
}

/** Files of a run directory: paths and sizes only — v1 has no file serving, the user opens them on disk. */
function filesCard(scope, runId) {
  const host = h('div');
  scope.loadInto(host, async () => {
    const r = await scope.api.get(`/systems/${SYS}/runs/${encodeURIComponent(runId)}/files`);
    if (!r?.files?.length) return emptyBox(t('dd.files_empty'), t('dd.files_hint'));
    return h('div', { class: 'tablewrap' }, h('table', { class: 'compact', 'aria-label': t('dd.files') },
      h('thead', h('tr', [t('dd.col_path'), t('dd.col_bytes')].map((x) => h('th', { scope: 'col' }, x)))),
      h('tbody', r.files.map((f) => h('tr', h('td', { class: 'wrap-anywhere' }, h('code', f.path)), h('td', { class: 'num' }, fmtBytes(f.bytes)))))));
  });
  return card(t('dd.files'), host);
}

function runsTab(scope) {
  const host = h('div', { class: 'stack-v' });
  const liveHost = h('div', { class: 'stack-v', 'aria-live': 'polite' }), listHost = h('div'), detailHost = h('div', { class: 'stack-v' });
  let events = [], selected = null;
  const runs = () => runsFromEvents(events);
  const runById = (id) => runs().find((r) => r.id === id) || null;

  // Live create-run progress line: elapsed + what is being generated (never a spinner-only state).
  function renderLive() {
    clear(liveHost);
    for (const r of liveRuns(runs())) liveHost.append(h('p', { class: 'note info', role: 'status' }, badge(t('comp.running'), 'info'), ' ',
      h('strong', t('dd.live_line', { name: r.dataset_name || r.script || r.id })), ' ',
      h('span', { class: 'muted small' }, t('dd.live_elapsed', { mins: Math.max(0, Math.round((Date.now() / 1000 - r.ts) / 60)) }))));
  }

  function renderList() {
    renderLive();
    clear(listHost);
    const rs = runs();
    if (!rs.length) { listHost.append(emptyBox(t('dd.runs_empty'), t('dd.runs_empty_hint'))); return; }
    const tbody = h('tbody');
    for (const r of rs) {
      const open = selected === r.id;
      tbody.append(h('tr',
        h('td', { class: 'num' }, fmtDate(r.ts)),
        h('td', h('button', { type: 'button', class: 'link', 'aria-expanded': String(open), onClick: () => select(r.id) }, r.id)),
        h('td', badge(r.verdict, verdictKind(r.verdict))),
        h('td', r.mode || '–'),
        h('td', { class: 'num' }, r.num_records != null ? fmtInt(r.num_records) : '–'),
        h('td', { class: 'wrap-anywhere' }, r.dataset_name || r.script || '–')));
    }
    listHost.append(h('div', { class: 'tablewrap tall' }, h('table', { class: 'compact', 'aria-label': t('dd.runs_list') },
      h('thead', h('tr', [t('ev.time'), t('dd.col_run'), t('dd.col_verdict'), t('dd.col_mode'), t('dd.col_records'), t('dd.col_dataset')].map((x) => h('th', { scope: 'col' }, x)))), tbody)));
  }

  function renderDetail() {
    clear(detailHost);
    const r = selected && runById(selected);
    if (!r) return;
    const meta = h('dl', { class: 'facts' },
      kv(t('dd.col_verdict'), badge(r.verdict, verdictKind(r.verdict))),
      kv(t('dd.col_mode'), r.mode || '–'),
      kv(t('dd.col_script'), h('code', r.script || '–')),
      kv(t('dd.col_records'), r.num_records != null ? fmtInt(r.num_records) : '–'),
      kv(t('dd.col_dataset'), r.dataset_name || '–'),
      kv(t('dd.col_format'), r.format || '–'),
      kv(t('dd.col_destination'), r.destination ? h('code', { title: r.destination }, r.destination) : '–'),
      kv(t('dd.col_duration'), r.duration_ms != null ? fmtMs(r.duration_ms) : '–'),
      kv(t('dd.col_started'), fmtDate(r.ts)));
    if (r.error) meta.append(kv(t('dd.col_error'), h('span', { class: 'wrap-anywhere' }, r.error)));
    detailHost.append(card(t('dd.detail_title', { id: r.id }), h('div', { class: 'stack-v' }, meta, h('p', { class: 'hint' }, t('dd.detail_hint')))));
    if (r.mode === 'preview' || r.mode === 'create') {
      detailHost.append(recordsCard(scope, r.id), filesCard(scope, r.id));
      announce(t('dd.detail_open', { id: r.id }));
    }
  }

  function select(id) { selected = selected === id ? null : id; renderList(); renderDetail(); }

  async function load() {
    try {
      const list = await scope.api.events(SYS, 100);
      if (!scope.alive) return;
      for (const e of [...list].sort((a, b) => a.ts - b.ts)) if (e.kind === RUN_KIND) events = pushEvent(events, e);
      renderList(); renderDetail();
    } catch (e) { if (scope.alive) clear(listHost).append(errorBox(e, load)); }
  }
  scope.stream(`/systems/${SYS}/events/stream`, { onEvent: (ev) => {
    if (!ev.data || ev.data.kind !== RUN_KIND) return;
    events = pushEvent(events, ev.data);
    renderList();
    if (selected && ev.data.data?.run_id === selected) renderDetail();
  } });
  load(); scope.every(20000, renderLive);
  return h('div', { class: 'stack-v' }, liveHost, card(t('dd.runs_list'), listHost), detailHost);
}

class NcDataDesigner extends NcView {
  async setup() {
    systemPage(this, { system: SYS, active: this.query?.tab, tabDefs: [
      { id: 'status', label: t('tab.status'), render: (scope, ctx) => statusTab(scope, ctx) },
      { id: 'scripts', label: t('tab.scripts'), render: (scope) => scriptsTab(scope) },
      { id: 'actions', label: t('tab.actions'), render: (scope) => actionsPanel(scope, SYS) },
      { id: 'runs', label: t('tab.runs'), render: (scope) => runsTab(scope) },
    ] });
  }
}
customElements.define('nc-datadesigner', NcDataDesigner);
