// Capture tab: how much of what Relay can record is switched on here. A coverage meter, a matrix of the nine capture
// levels (enabled, state, what is configured, what was observed), the three OTLP projections and the hints as a to-do
// list. `detail` and `observed` have fixed keys per level (docs/ARCHITECTURE.md 9.12, "Pinned details"); describeLevel()
// (pure.js) types them and anything it does not know falls back to the generic renderer. Everything is text.
import { h } from '../dom.js';
import { t } from '../i18n.js';
import { fmtInt, fmtBytes, timeAgo } from '../format.js';
import { badge, card, errorBox, skeleton } from '../components/ui.js';
import { fmtStamp, fmtDur } from '../viz/fmt.js';
import { createNav } from './nav.js';
import { liveLoad } from './live.js';
import { dash, swap } from './ui.js';
import { coverage, describeLevel } from './pure.js';

export { coverage };

const STATE_KIND = { active: 'ok', idle: 'info', stale: 'warn', off: 'muted', unknown: 'muted' };
const STATE_GLYPH = { active: '●', idle: '◐', stale: '◔', off: '○', unknown: '?' };
const GROUPS = [
  ['rt.cap.g_files', ['atof', 'atif']],
  ['rt.cap.g_otel', ['otel_traces', 'otel_logs', 'otel_metrics']],
  ['rt.cap.g_payload', ['payloads', 'redaction']],
  ['rt.cap.g_enrich', ['resource_metrics', 'pricing']],
];
const PROJ = ['gen_ai', 'openinference', 'full'];

export const stateChip = (s) =>
  h(
    'span',
    { class: `badge ${STATE_KIND[s] ?? 'muted'} cap-state st-${s}`, title: t(`rt.cap.state_${s}_tip`) === `rt.cap.state_${s}_tip` ? undefined : t(`rt.cap.state_${s}_tip`) },
    h('span', { 'aria-hidden': 'true' }, STATE_GLYPH[s] ?? '?'),
    t(`rt.cap.state_${s}`) === `rt.cap.state_${s}` ? String(s) : t(`rt.cap.state_${s}`),
  );

const isEndpoint = (o) => o && typeof o === 'object' && !Array.isArray(o) && ('target' in o || 'host' in o) && ('projection' in o || 'reachable' in o || 'transport' in o);
/** `reachable`: true / false for a loopback endpoint the console could probe, null = not checked (anything else). */
const reachChip = (reach) =>
  reach === true ? badge(`✓ ${t('rt.cap.reachable')}`, 'ok') : reach === false ? badge(`✕ ${t('rt.cap.unreachable')}`, 'bad') : h('span', { class: 'muted small' }, t('rt.cap.reach_unknown'));
function endpoint(o) {
  const meta = [
    o.service_name ? h('span', { class: 'muted small', title: t('rt.cap.service_tip') }, t('rt.cap.service', { name: o.service_name })) : null,
    'reachable' in o ? reachChip(o.reachable) : null,
  ].filter(Boolean);
  return h(
    'li',
    { class: 'cap-ep' },
    h(
      'div',
      { class: 'cap-ep-main' },
      o.projection ? badge(String(o.projection), 'info') : null,
      h('code', { class: 'small' }, String(o.target ?? o.host ?? '–')),
      o.transport ? h('span', { class: 'chip' }, String(o.transport)) : null,
    ),
    meta.length ? h('div', { class: 'cap-ep-meta' }, ...meta) : null,
  );
}
const keyLabel = (key) => (t(`rt.cap.k_${key}`) === `rt.cap.k_${key}` ? String(key).replace(/_/g, ' ') : t(`rt.cap.k_${key}`));
const MODE_KEY = { full: 'rt.cap.mode_full', turn_filtered: 'rt.cap.mode_turn_filtered' };

/** One typed fact (pure.js describeLevel) -> [label, value nodes]. */
function factRow(f) {
  switch (f.type) {
    case 'sinks':
      return [
        keyLabel('sinks'),
        h(
          'ul',
          { class: 'cap-eps' },
          f.items.map((x) =>
            h(
              'li',
              { class: 'cap-ep' },
              h(
                'div',
                { class: 'cap-ep-main' },
                x?.type ? badge(String(x.type), 'info') : null,
                x?.file ? h('code', { class: 'small' }, String(x.file)) : null,
                x?.mode ? h('span', { class: 'chip' }, String(x.mode)) : null,
              ),
            ),
          ),
        ),
      ];
    case 'endpoints':
      return [
        keyLabel('endpoints'),
        h(
          'ul',
          { class: 'cap-eps' },
          f.items.map((x) => endpoint(x)),
        ),
      ];
    case 'derived':
      return [
        keyLabel('endpoints'),
        h(
          'div',
          { class: 'stack-v tight' },
          h('span', t('rt.cap.derived', { n: f.endpoints })),
          f.targets?.length
            ? h(
                'span',
                { class: 'chips' },
                f.targets.map((x) => h('span', { class: 'chip', title: x?.reachable === false ? t('rt.cap.unreachable') : undefined }, String(x?.target ?? x))),
              )
            : null,
        ),
      ];
    case 'mode':
      return [keyLabel('mode'), MODE_KEY[f.value] ? t(MODE_KEY[f.value]) : String(f.value)];
    case 'chips':
      return [
        keyLabel(f.key),
        h(
          'span',
          { class: 'chips' },
          f.items.map((x) => h('span', { class: 'chip' }, String(x))),
        ),
      ];
    case 'ms':
      return [keyLabel(f.key), typeof f.value === 'number' ? t('rt.cap.every', { d: fmtDur(f.value) }) : String(f.value)];
    case 'bool':
      return [keyLabel(f.key), badge(f.value ? t('common.on') : t('common.off'), f.value ? 'ok' : 'muted')];
    case 'text':
      return [keyLabel(f.key), String(f.value)];
    case 'ts':
      return [keyLabel(f.key), h('span', { class: 'num', title: new Date(f.value * 1000).toISOString() }, `${fmtStamp(f.value)} UTC · ${timeAgo(f.value)}`)];
    case 'count':
      return [keyLabel(f.key), h('span', { class: 'num' }, fmtInt(f.value))];
    case 'bytes':
      return [keyLabel(f.key), h('span', { class: 'num' }, fmtBytes(f.value))];
    default:
      return [keyLabel(f.key), renderValue(f.value, f.key, 1)];
  }
}
const factList = (facts) =>
  h(
    'dl',
    { class: 'facts rt-facts tight' },
    facts.map((f) => {
      const [k, v] = factRow(f);
      return h('div', { class: 'kv' }, h('dt', k), h('dd', v));
    }),
  );

/** Render any JSON value as text-only nodes. Keys that name a time, a size or a count get a unit. */
export function renderValue(v, key = '', depth = 0) {
  if (v == null) return dash();
  if (typeof v === 'boolean') return badge(v ? t('common.yes') : t('common.no'), v ? 'ok' : 'muted');
  if (typeof v === 'number') {
    if (/bytes$/.test(key)) return fmtBytes(v);
    if (/(^|_)ts$/.test(key)) return h('span', { class: 'num', title: new Date(v * 1000).toISOString() }, `${fmtStamp(v)} UTC · ${timeAgo(v)}`);
    return h('span', { class: 'num' }, fmtInt(v));
  }
  if (typeof v === 'string') return v;
  if (Array.isArray(v)) {
    if (!v.length) return dash();
    if (v.every(isEndpoint)) return h('ul', { class: 'cap-eps' }, v.map(endpoint));
    if (v.every((x) => x == null || typeof x !== 'object'))
      return h(
        'span',
        { class: 'chips' },
        v.map((x) => h('span', { class: 'chip' }, String(x))),
      );
    return h(
      'ul',
      { class: 'plain-list' },
      v.map((x) => h('li', renderValue(x, '', depth + 1))),
    );
  }
  if (isEndpoint(v)) return h('ul', { class: 'cap-eps' }, endpoint(v));
  if (depth > 3) return h('code', { class: 'small' }, '{…}');
  const rows = Object.entries(v);
  if (!rows.length) return dash();
  return h(
    'dl',
    { class: 'facts rt-facts tight' },
    rows.map(([k, x]) => h('div', { class: 'kv' }, h('dt', keyLabel(k)), h('dd', renderValue(x, k, depth + 1)))),
  );
}

export function captureTab(scope, ctx) {
  const nav = createNav('capture', ctx);
  const body = h('div', { class: 'stack-v', 'aria-busy': 'true' }, skeleton(5));
  function render(c) {
    const levels = c.levels || [];
    const cov = coverage(levels);
    const byId = Object.fromEntries(levels.map((l) => [l.id, l]));
    const meter = h(
      'div',
      { class: 'cap-meter', role: 'img', 'aria-label': t('rt.cap.meter_alt', { on: cov.on, total: cov.total }) },
      levels.map((l) =>
        h(
          'span',
          { class: `cap-cell st-${l.state}${l.enabled ? ' on' : ''}`, title: `${l.label}: ${t(`rt.cap.state_${l.state}`) === `rt.cap.state_${l.state}` ? l.state : t(`rt.cap.state_${l.state}`)}` },
          h('span', { class: 'sr-only' }, l.label),
        ),
      ),
    );
    const head = card(t('rt.cap.title'), [
      h(
        'div',
        { class: 'row gap wrap between' },
        h('div', { class: 'cap-big' }, h('strong', { class: 'num' }, `${cov.on} / ${cov.total}`), h('span', { class: 'muted' }, ` ${t('rt.cap.switched_on')}`)),
        h('span', { class: 'muted small' }, c.relay_version ? t('rt.cap.version', { v: c.relay_version }) : ''),
      ),
      meter,
      h(
        'div',
        { class: 'row gap wrap small' },
        ['active', 'idle', 'stale', 'unknown', 'off'].filter((s) => cov.by[s]).map((s) => h('span', { class: 'row gap' }, stateChip(s), h('span', { class: 'num' }, `× ${cov.by[s]}`))),
      ),
      h('p', { class: 'hint' }, t('rt.cap.explain')),
    ]);
    const rows = [];
    for (const [gk, ids] of GROUPS) {
      const ls = ids.map((i) => byId[i]).filter(Boolean);
      if (!ls.length) continue;
      rows.push(h('tr', { class: 'grp' }, h('th', { colspan: 5, scope: 'rowgroup' }, t(gk))));
      for (const l of ls) rows.push(levelRow(l, levels));
    }
    const other = levels.filter((l) => !GROUPS.some(([, ids]) => ids.includes(l.id)));
    if (other.length) {
      rows.push(h('tr', { class: 'grp' }, h('th', { colspan: 5, scope: 'rowgroup' }, t('rt.cap.g_other'))));
      for (const l of other) rows.push(levelRow(l, levels));
    }
    const matrix = card(t('rt.cap.matrix'), [
      h(
        'div',
        { class: 'tablewrap' },
        h(
          'table',
          { class: 'compact cap-table', 'aria-label': t('rt.cap.matrix') },
          h(
            'thead',
            h(
              'tr',
              ['rt.cap.c_level', 'rt.cap.c_enabled', 'rt.cap.c_state', 'rt.cap.c_configured', 'rt.cap.c_observed'].map((k) => h('th', { scope: 'col' }, t(k))),
            ),
          ),
          h('tbody', rows),
        ),
      ),
    ]);
    const proj = c.projections || {};
    const projections = card(t('rt.cap.projections'), [
      h('p', { class: 'hint' }, t('rt.cap.projections_hint')),
      h(
        'div',
        { class: 'grid three' },
        PROJ.map((p) =>
          h(
            'div',
            { class: `proj-tile${proj[p] ? ' on' : ''}` },
            h('div', { class: 'row gap between' }, h('strong', p), proj[p] ? badge(`✓ ${t('rt.cap.proj_on')}`, 'ok') : badge(`○ ${t('rt.cap.proj_off')}`, 'muted')),
            h('p', { class: 'small muted' }, t(`rt.cap.proj_${p}`)),
          ),
        ),
      ),
    ]);
    const hints = c.hints || [];
    const todo = card(t('rt.cap.hints'), [
      hints.length
        ? h(
            'ul',
            { class: 'todo' },
            hints.map((x) => h('li', h('span', { class: 'todo-box', 'aria-hidden': 'true' }, '☐'), h('div', h('div', x.message), h('code', { class: 'muted small' }, x.code)))),
          )
        : h('p', { class: 'note ok' }, t('rt.cap.no_hints')),
      h('p', { class: 'hint' }, t('rt.cap.hints_hint')),
    ]);
    swap(body, head, matrix, projections, todo);
    body.removeAttribute('aria-busy');
  }
  function levelRow(l, levels) {
    const d = describeLevel(l, levels);
    return h(
      'tr',
      { class: `cap-row st-${l.state}` },
      h('th', { scope: 'row', class: 'cap-level' }, h('div', l.label), h('code', { class: 'muted small' }, l.id)),
      h('td', l.enabled ? badge(`✓ ${t('rt.cap.on')}`, 'ok') : badge(`✕ ${t('rt.cap.off')}`, 'muted')),
      h('td', stateChip(l.state)),
      h(
        'td',
        d.configured.length ? factList(d.configured) : h('span', { class: 'muted' }, l.enabled ? t('rt.cap.no_detail') : t('rt.cap.not_configured')),
        ...d.warnings.map((w) => h('p', { class: 'note warn small cap-warn', role: 'note' }, t('rt.cap.dup_export', { n: w.n }))),
      ),
      h('td', d.observed.length ? factList(d.observed) : h('span', { class: 'muted' }, l.state === 'unknown' ? t('rt.cap.not_observable') : t('rt.cap.nothing_seen'))),
    );
  }
  const live = liveLoad(scope, {
    load: (signal) => scope.api.capture({ signal }),
    ms: 15000,
    onData: (c, { changed }) => {
      if (changed || body.getAttribute('aria-busy') === 'true') render(c);
    },
    onError: (e, { quiet }) => {
      if (!quiet || body.getAttribute('aria-busy') === 'true')
        swap(
          body,
          errorBox(e, () => live.refresh()),
        );
    },
  });
  live.refresh();
  void nav;
  return h('div', { class: 'stack-v rt-tab' }, body);
}
