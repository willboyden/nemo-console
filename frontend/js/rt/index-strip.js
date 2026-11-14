// The header strip of the explorer tabs: state of the trace index (building / ready / empty / disabled) with a progress
// bar while it builds, the coverage range, counts, a note when truncated, the index's own problems and its files.
import { swap } from './ui.js';
import { h } from '../dom.js';
import { t } from '../i18n.js';
import { fmtInt, fmtBytes, timeAgo } from '../format.js';
import { badge, emptyBox, errorBox, fixBlock } from '../components/ui.js';
import { fmtStamp, fmtDur } from '../viz/fmt.js';
import { liveLoad } from './live.js';
import { navLink } from './nav.js';
import { buildProgress } from './pure.js';

export { buildProgress };

const STATE_KIND = { ready: 'ok', building: 'info', empty: 'muted', disabled: 'warn' };
const STATE_GLYPH = { ready: '●', building: '◐', empty: '○', disabled: '⏸' };

export const hasData = (index) => !!index && (index.state === 'ready' || index.state === 'building' || index.traces > 0);

function stat(label, value, sub) {
  return h('span', { class: 'ix-stat' }, h('b', { class: 'ix-v num' }, value), h('span', { class: 'ix-l' }, label), sub ? h('span', { class: 'ix-s' }, `(${sub})`) : null);
}

function filesTable(index) {
  if (!index.files?.length) return h('p', { class: 'hint' }, t('rt.index.no_files'));
  return h(
    'div',
    { class: 'tablewrap' },
    h(
      'table',
      { class: 'compact' },
      h(
        'thead',
        h(
          'tr',
          ['rt.index.file', 'rt.index.size', 'rt.index.scanned', 'rt.index.events', 'rt.index.state', 'rt.index.skipped'].map((k, i) =>
            h('th', { scope: 'col', class: i > 0 && i !== 4 ? 'al-right' : '' }, t(k)),
          ),
        ),
      ),
      h(
        'tbody',
        index.files.map((f) =>
          h(
            'tr',
            h('td', h('code', f.name), f.compressed ? h('span', { class: 'muted small' }, ` ${t('rt.index.gz')}`) : null),
            h('td', { class: 'num al-right' }, fmtBytes(f.bytes)),
            h('td', { class: 'num al-right' }, fmtBytes(f.scanned_bytes)),
            h('td', { class: 'num al-right' }, fmtInt(f.events)),
            h(
              'td',
              badge(
                t(`rt.index.fstate.${f.state}`) === `rt.index.fstate.${f.state}` ? String(f.state) : t(`rt.index.fstate.${f.state}`),
                f.state === 'indexed' ? 'ok' : f.state === 'error' ? 'bad' : f.state === 'skipped' ? 'warn' : 'info',
              ),
            ),
            h('td', { class: 'num al-right' }, fmtInt(f.skipped_lines)),
          ),
        ),
      ),
    ),
  );
}

/** Mounts the strip into a host and keeps it fresh; onIndex(index, {changed}) tells the tab what the index says. */
export function indexStrip(scope, nav, { onIndex } = {}) {
  const host = h('section', { class: 'card ix-strip', 'aria-label': t('rt.index.title') });
  const body = h('div', { class: 'ix-body' });
  host.append(body);
  let openDetails = false;
  const draw = (ix) => {
    const st = ix.state;
    const prog = st === 'building' ? buildProgress(ix) : null;
    const cov =
      ix.coverage?.from_ts != null && ix.coverage?.to_ts != null
        ? `${fmtStamp(ix.coverage.from_ts)} → ${fmtStamp(ix.coverage.to_ts)} UTC · ${fmtDur((ix.coverage.to_ts - ix.coverage.from_ts) * 1000)}`
        : null;
    const problems = ix.problems || [];
    const details = h(
      'details',
      { class: 'ix-details' },
      h('summary', problems.length ? t('rt.index.details_notes', { n: problems.length }) : t('rt.index.details')),
      h(
        'div',
        { class: 'stack-v' },
        problems.length
          ? h(
              'ul',
              { class: 'ix-problems' },
              problems.map((p) => h('li', { class: 'note info' }, h('code', { class: 'small' }, p.code), ' ', p.message)),
            )
          : null,
        filesTable(ix),
        h('p', { class: 'hint' }, t('rt.index.limits', { scan: fmtBytes(ix.limits?.scan_bytes), spans: fmtInt(ix.limits?.max_spans), line: fmtBytes(ix.limits?.max_line_bytes) })),
      ),
    );
    details.open = openDetails;
    details.addEventListener('toggle', () => (openDetails = details.open));
    const top = h(
      'div',
      { class: 'row gap wrap between' },
      h(
        'div',
        { class: 'row gap wrap' },
        h(
          'span',
          { class: `badge ${STATE_KIND[st] ?? 'muted'} ix-state` },
          h('span', { 'aria-hidden': 'true' }, STATE_GLYPH[st] ?? '?'),
          t(`rt.index.state_${st}`) === `rt.index.state_${st}` ? String(st) : t(`rt.index.state_${st}`),
        ),
        cov ? h('span', { class: 'muted small num' }, t('rt.index.coverage', { range: cov })) : h('span', { class: 'muted small' }, t('rt.index.no_coverage')),
        ix.content === 'off' ? badge(t('rt.index.content_off'), 'warn', t('rt.index.content_off_tip')) : null,
      ),
      h(
        'span',
        { class: 'muted small' },
        ix.last_refresh_ts != null
          ? ix.build_ms != null
            ? t('rt.index.refreshed', { ago: timeAgo(ix.last_refresh_ts), ms: fmtDur(ix.build_ms) })
            : t('rt.index.refreshed_only', { ago: timeAgo(ix.last_refresh_ts) })
          : '',
      ),
    );
    const progress =
      st === 'building'
        ? h(
            'div',
            { class: 'ix-progress' },
            h(
              'div',
              { class: 'progress', role: 'progressbar', 'aria-label': t('rt.index.progress'), 'aria-valuemin': 0, 'aria-valuemax': 100, 'aria-valuenow': prog == null ? null : Math.round(prog * 100) },
              (() => {
                const i = h('i', { class: prog == null ? 'indeterminate' : '' });
                if (prog != null) i.style.width = `${Math.round(prog * 100)}%`;
                return i;
              })(),
            ),
            h('span', { class: 'muted small' }, prog == null ? t('rt.index.building') : t('rt.index.building_pct', { pct: Math.round(prog * 100) })),
          )
        : null;
    const stats = h(
      'div',
      { class: 'ix-stats' },
      stat(t('rt.index.traces'), fmtInt(ix.traces)),
      stat(t('rt.index.sessions'), fmtInt(ix.sessions)),
      stat(t('rt.index.spans'), fmtInt(ix.spans), ix.open_spans ? t('rt.index.open_n', { n: fmtInt(ix.open_spans) }) : null),
      stat(t('rt.index.marks'), fmtInt(ix.marks)),
      stat(t('rt.index.events'), fmtInt(ix.events)),
      stat(t('rt.index.trajectories'), fmtInt(ix.trajectories)),
    );
    const notes = [];
    // Ready, but the newest event is more than a day old: an idle lab, not a broken page.
    const newest = ix.coverage?.to_ts;
    if (st === 'ready' && newest != null && Date.now() / 1000 - newest > 86400)
      notes.push(h('p', { class: 'note info ix-stale', role: 'status' }, t('rt.index.stale', { when: `${fmtStamp(newest)} UTC`, ago: timeAgo(newest) })));
    if (ix.truncated) notes.push(h('p', { class: 'note warn', role: 'status' }, h('strong', t('rt.index.truncated')), ' ', t('rt.index.truncated_hint')));
    swap(body, top, progress, st === 'disabled' ? null : stats, ...notes, st === 'disabled' ? null : details);
  };
  let current = null;
  const live = liveLoad(scope, {
    load: (signal) => scope.api.traceIndex({ signal }),
    ms: 4000,
    onData: (ix, meta) => {
      current = ix;
      if (meta.changed) draw(ix);
      onIndex?.(ix, meta);
    },
    onError: (e, { quiet }) => {
      if (!quiet || !current)
        swap(
          body,
          errorBox(e, () => live.refresh()),
        );
    },
  });
  live.refresh();
  return { el: host, current: () => current, refresh: live.refresh };
}

/** The explanation shown in place of the lists when the index is disabled or empty (null when there is data to show). */
export function indexGate(index, nav, ctxTab = 'capture') {
  if (!index) return null;
  if (index.state === 'disabled')
    return emptyBox(
      t('rt.index.disabled_title'),
      t('rt.index.disabled_hint'),
      h('div', { class: 'stack-v ix-how' }, h('p', { class: 'hint' }, t('rt.index.disabled_how')), fixBlock('NEMO_CONSOLE_RELAY_TRACE_INDEX=on', t('rt.index.setting_label'))),
    );
  if (index.state === 'empty') return emptyBox(t('rt.index.empty_title'), t('rt.index.empty_hint'), navLink(nav, {}, { class: 'btn sm' }, t('rt.index.open_capture'), ctxTab));
  return null;
}
