// The Events tab of the Dynamo page (addendum 9.14): the `dynamo.log` events (the frontend and worker logs, redacted on the server: R-5)
// with a level filter (toggle chips with the count of each level), a component filter, a plain-text search, live over SSE. Every log line is
// rendered as TEXT (textContent through h()), never as markup. The filters live in the URL hash.
import { h, clear } from '../dom.js';
import { t } from '../i18n.js';
import { fmtTime } from '../format.js';
import { describeError } from '../errors.js';
import { badge, btn, emptyBox, field } from '../components/ui.js';
import { createHashState } from '../charts/hash-state.js';
import { pushEvent } from '../components/events-panel.js';
import { announce } from '../components/live.js';
import { LOG_LEVELS, logRows, filterLogs, logFacets, encodeLevels, decodeLevels } from '../dynamo-model.js';
import { swap } from '../charts/parts.js';

const CAP = 500;
const SHOWN = 200;
const LEVEL_KIND = { ERROR: 'bad', WARN: 'warn', INFO: 'info', DEBUG: 'muted', TRACE: 'muted' };

export function eventsTab(scope, ctx) {
  const hash = createHashState({ path: 'dynamo', tab: 'events', initial: ctx?.initial !== false });
  let levels = decodeLevels(hash.get('level')); // a Set, or null = all
  let component = hash.get('component'),
    text = hash.get('q');
  let events = [],
    paused = false,
    state = 'connecting',
    failure = null;

  const stateEl = h('span');
  const countEl = h('span', { class: 'muted small', role: 'status' });
  const chipsHost = h('div', { class: 'series-chips', role: 'group', 'aria-label': t('dynv.ev_levels') });
  const compSel = h('select', { 'aria-label': t('dynv.ev_component'), onChange: (e) => setFilter({ component: e.target.value }) });
  const search = h('input', { type: 'search', 'aria-label': t('dynv.ev_search'), placeholder: t('dynv.ev_search'), value: text, onInput: (e) => setFilter({ q: e.target.value }) });
  const failHost = h('div');
  const tbody = h('tbody');
  const emptyHost = h('div');
  const wrap = h(
    'div',
    { class: 'tablewrap tall' },
    h(
      'table',
      { class: 'compact dyn-log' },
      h(
        'thead',
        h(
          'tr',
          ['dynv.ev_time', 'dynv.ev_level', 'dynv.ev_comp', 'dynv.ev_msg'].map((k) => h('th', { scope: 'col' }, t(k))),
        ),
      ),
      tbody,
    ),
  );

  function setFilter(patch) {
    if ('component' in patch) component = patch.component;
    if ('q' in patch) text = patch.q;
    if ('levels' in patch) levels = patch.levels;
    hash.set({ level: encodeLevels(levels) || null, component: component || null, q: text || null });
    render();
  }
  const toggleLevel = (l) => {
    const next = new Set(levels ?? LOG_LEVELS);
    if (next.has(l)) next.delete(l);
    else next.add(l);
    setFilter({ levels: next.size === 0 || next.size === LOG_LEVELS.length ? null : next });
  };

  function render() {
    const rows = logRows(events);
    const facets = logFacets(rows);
    chipsHost.replaceChildren(
      ...LOG_LEVELS.map((l) => {
        const on = !levels || levels.has(l);
        const b = h(
          'button',
          { type: 'button', class: 'series-chip', 'aria-pressed': String(on), title: t('dynv.ev_toggle', { level: l }) },
          h('span', { class: 'series-chip-name' }, l),
          h('span', { class: 'num muted' }, String(facets.levels[l])),
        );
        b.addEventListener('click', () => toggleLevel(l));
        return b;
      }),
    );
    const names = facets.components.map((c) => c.name);
    const list = component && !names.includes(component) ? [component, ...names] : names;
    compSel.replaceChildren(h('option', { value: '', selected: !component }, t('dynv.ev_all_components')), ...list.map((n) => h('option', { value: n, selected: n === component }, n)));
    const shown = filterLogs(rows, { levels, component, text });
    countEl.textContent = t('dynv.ev_count', { shown: Math.min(shown.length, SHOWN), total: rows.length });
    stateEl.replaceChildren(badge(t(`ev.state_${paused ? 'paused' : state}`), paused ? 'warn' : state === 'live' ? 'ok' : state === 'failed' ? 'bad' : 'muted'));
    swap(failHost, state === 'failed' ? h('div', { class: 'note bad', role: 'alert' }, h('p', failure), btn(t('common.retry'), { size: 'sm', icon: 'restart', onClick: () => connect() })) : '');
    tbody.replaceChildren(
      ...shown
        .slice(0, SHOWN)
        .map((r) => h('tr', h('td', { class: 'num' }, fmtTime(r.ts)), h('td', badge(r.level, LEVEL_KIND[r.level])), h('td', h('code', r.component)), h('td', { class: 'wrap-anywhere' }, r.text))),
    );
    wrap.hidden = shown.length === 0;
    // while Dynamo is off the note above says why there is nothing; the generic hint would claim live lines
    swap(emptyHost, shown.length || state === 'off' ? '' : rows.length ? emptyBox(t('dynv.ev_none_match'), t('dynv.ev_none_match_hint')) : emptyBox(t('dynv.ev_empty'), t('dynv.ev_empty_hint')));
  }

  const pauseBtn = btn(t('ev.pause'), {
    size: 'sm',
    onClick: () => {
      paused = !paused;
      pauseBtn.querySelector('span').textContent = t(paused ? 'ev.resume' : 'ev.pause');
      announce(t(paused ? 'ev.paused_announce' : 'ev.resumed_announce'));
      render();
    },
  });
  const resetBtn = btn(t('dynv.ev_reset'), {
    size: 'sm',
    icon: 'close',
    onClick: () => {
      search.value = '';
      setFilter({ levels: null, component: '', q: '' });
    },
  });

  // Dynamo is opt-in: while it is off (no frontend, worker or mocker container) there are no logs to read, and asking
  // for them would only fail. Read the status first; load and follow the logs only when Dynamo is not off.
  const offHost = h('div');
  scope.api
    .status('dynamo')
    .then((st) => {
      if (!scope.alive) return;
      if (st && st.health === 'off') {
        clear(offHost).append(h('p', { class: 'note info', role: 'status' }, t('dynv.ev_off')));
        state = 'off';
        render();
        return;
      }
      // the status probe itself failed (e.g. no container runtime): reading the logs would fail the same way, so say so
      // and let the user try anyway instead of opening with an error
      if (st && st.health === 'unknown') {
        clear(offHost).append(
          h(
            'div',
            { class: 'note warn', role: 'status' },
            h('p', t('dynv.ev_unknown', { why: typeof st.summary === 'string' ? st.summary : '' })),
            btn(t('dynv.ev_try'), {
              size: 'sm',
              onClick: () => {
                clear(offHost);
                loadEvents();
                connect();
              },
            }),
          ),
        );
        state = 'off';
        render();
        return;
      }
      loadEvents();
      connect();
    })
    .catch(() => {
      if (!scope.alive) return;
      loadEvents();
      connect();
    });
  function loadEvents() {
    scope.api
      .events('dynamo', CAP)
      .then((list) => {
        if (!scope.alive) return;
        for (const e of [...list].sort((a, b) => a.ts - b.ts)) if (e.kind === 'dynamo.log') events = pushEvent(events, e, CAP);
        render();
      })
      .catch((e) => {
        if (scope.alive) {
          state = 'failed';
          failure = describeError(e);
          render();
        }
      });
  }
  function connect() {
    state = 'connecting';
    failure = null;
    render();
    scope.stream('/systems/dynamo/events/stream', {
      onOpen: () => {
        state = 'live';
        render();
      },
      onError: (e) => {
        if (e?.status) {
          state = 'failed';
          failure = describeError(e);
        } else state = 'reconnecting';
        render();
      },
      onEvent: (ev) => {
        if (paused || !ev.data || typeof ev.data !== 'object' || ev.data.kind !== 'dynamo.log') return;
        events = pushEvent(events, ev.data, CAP);
        render();
      },
    });
  }
  return h(
    'div',
    { class: 'stack-v' },
    h('p', { class: 'hint' }, t('dynv.ev_lead')),
    offHost,
    h(
      'section',
      { class: 'card', 'aria-label': t('dynv.ev_title') },
      h('header', { class: 'card-head' }, h('h2', t('dynv.ev_title')), h('div', { class: 'row gap' }, stateEl, pauseBtn)),
      h(
        'div',
        { class: 'card-body stack-v' },
        failHost,
        chipsHost,
        h('div', { class: 'row gap wrap bottom w2-controls' }, field(t('dynv.ev_component'), compSel), field(t('dynv.ev_search'), search), resetBtn, countEl),
        h('p', { class: 'muted small' }, t('dynv.ev_redacted')),
        emptyHost,
        wrap,
      ),
    ),
  );
}
