// Metadata-only event table: recent events over GET, then live over SSE (never any prompt/completion text).
import { h, clear } from '../dom.js';
import { t } from '../i18n.js';
import { fmtTime, fmtInt, fmtMs } from '../format.js';
import { badge, btn, emptyBox } from './ui.js';
import { describeError } from '../errors.js';
import { announce } from './live.js';

export const EVENT_CAP = 100;
/** Prepend newest-first, capped; identical (ts, kind, summary) events from the initial fetch + stream are de-duplicated. */
export function pushEvent(list, ev, cap = EVENT_CAP) {
  const key = (e) => `${e.ts}|${e.kind}|${e.summary}`;
  if (list.some((e) => key(e) === key(ev))) return list;
  return [ev, ...list].slice(0, cap);
}

const COLS = {
  'routing.decision': (d) => [d.route || '–', d.tier || '–', d.model || '–', d.prompt_tokens != null ? `${fmtInt(d.prompt_tokens)} / ${fmtInt(d.completion_tokens)}` : '–', d.latency_ms != null ? fmtMs(d.latency_ms) : '–'],
};

/** mode 'routing' shows route/tier/model/tokens/latency columns; 'generic' shows kind + summary. */
export function eventsPanel(scope, systemId, { mode = 'generic', kinds = null, limit = 30 } = {}) {
  let events = [], paused = false, state = 'connecting', failure = null;
  const stateEl = h('span'), tbody = h('tbody'), host = h('div', { class: 'stack-v' });
  const accept = (e) => !kinds || kinds.includes(e.kind);
  const headers = mode === 'routing' ? ['ev.time', 'ev.route', 'ev.tier', 'ev.model', 'ev.tokens', 'ev.latency'] : ['ev.time', 'ev.kind', 'ev.summary'];
  const render = () => {
    clear(stateEl).append(badge(t(`ev.state_${paused ? 'paused' : state}`), paused ? 'warn' : state === 'live' ? 'ok' : state === 'failed' ? 'bad' : 'muted'));
    clear(failHost); if (state === 'failed') failHost.append(h('div', { class: 'note bad', role: 'alert' }, h('p', failure), btn(t('common.retry'), { size: 'sm', icon: 'restart', onClick: () => connect() })));
    clear(tbody);
    for (const e of events) {
      const cols = mode === 'routing' && COLS[e.kind] ? COLS[e.kind](e.data || {}) : null;
      tbody.append(cols ? h('tr', h('td', { class: 'num' }, fmtTime(e.ts)), cols.map((c, i) => h('td', { class: i >= 3 ? 'num' : '' }, i === 0 ? h('code', c) : c)))
        : h('tr', h('td', { class: 'num' }, fmtTime(e.ts)), h('td', h('code', e.kind)), h('td', { class: 'wrap-anywhere' }, e.summary)));
    }
    empty.hidden = events.length > 0; wrap.hidden = events.length === 0;
  };
  const failHost = h('div');
  const empty = emptyBox(t('ev.empty'), t('ev.empty_hint'));
  const wrap = h('div', { class: 'tablewrap tall' }, h('table', { class: 'compact' }, h('thead', h('tr', headers.map((k) => h('th', { scope: 'col' }, t(k))))), tbody));
  const pauseBtn = btn(t('ev.pause'), { size: 'sm', onClick: () => { paused = !paused; pauseBtn.querySelector('span').textContent = t(paused ? 'ev.resume' : 'ev.pause'); announce(t(paused ? 'ev.paused_announce' : 'ev.resumed_announce')); render(); } });
  const clearBtn = btn(t('ev.clear'), { size: 'sm', onClick: () => { events = []; render(); } });
  host.append(failHost, h('div', { class: 'row gap wrap between' }, h('div', { class: 'row gap' }, stateEl, h('span', { class: 'muted small' }, t('ev.metadata_only'))), h('div', { class: 'row gap' }, pauseBtn, clearBtn)), empty, wrap);
  render();
  scope.api.events(systemId, limit).then((list) => { if (!scope.alive) return; for (const e of [...list].sort((a, b) => a.ts - b.ts)) if (accept(e)) events = pushEvent(events, e); render(); }).catch(() => {});
  function connect() {
    state = 'connecting'; failure = null; render();
    scope.stream(`/systems/${systemId}/events/stream`, {
      onOpen: () => { state = 'live'; render(); },
      onError: (e) => { if (e?.status) { state = 'failed'; failure = describeError(e); } else state = 'reconnecting'; render(); },
      onEvent: (ev) => { if (paused || !ev.data || typeof ev.data !== 'object' || !accept(ev.data)) return; events = pushEvent(events, ev.data); render(); },
    });
  }
  connect();
  return host;
}
