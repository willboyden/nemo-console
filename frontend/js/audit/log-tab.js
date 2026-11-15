// The Audit page's Log tab (addendum 9.16), admin only: the newest 200 audited calls, now narrowed by a time range (a preset, or a custom since / until in UTC), the actor (the key's alias),
// the status (ok or error) and the system, all kept in the URL hash (`range`, `since`, `until`, `actor`, `status`, `system`) so a filtered log can be shared and the Summary's matrix can open it.
// The filters are sent to `GET /audit` (since, until in epoch seconds; actor; status ok|error; system; limit). Every cell from a row is text; the parameters are shown as the server redacted them.
import { h } from '../dom.js';
import { t } from '../i18n.js';
import { fmtDate, debounce } from '../format.js';
import { auditLabel } from '../status-util.js';
import { SYSTEM_IDS } from '../systems.js';
import { isAdmin } from '../app-context.js';
import { badge, btn, card, emptyBox, errorBox, field, select, skeleton } from '../components/ui.js';
import { createHashState } from '../charts/hash-state.js';
import { swap } from '../charts/parts.js';
import { LOG_RANGES, LOG_LIMIT, logState, logQuery, logFiltered, parseUtcInput, toUtcInput } from '../audit-model.js';

const KEYS = ['range', 'since', 'until', 'actor', 'status', 'system'];

export function logTab(scope, ctx) {
  const hash = createHashState({ path: 'audit', tab: 'log', initial: ctx?.initial !== false, keys: KEYS });
  let st = logState(hash.all());
  let seq = 0;
  const host = h('div', {}, skeleton(4));
  const countHost = h('p', { class: 'muted small audit-count', role: 'status' });
  const actors = new Set(st.actor ? [st.actor] : []);

  /** Merge a patch into the URL state (null removes a key) and re-read the filter state from it. */
  function write(patch) {
    hash.set(patch);
    st = logState(hash.all());
  }

  // ---- controls
  const rangeSel = select(
    LOG_RANGES.map((r) => ({ value: r, label: t(`audv.range_${r}`) })),
    st.range,
    (v) => {
      write({ range: v === 'any' ? null : v, ...(v === 'custom' ? {} : { since: null, until: null }) });
      syncCustom();
      load();
    },
    { 'aria-label': t('audv.range') },
  );
  const sinceIn = h('input', { type: 'datetime-local', 'aria-label': t('audv.since'), value: toUtcInput(st.since) });
  const untilIn = h('input', { type: 'datetime-local', 'aria-label': t('audv.until'), value: toUtcInput(st.until) });
  const onTime = () => {
    const a = parseUtcInput(sinceIn.value),
      b = parseUtcInput(untilIn.value);
    write({ range: 'custom', since: a, until: b });
    load();
  };
  sinceIn.addEventListener('change', onTime);
  untilIn.addEventListener('change', onTime);
  const customRow = h('div', { class: 'row gap wrap bottom' }, field(t('audv.since_utc'), sinceIn), field(t('audv.until_utc'), untilIn));
  const syncCustom = () => {
    customRow.hidden = st.range !== 'custom';
  };
  const actorIn = h('input', { type: 'search', list: 'audit-actors', 'aria-label': t('audv.actor'), placeholder: t('audv.actor_ph'), value: st.actor, autocomplete: 'off', maxlength: 128 });
  const actorList = h('datalist', { id: 'audit-actors' });
  const applyActor = debounce(() => {
    write({ actor: actorIn.value.trim() || null });
    load();
  }, 300);
  actorIn.addEventListener('input', applyActor);
  const statusSel = select(
    [
      { value: '', label: t('audv.status_any') },
      { value: 'ok', label: t('audv.status_ok') },
      { value: 'error', label: t('audv.status_error') },
    ],
    st.status,
    (v) => {
      write({ status: v || null });
      load();
    },
    { 'aria-label': t('audv.status') },
  );
  const sysSel = select(
    [{ value: '', label: t('audit.all') }, ...SYSTEM_IDS.map((x) => ({ value: x, label: t(`sys.${x}`) }))],
    st.system,
    (v) => {
      write({ system: v || null });
      load();
    },
    { 'aria-label': t('audit.system') },
  );
  const resetBtn = btn(t('audv.reset'), {
    size: 'sm',
    icon: 'close',
    onClick: () => {
      hash.clear(/./);
      st = logState({});
      rangeSel.value = 'any';
      sinceIn.value = untilIn.value = actorIn.value = statusSel.value = sysSel.value = '';
      syncCustom();
      load();
    },
  });
  const syncReset = () => {
    resetBtn.hidden = !logFiltered(st);
  };
  syncCustom();

  // ---- rows
  const COLS = ['audit.when', 'audit.actor', 'audit.system_col', 'audit.action', 'audit.params', 'audit.status'];
  function table(rows) {
    return h(
      'div',
      { class: 'tablewrap tall' },
      h(
        'table',
        { class: 'compact' },
        h(
          'thead',
          h(
            'tr',
            COLS.map((k) => h('th', { scope: 'col' }, t(k))),
          ),
        ),
        h(
          'tbody',
          rows.map((r) =>
            h(
              'tr',
              h('td', { class: 'num' }, fmtDate(r.ts)),
              h('td', { class: 'audit-actor' }, r.actor || '–'),
              h('td', r.system || '–'),
              h('td', h('code', auditLabel(r)), r.role ? h('span', { class: 'muted small' }, ` (${r.role})`) : null),
              h('td', { class: 'wrap-anywhere small' }, r.params && Object.keys(r.params).length ? JSON.stringify(r.params) : '–'),
              h('td', badge(String(r.status ?? '–'), (r.status ?? 200) >= 400 ? 'bad' : 'ok')),
            ),
          ),
        ),
      ),
    );
  }

  async function load() {
    const my = ++seq;
    syncReset();
    swap(host, skeleton(4));
    try {
      const rows = await scope.api.audit(logQuery(st, Date.now() / 1000, LOG_LIMIT));
      if (!scope.alive || my !== seq) return;
      for (const r of rows) if (r.actor) actors.add(r.actor);
      countHost.textContent = rows.length >= LOG_LIMIT ? t('audv.count_capped', { count: rows.length }) : t('audv.count', { count: rows.length });
      swap(host, rows.length ? table(rows) : emptyBox(t('audit.empty'), logFiltered(st) ? t('audv.empty_filtered') : null));
    } catch (e) {
      if (scope.alive && my === seq) {
        countHost.textContent = '';
        swap(host, errorBox(e, load));
      }
    }
  }
  /** The actors the log has seen (for the suggestions): the summary names them over 30 days, so a filter can be picked before any row is loaded. */
  async function fillActors() {
    try {
      const a = await scope.api.auditSummary('30d');
      if (!scope.alive) return;
      for (const r of Array.isArray(a?.by_actor) ? a.by_actor : []) if (typeof r?.actor === 'string' && r.actor) actors.add(r.actor);
      actorList.replaceChildren(...[...actors].sort().map((x) => h('option', { value: x })));
    } catch {
      /* suggestions are optional */
    }
  }

  if (!isAdmin()) return h('div', { class: 'stack-v' }, h('p', { class: 'muted' }, t('audit.explain')), card(t('audit.title'), emptyBox(t('audit.admin_only'), t('audit.admin_only_hint'))));
  load();
  fillActors();
  return h(
    'div',
    { class: 'stack-v' },
    h('p', { class: 'muted' }, t('audit.explain')),
    card(
      t('audit.title'),
      h(
        'div',
        { class: 'stack-v' },
        h(
          'div',
          { class: 'row gap wrap bottom audit-filters' },
          field(t('audv.range'), rangeSel),
          field(t('audv.actor'), actorIn),
          field(t('audv.status'), statusSel),
          field(t('audit.system'), sysSel),
          resetBtn,
          btn(t('common.refresh'), { size: 'sm', icon: 'restart', onClick: () => load() }),
        ),
        customRow,
        actorList,
        countHost,
        host,
      ),
    ),
  );
}
