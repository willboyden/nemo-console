// The "Runtime diagnostics" card of the Capture tab (docs/ARCHITECTURE.md addendum 9.17), built ONLY for an admin (the caller does
// not construct it for anyone else; the route answers a viewer 403). It reads `GET /systems/relay/diagnostics`: the bounded counters
// and recent events the gateway keeps in its own memory (Relay 0.11), as the console reshaped them. The answer never carries an
// event's `detail`, `correlation`, owner or process ids, so there is nothing sensitive to hold back here; everything is text anyway.
// Totals, a component table (failures first), failures by component as bars, memory and loss counters, and the recent events
// with an outcome filter. A `reason` (disabled, unreachable, unsupported_version, error) gets one fixed text. Refresh button, and a
// refresh every 30 s while the browser tab is visible (liveLoad pauses a hidden tab).
import { h } from '../dom.js';
import { t } from '../i18n.js';
import { fmtBytes, fmtInt, fmtDuration, timeAgo } from '../format.js';
import { badge, btn, card, emptyBox, errorBox, select, skeleton, stat } from '../components/ui.js';
import { fmtStamp } from '../viz/fmt.js';
import { labelledBar } from '../charts/parts.js';
import { liveLoad } from './live.js';
import { dash, snippetBlock, swap } from './ui.js';
import { breakParts, eventsTruncated, normalizeDiagnostics, failureBars, outcomeOptions, filterEvents, memoryGauges, outcomeKind, outcomeGlyph, severityKind } from '../relay-runtime-model.js';

/** The two environment lines the gateway needs for its diagnostics to be readable (both must be set; the console never sets them). */
export const DIAGNOSTICS_ON_LINES = 'NEMO_RELAY_DIAGNOSTICS=1\nNEMO_RELAY_DIAGNOSTICS_HTTP=1\n';
const REFRESH_MS = 30000;
/** The catalogue's word for `name` under `key` (built from a template, so the key family is checked by tests), else the name itself with spaces. */
const named = (key, name) => (t(key) === key ? String(name).replace(/_/g, ' ') : t(key));
const stamp = (s) => (s == null ? dash() : h('span', { class: 'num', title: `${new Date(s * 1000).toISOString()} · ${timeAgo(s)}` }, `${fmtStamp(s)} UTC`));
/** A name as text with a line-break opportunity between its parts (a <wbr> element, never markup from the data). */
const breakable = (name) => breakParts(name).flatMap((part, i) => (i ? [h('wbr'), part] : [part]));
const num = (n) => (n == null ? dash() : h('span', { class: 'num' }, fmtInt(n)));

export function diagnosticsCard(scope) {
  let model = null;
  let outcome = 'all';
  let optionsKey = '';
  let lossOpen = false; // the list of counters at zero stays open or closed across refreshes

  // ---- persistent hosts (a table the user scrolled keeps its place across a refresh)
  const updated = h('span', { class: 'muted small res-updated' });
  const refreshBtn = btn(t('common.refresh'), { size: 'sm', icon: 'restart', onClick: () => live.refresh() });
  const noteHost = h('div');
  const metaHost = h('div', { class: 'row gap wrap small rtd-meta' });
  const totalsHost = h('div', { class: 'stats rtd-totals' });
  const compWrap = h('div', { class: 'tablewrap tall' });
  const barsHost = h('div', { class: 'rtd-bars' });
  const memHost = h('div', { class: 'rtd-mem' });
  const lossHost = h('div');
  const filterHost = h('div', { class: 'field' });
  const evCaption = h('p', { class: 'muted small', role: 'status' });
  const evWrap = h('div', { class: 'tablewrap tall' });
  const sub = (key, ...rest) => h('h3', { class: 'sub' }, t(key), ...rest);
  const dataEl = h(
    'div',
    { class: 'stack-v rtd-data' },
    noteHost,
    metaHost,
    totalsHost,
    h('div', { class: 'stack-v tight' }, sub('rt.diag.components'), h('p', { class: 'hint' }, t('rt.diag.components_hint')), compWrap),
    h(
      'div',
      { class: 'grid two' },
      h('div', { class: 'stack-v' }, h('div', { class: 'stack-v tight' }, sub('rt.diag.by_component'), barsHost), h('div', { class: 'stack-v tight' }, sub('rt.diag.memory'), memHost)),
      h('div', { class: 'stack-v tight' }, sub('rt.diag.loss'), lossHost),
    ),
    h('div', { class: 'stack-v tight' }, h('div', { class: 'row gap wrap between' }, sub('rt.diag.events'), filterHost), evCaption, evWrap),
  );
  const stateHost = h('div');
  const body = h('div', { class: 'stack-v', 'aria-busy': 'true' }, skeleton(4));
  const el = card(t('rt.diag.title'), body, [updated, refreshBtn], { id: 'rt-diagnostics' });
  el.classList.add('rt-diag');

  // ---- the parts
  const totalsTiles = (tot, lastFailureAt, componentFailures) => [
    stat(t('rt.diag.t_recorded'), fmtInt(tot.recorded), t('rt.diag.t_recorded_sub')),
    stat(t('rt.diag.t_successes'), fmtInt(tot.successes)),
    (() => {
      // the total and the per-component counts come from different counters of the gateway: never say "none" while a component below shows failures
      const below = tot.failures > 0 ? 0 : componentFailures;
      const sub =
        tot.failures > 0
          ? lastFailureAt != null
            ? t('rt.diag.last_failure', { when: `${fmtStamp(lastFailureAt)} UTC` })
            : null
          : below > 0
            ? t('rt.diag.failed_below', { count: below })
            : t('rt.diag.none_failed');
      const s = stat(t('rt.diag.t_failures'), fmtInt(tot.failures), sub);
      if (tot.failures > 0 || below > 0) s.classList.add('hot');
      return s;
    })(),
    stat(t('rt.diag.t_unspecified'), fmtInt(tot.unspecified), t('rt.diag.t_unspecified_sub')),
    (() => {
      const s = stat(t('rt.diag.t_evictions'), fmtInt(tot.evictions), t('rt.diag.t_evictions_sub'));
      if (tot.evictions > 0) s.classList.add('hot');
      return s;
    })(),
  ];

  function componentsTable(rows) {
    if (!rows.length) return h('p', { class: 'muted' }, t('rt.diag.no_components'));
    return h(
      'table',
      { class: 'compact rtd-table', 'aria-label': t('rt.diag.components') },
      h(
        'thead',
        h(
          'tr',
          ['rt.diag.c_component', 'rt.diag.c_instances', 'rt.diag.c_successes', 'rt.diag.c_failures', 'rt.diag.c_occurrences', 'rt.diag.c_last_failure'].map((k, i) =>
            h('th', { scope: 'col', class: i && i < 5 ? 'al-right' : '' }, t(k)),
          ),
        ),
      ),
      h(
        'tbody',
        rows.map((c) =>
          h(
            'tr',
            { class: (c.failures ?? 0) > 0 ? 'rtd-fail' : null },
            h('th', { scope: 'row', class: 'rtd-key' }, h('code', c.key)),
            h('td', { class: 'al-right' }, num(c.instances)),
            h('td', { class: 'al-right' }, num(c.successes)),
            h('td', { class: 'al-right' }, (c.failures ?? 0) > 0 ? h('span', { class: 'badge bad' }, h('span', { 'aria-hidden': 'true' }, '✕'), fmtInt(c.failures)) : num(c.failures)),
            h('td', { class: 'al-right' }, num(c.occurrences)),
            h('td', stamp(c.lastFailureAt)),
          ),
        ),
      ),
    );
  }

  function failureBarsEl(rows) {
    const fb = failureBars(rows);
    if (!fb.rows.length) return h('p', { class: 'note ok' }, t('rt.diag.no_failures'));
    return h(
      'div',
      { class: 'stack-v tight' },
      h(
        'div',
        { class: 'rtd-barlist' },
        fb.rows.map((r) =>
          h(
            'div',
            { class: 'rtd-row' },
            h('span', { class: 'rtd-name', title: r.key }, r.key),
            labelledBar(r.failures, fb.top, t('rt.diag.bar_alt', { name: r.key, count: r.failures }), 'bf-bad'),
            h('span', { class: 'num rtd-val' }, fmtInt(r.failures)),
          ),
        ),
      ),
      fb.total > fb.shown ? h('p', { class: 'muted small' }, t('rt.diag.bars_more', { shown: fb.shown, total: fb.total })) : null,
    );
  }

  function memoryEl(m) {
    const gs = memoryGauges(m);
    if (!m || (!gs.length && m.reservedBytes == null)) return h('p', { class: 'muted' }, t('rt.diag.no_memory'));
    const fmt = { events: (n) => fmtInt(n), detail: (n) => fmtBytes(n) };
    return h(
      'div',
      { class: 'stack-v tight' },
      m.reservedBytes != null ? h('p', { class: 'rtd-reserved' }, t('rt.diag.reserved'), ' ', h('strong', { class: 'num' }, fmtBytes(m.reservedBytes))) : null,
      gs.map((g) => {
        const used = fmt[g.id](g.used);
        const cap = g.capacity != null ? fmt[g.id](g.capacity) : null;
        const text = cap == null ? used : t('rt.diag.of', { used, capacity: cap });
        const alt = `${t(`rt.diag.g_${g.id}`)}: ${text}`;
        return h(
          'div',
          { class: 'rtd-row' },
          h('span', { class: 'rtd-name', title: t(`rt.diag.g_${g.id}`) }, t(`rt.diag.g_${g.id}`)),
          labelledBar(g.ratio == null ? null : g.ratio, 1, alt, g.ratio != null && g.ratio >= 0.9 ? 'bf-bad' : 'bf1'),
          h('span', { class: 'num rtd-val wide' }, text),
        );
      }),
      h('p', { class: 'hint' }, t('rt.diag.memory_hint')),
    );
  }

  function lossEl(rows) {
    if (!rows.length) return h('p', { class: 'muted' }, t('rt.diag.no_loss'));
    const hot = rows.filter((r) => r.nonZero);
    const zero = rows.filter((r) => !r.nonZero);
    const item = (r) =>
      h(
        'li',
        { class: r.nonZero ? 'hot' : null },
        h('span', { class: 'rtd-loss-name', title: r.name }, named(`rt.diag.ln_${r.name}`, r.name)),
        r.nonZero ? h('span', { class: 'badge warn' }, h('span', { 'aria-hidden': 'true' }, '⚠'), fmtInt(r.value)) : h('span', { class: 'num muted' }, '0'),
      );
    const zeros = h('details', { class: 'chart-data rtd-zeros', open: lossOpen }, h('summary', t('rt.diag.loss_zero', { count: zero.length })), h('ul', { class: 'rtd-loss' }, zero.map(item)));
    zeros.addEventListener('toggle', () => {
      lossOpen = zeros.open;
    });
    return h(
      'div',
      { class: 'stack-v tight' },
      h('p', { class: hot.length ? 'note warn small' : 'note ok small', role: 'note' }, hot.length ? t('rt.diag.loss_some', { count: hot.length }) : t('rt.diag.loss_none')),
      hot.length ? h('ul', { class: 'rtd-loss' }, hot.map(item)) : null,
      zero.length ? zeros : null,
    );
  }

  function eventsTable(rows) {
    if (!rows.length) return h('p', { class: 'muted' }, t('rt.diag.no_events'));
    return h(
      'table',
      { class: 'compact rtd-table', 'aria-label': t('rt.diag.events') },
      h(
        'thead',
        h(
          'tr',
          ['rt.diag.c_code', 'rt.diag.c_component', 'rt.diag.c_severity', 'rt.diag.c_outcome', 'rt.diag.c_count', 'rt.diag.c_time'].map((k, i) =>
            h('th', { scope: 'col', class: i === 4 ? 'al-right' : '' }, t(k)),
          ),
        ),
      ),
      h(
        'tbody',
        rows.map((e) =>
          h(
            'tr',
            { class: e.outcome === 'failure' ? 'rtd-fail' : null },
            h('th', { scope: 'row', class: 'rtd-key' }, h('code', e.code)),
            h('td', { class: 'rtd-wrap' }, h('code', { class: 'small' }, breakable(e.component))),
            h('td', badge(named(`rt.diag.sev_${e.severity}`, e.severity), severityKind(e.severity))),
            h('td', badge(`${outcomeGlyph(e.outcome)} ${named(`rt.diag.outcome_${e.outcome}`, e.outcome)}`, outcomeKind(e.outcome))),
            h('td', { class: 'al-right' }, num(e.count)),
            h('td', stamp(e.at)),
          ),
        ),
      ),
    );
  }

  function drawEvents() {
    const opts = outcomeOptions(model.events);
    const key = opts.map((o) => `${o.value}:${o.count}`).join('|');
    if (outcome !== 'all' && !opts.some((o) => o.value === outcome)) outcome = 'all';
    if (key !== optionsKey) {
      optionsKey = key;
      swap(
        filterHost,
        h('label', { for: 'rtd-outcome' }, t('rt.diag.filter')),
        select(
          [
            { value: 'all', label: t('rt.diag.all_outcomes', { n: fmtInt(model.events.length) }) },
            ...opts.map((o) => ({ value: o.value, label: `${named(`rt.diag.outcome_${o.value}`, o.value)} (${fmtInt(o.count)})` })),
          ],
          outcome,
          (v) => {
            outcome = v;
            drawEvents();
          },
          { id: 'rtd-outcome' },
        ),
      );
    }
    const rows = filterEvents(model.events, outcome);
    const r = model.retention;
    const base = r
      ? t('rt.diag.events_caption', {
          shown: fmtInt(rows.length),
          total: fmtInt(r.retained ?? model.events.length),
          oldest: r.oldestSeq == null ? '–' : fmtInt(r.oldestSeq),
          latest: r.latestSeq == null ? '–' : fmtInt(r.latestSeq),
        })
      : t('rt.diag.events_caption_short', { shown: fmtInt(rows.length) });
    // the console gathers a bounded number of events: say so when the gateway holds more than were listed
    const caption = eventsTruncated(r, model.events.length) ? `${base} ${t('rt.diag.events_truncated', { gathered: fmtInt(model.events.length) })}` : base;
    if (evCaption.textContent !== caption) evCaption.textContent = caption; // a refresh with the same counts is not announced again
    swap(evWrap, eventsTable(rows));
  }

  function render(d) {
    model = d;
    updated.textContent = t('rt.diag.updated', { time: new Date().toISOString().slice(11, 19) });
    if (!d.available) {
      const r = d.reason;
      swap(stateHost, emptyBox(t(`rt.diag.reason_${r}_title`), t(`rt.diag.reason_${r}_text`), r === 'disabled' ? snippetBlock(DIAGNOSTICS_ON_LINES, t('rt.diag.reason_disabled_fix')) : null));
      swap(body, stateHost);
      body.removeAttribute('aria-busy');
      return;
    }
    swap(noteHost);
    swap(
      metaHost,
      d.version ? h('span', { class: 'chip' }, t('rt.diag.version', { v: d.version })) : null,
      d.capturedAt != null ? h('span', { class: 'chip', title: new Date(d.capturedAt * 1000).toISOString() }, t('rt.diag.captured', { when: `${fmtStamp(d.capturedAt)} UTC` })) : null,
      d.elapsedS != null ? h('span', { class: 'chip' }, t('rt.diag.elapsed', { d: fmtDuration(d.elapsedS) })) : null,
    );
    swap(
      totalsHost,
      totalsTiles(
        d.totals,
        d.lastFailureAt,
        d.components.reduce((a, c) => a + (c.failures ?? 0), 0),
      ),
    );
    swap(compWrap, componentsTable(d.components));
    swap(barsHost, failureBarsEl(d.components));
    swap(memHost, memoryEl(d.memory));
    swap(lossHost, lossEl(d.loss));
    drawEvents();
    if (body.firstChild !== dataEl) swap(body, dataEl);
    body.removeAttribute('aria-busy');
  }

  // ---- polling
  let inflight = 0;
  const live = liveLoad(scope, {
    load: async (signal) => {
      inflight++;
      refreshBtn.disabled = true;
      try {
        return await scope.api.diagnostics({ signal });
      } finally {
        inflight--;
        refreshBtn.disabled = inflight > 0;
      }
    },
    ms: REFRESH_MS,
    onData: (answer, { changed }) => {
      swap(noteHost);
      if (changed || !model || body.getAttribute('aria-busy') === 'true') render(normalizeDiagnostics(answer));
      else updated.textContent = t('rt.diag.updated', { time: new Date().toISOString().slice(11, 19) });
    },
    onError: (e, { quiet }) => {
      if (model && quiet) return;
      if (model && model.available) {
        swap(noteHost, h('p', { class: 'note warn', role: 'status' }, t('rt.diag.refresh_failed')));
        return;
      }
      model = null;
      // a 403 on a GET is the role check: the page believed it was an admin (or the key was changed in another tab)
      swap(body, e?.status === 403 ? emptyBox(t('rt.diag.admin_only_title'), t('rt.diag.admin_only')) : errorBox(e, () => live.refresh()));
      body.removeAttribute('aria-busy');
    },
  });
  live.refresh();
  return { el, refresh: () => live.refresh() };
}
