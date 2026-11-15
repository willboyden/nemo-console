// The NVIDIA Flare Status additions (addendum 9.16): the target grid (every target the launcher knows: where it runs, on which chip, in what state, how many packages are prepared, what it serves),
// and under the page's window picker, from ONE series request split by target: generated tokens per second and requests per second per target (rates of the engine counters), prefill vs generated vs
// cache-reused tokens as stacked bars, and active vs pending requests per target. The engine counters exist only while a target serves: a target that does not serve has no line, and a figure it
// does not report is an en dash, never 0. Words: i18n keys `flv.*`. Everything from the server is text.
import { h } from '../dom.js';
import { t } from '../i18n.js';
import { describeError } from '../errors.js';
import { fmtInt, fmtTick, fmtCompact } from '../format.js';
import { badge } from '../components/ui.js';
import { linesCard } from '../charts/lines-card.js';
import { stackCard } from '../charts/stack-card.js';
import { swap } from '../charts/parts.js';
import { FLARE_KEYS, TARGET_STATES, targetGrid, rateLines, tokenMix, loadByTarget, targetsWithData, MIX_KEYS } from '../flare-model.js';

const GAP_S = { '15m': 5, '1h': 15, '24h': 30, '7d': 60 };
const STATE_KIND = { serving: 'ok', loading: 'info', busy: 'warn', failed: 'bad', stopped: 'muted', absent: 'muted', unknown: 'muted' };
const STATE_GLYPH = { serving: '✓', loading: '◌', busy: '!', failed: '✕', stopped: '–', absent: '–', unknown: '?' };

const tok = (v) => (v == null ? '–' : fmtCompact(v));
const rateText = (per) => (v) => (v == null ? '–' : `${v >= 100 ? Math.round(v) : fmtTick(+v.toFixed(2))}/${per === 'min' ? 'min' : 's'}`);
const kv = (k, v) => (v == null || v === '' ? null : h('div', { class: 'kv' }, h('dt', k), h('dd', v)));

/** One target's tile: its name, the state (a glyph and a word as well as a colour), where it runs and what it serves. */
export function targetTile(tg) {
  const serving = tg.state === 'serving';
  return h(
    'article',
    { class: `fl-target st-${tg.state}`, 'aria-label': t('flv.target_aria', { target: tg.target, state: t(`flv.state_${tg.state}`) }) },
    h('header', { class: 'row gap between' }, h('strong', { class: 'fl-target-name' }, tg.target), badge(`${STATE_GLYPH[tg.state]} ${t(`flv.state_${tg.state}`)}`, STATE_KIND[tg.state])),
    h(
      'dl',
      { class: 'facts fl-facts' },
      kv(t('flv.hosts'), tg.hosts.length ? tg.hosts.join(', ') : null),
      kv(t('flv.chip'), tg.chip ? `${tg.chip}${tg.family ? ` (${tg.family})` : ''}` : null),
      kv(t('flv.modes'), tg.modes.length ? tg.modes.join(', ') : null),
      kv(t('flv.packages'), tg.packages == null ? '–' : String(Math.round(tg.packages))),
      serving ? kv(t('flv.serving'), tg.model ? `${tg.model}${tg.context != null ? ` · ${t('flv.context', { tokens: fmtInt(tg.context) })}` : ''}` : '?') : null,
      !serving && tg.state === 'busy' ? kv(t('flv.gpu'), tg.busyWhy ? t('flv.gpu_busy_why', { why: tg.busyWhy }) : t('flv.gpu_busy')) : null,
      tg.imageLoaded == null ? null : kv(t('flv.image'), tg.imageLoaded ? t('flv.image_loaded') : t('flv.image_missing')),
      tg.error ? kv(t('flv.problem'), tg.error) : null,
    ),
  );
}

/** The grid card: tiles grouped by chip, the state counts in words. Returns {el, update(status, points)}. */
function gridCard() {
  const summary = h('p', { class: 'fl-summary', role: 'status' });
  const groups = h('div', { class: 'stack-v' });
  const el = h(
    'section',
    { class: 'card chart-card fl-grid', 'aria-label': t('flv.grid_title') },
    h('header', { class: 'card-head' }, h('h3', t('flv.grid_title'))),
    h('div', { class: 'card-body stack-v' }, h('p', { class: 'hint' }, t('flv.grid_hint')), summary, groups),
  );
  let sig = '';
  return {
    el,
    update(status, points) {
      const m = targetGrid(status, points);
      const next = JSON.stringify(m);
      if (next === sig) return;
      sig = next;
      if (m.empty) {
        swap(summary, t('flv.grid_none'));
        swap(groups);
        return;
      }
      const parts = TARGET_STATES.filter((s) => m.counts[s]).map((s) => `${m.counts[s]} ${t(`flv.state_${s}`).toLowerCase()}`);
      swap(summary, t('flv.grid_summary', { count: m.targets.length, states: parts.join(', ') }));
      swap(
        groups,
        m.groups.map((g) =>
          h(
            'section',
            { class: 'fl-group', 'aria-label': g.chip || t('flv.chip_unknown') },
            h(
              'h4',
              { class: 'sub fl-group-head' },
              g.chip ? t('flv.chip_group', { chip: g.chip, family: g.targets[0]?.family ?? '–' }) : t('flv.chip_unknown'),
              ' ',
              h('span', { class: 'muted small' }, t('flv.group_count', { count: g.targets.length })),
            ),
            h('div', { class: 'fl-tiles' }, g.targets.map(targetTile)),
          ),
        ),
      );
    },
  };
}

/** `desc.extras(scope, shared)` of the Flare page: returns {el}. */
export function flareExtras(scope, shared) {
  const win = () => shared.win();
  const grid = gridCard();
  const rates = linesCard(scope, {
    title: t('flv.rates_title'),
    hint: t('flv.rates_hint'),
    win,
    wide: true,
    panels: [
      { id: 'gen', title: t('flv.rate_tokens'), yFmt: rateText('s'), valueFmt: rateText('s') },
      { id: 'req', title: t('flv.rate_requests'), yFmt: rateText('s'), valueFmt: rateText('s') },
    ],
  });
  const mix = stackCard(scope, {
    title: t('flv.mix_title'),
    hint: t('flv.mix_hint'),
    parts: [
      { key: 'prefill', name: t('flv.mix_prefill'), cls: 'bf1' },
      { key: 'generated', name: t('flv.mix_generated'), cls: 'bf2' },
      { key: 'reused', name: t('flv.mix_reused'), cls: 'bf3' },
    ],
    valueFmt: tok,
    axisFmt: (v) => fmtCompact(v),
    rowsTitle: t('flv.mix_window'),
    timeTitle: t('flv.mix_over_time'),
    emptyText: t('flv.no_counters'),
    wide: true,
  });
  const load = stackCard(scope, {
    title: t('flv.load_title'),
    hint: t('flv.load_hint'),
    parts: [
      { key: 'active', name: t('flv.load_active'), cls: 'bf1' },
      { key: 'pending', name: t('flv.load_pending'), cls: 'bf5' },
    ],
    valueFmt: (v) => (v == null ? '–' : String(Math.round(v * 10) / 10)),
    axisFmt: (v) => (Number.isInteger(v) ? String(v) : ''),
    rowsTitle: t('flv.load_now'),
    timeTitle: t('flv.load_over_time'),
    emptyText: t('flv.no_counters'),
    wide: true,
  });
  const noteHost = h('div');
  const el = h(
    'div',
    { class: 'stack-v fl-viz' },
    h('h2', t('flv.title')),
    grid.el,
    h('div', { class: 'row gap wrap between' }, h('h2', t('flv.engine_title')), h('span', { class: 'muted small' }, t('flv.engine_hint'))),
    noteHost,
    rates.el,
    mix.el,
    load.el,
  );

  // ---- the grid follows the page's polled status; the package counts come from the metrics snapshot
  let status = null,
    points = [];
  shared.ctx?.onStatus((st) => {
    status = st;
    grid.update(status, points);
  });
  async function loadPoints() {
    try {
      const m = await scope.api.metrics('flare');
      if (!scope.alive) return;
      points = m.available ? m.points : [];
      if (status) grid.update(status, points);
    } catch {
      /* the grid still has the package counts in the status text */
    }
  }

  // ---- the series
  let last = 0,
    busy = false;
  async function loadSeries(force = false) {
    const w = win();
    if (busy || (!force && Date.now() - last < (GAP_S[w] ?? 15) * 1000)) return;
    busy = true;
    last = Date.now();
    try {
      const a = await scope.api.series('flare', { keys: FLARE_KEYS, window: w, split: true });
      if (!scope.alive || w !== win()) return;
      const targets = targetsWithData(a);
      swap(
        noteHost,
        targets.length ? null : h('p', { class: 'note info', role: 'status' }, t('flv.none_serving')),
        a.truncated ? h('p', { class: 'note info', role: 'status' }, t('series.truncated')) : null,
      );
      // rates: generated tokens per second, completed requests per second (a failed-request line only where one exists)
      const gen = rateLines(a, 'flare.generated_total');
      const done = rateLines(a, 'flare.requests_completed_total');
      const fail = rateLines(a, 'flare.requests_failed_total');
      const failLines = fail.lines.filter((l) => l.segments.some((s) => s.some((p) => p[1] > 0))).map((l, i) => ({ ...l, name: t('flv.failed_line', { target: l.name }), idx: done.lines.length + i }));
      rates.update({
        from: a.from,
        to: a.to,
        panels: [
          { lines: gen.lines, title: gen.per === 'min' ? t('flv.rate_tokens_min') : t('flv.rate_tokens'), yFmt: rateText(gen.per), valueFmt: rateText(gen.per) },
          { lines: [...done.lines, ...failLines], title: done.per === 'min' ? t('flv.rate_requests_min') : t('flv.rate_requests'), yFmt: rateText(done.per), valueFmt: rateText(done.per) },
        ],
      });
      const m = tokenMix(a);
      mix.update({
        rows: m.rows.map((r) => ({
          id: r.target,
          label: r.target,
          values: r.values,
          totalText: tok(MIX_KEYS.reduce((acc, k) => acc + (r.values[k] ?? 0), 0)),
          note: r.resets ? t('flv.resets_note', { target: r.target, count: r.resets }) : null,
        })),
        time: m.time.filter((x) => x.series.length).map((x) => ({ label: x.target, ts: x.ts, bucketS: x.bucketS, series: x.series })),
      });
      const l = loadByTarget(a);
      load.update({
        rows: l.rows.map((r) => ({
          id: r.target,
          label: r.target,
          values: r.values,
          totalText: t('flv.load_peak', { active: r.peakActive == null ? '–' : Math.round(r.peakActive), pending: r.peakPending == null ? '–' : Math.round(r.peakPending) }),
          note: null,
        })),
        time: l.time.filter((x) => x.series.length).map((x) => ({ label: x.target, ts: x.ts, bucketS: x.bucketS, series: x.series })),
      });
    } catch (e) {
      if (scope.alive) swap(noteHost, h('p', { class: 'note warn', role: 'status' }, describeError(e)));
    } finally {
      busy = false;
    }
  }
  shared.onWindow(() => loadSeries(true));
  loadPoints();
  loadSeries(true);
  scope.every(10000, () => {
    loadPoints();
    loadSeries(false);
  });
  return { el };
}
