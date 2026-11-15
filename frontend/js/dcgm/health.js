// The GPU health cards of the DCGM Status tab (addendum 9.14): really busy?, memory (used / reserved / free), power against its limit,
// energy over the window, throttle violations by reason, PCIe replays are charts of the generic page, remapped rows, and the XID and
// diagnostics timeline. They follow the page's window picker (`shared`), fetch what they need (three series requests, the metrics
// snapshot, the events) and are updated IN PLACE on every refresh, so an open data table or a selected event survives it. A GPU the
// answer has no figure for shows an en dash, never 0. Words: i18n keys `gpu.*`; every value from the server is text.
import { h } from '../dom.js';
import { t } from '../i18n.js';
import { describeError } from '../errors.js';
import { fmtRatio, fmtInt, fmtUnit, fmtDuration } from '../format.js';
import { badge, stat } from '../components/ui.js';
import { valueSegments } from '../series-model.js';
import { groupByGpu, busyModel, memoryModel, powerModel, energyModel, energyTotal, violationModel, remapModel, VIOLATION_REASONS } from '../dcgm-model.js';
import { WINDOW_SPAN_S } from '../series-math.js';
import { linesCard } from '../charts/lines-card.js';
import { stackCard } from '../charts/stack-card.js';
import { timelineCard } from './timeline-card.js';
import { swap, windowPhrase } from '../charts/parts.js';

const BUSY_KEYS = ['dcgm.gpu_util', 'dcgm.gr_active', 'dcgm.tensor_active', 'dcgm.dram_active', 'dcgm.sm_clock_mhz', 'dcgm.enc_util', 'dcgm.dec_util'];
const MEM_KEYS = ['dcgm.fb_used_b', 'dcgm.fb_reserved_b', 'dcgm.fb_free_b', 'dcgm.power_w', 'dcgm.power_limit_w', 'dcgm.energy_mj_total'];
const VIOL_KEYS = ['dcgm.violation_us_total'];
const VERDICT_KIND = { idle: 'muted', busy: 'ok', light: 'info', misleading: 'warn', util_only: 'muted', unknown: 'muted' };
/** Minimum seconds between refreshes by window: the long windows are 1-minute / 10-minute buckets, so asking every 10 s is wasted. */
const GAP_S = { '15m': 10, '1h': 30, '24h': 60, '7d': 120 };

const pct = (v) => (v == null ? '–' : fmtRatio(v, 0));
const watts = (v) => (v == null ? '–' : `${v.toFixed(0)} W`);
const gib = (v) => (v == null ? '–' : `${v.toFixed(v < 10 ? 2 : 1)} GiB`);
const gpuTitle = (g) => (g.model ? t('gpu.card_title_model', { gpu: g.gpu, model: g.model }) : t('gpu.card_title', { gpu: g.gpu }));

/** A bar for a ratio inside a tile row (decorative: the figure beside it is the content, and the bar has the aria-label). */
function ratioBar(name, v) {
  const fill = h('i');
  fill.style.width = v == null ? '0%' : `${Math.min(100, Math.max(0, v * 100)).toFixed(1)}%`;
  return h(
    'div',
    { class: 'busy-row' },
    h('span', { class: 'busy-name' }, name),
    h('span', { class: `busy-bar${v == null ? ' none' : ''}`, role: 'img', 'aria-label': `${name}: ${pct(v)}` }, fill),
    h('span', { class: 'num busy-val' }, pct(v)),
  );
}

/** Keep one card per GPU: created when a GPU first appears, kept (with its state) across refreshes, in GPU order. */
function perGpu(host, make, emptyText) {
  const cards = new Map();
  return {
    sync(groups) {
      const ids = groups.map((g) => g.gpu);
      if (!ids.length) {
        cards.clear();
        swap(host, h('p', { class: 'muted' }, emptyText));
        return;
      }
      for (const g of groups) if (!cards.has(g.gpu)) cards.set(g.gpu, make(g));
      for (const id of [...cards.keys()]) if (!ids.includes(id)) cards.delete(id);
      const want = ids.map((id) => cards.get(id).el);
      if (want.length !== host.children.length || want.some((n, i) => host.children[i] !== n)) swap(host, ...want);
    },
    get: (id) => cards.get(id),
    size: () => cards.size,
  };
}

/**
 * @param scope  the tab's Scope;  @param shared the generic page's window state {win(), onWindow(fn)}
 * Returns {el (the whole section), refresh()}.
 */
export function dcgmHealth(scope, shared) {
  const win = () => shared.win();
  const noteHost = h('div');
  const busyHost = h('div', { class: 'grid two' });
  const powerHost = h('div', { class: 'grid two' });
  const violHost = h('div', { class: 'grid two' });
  const memHost = h('div', { class: 'stack-v' });
  const energyHost = h('div');
  const remapHost = h('div');
  const busyCards = perGpu(busyHost, (g) => busyCard(scope, g, win), t('gpu.busy.none'));
  const powerCards = perGpu(powerHost, (g) => powerCard(scope, g, win), t('gpu.power.none'));
  const violCards = perGpu(violHost, (g) => violCard(scope, g, win), t('gpu.viol.none_reported'));
  const mem = stackCard(scope, {
    title: t('gpu.mem.title'),
    hint: t('gpu.mem.hint'),
    parts: [
      { key: 'used', name: t('gpu.mem.used'), cls: 'bf1' },
      { key: 'reserved', name: t('gpu.mem.reserved'), cls: 'bf3' },
      { key: 'free', name: t('gpu.mem.free'), cls: 'bf2' },
    ],
    valueFmt: gib,
    axisFmt: (v) => `${Math.round(v)} GiB`,
    rowsTitle: t('gpu.mem.now'),
    timeTitle: t('gpu.mem.over_time'),
    wide: true,
  });
  memHost.append(mem.el);
  const tl = timelineCard(scope, { win });
  const sections = [
    section(t('gpu.busy.title'), t('gpu.busy.lead'), busyHost),
    memHost,
    section(t('gpu.power.title'), t('gpu.power.lead'), powerHost),
    section(t('gpu.energy.title'), t('gpu.energy.lead'), energyHost),
    section(t('gpu.viol.title'), t('gpu.viol.lead'), violHost),
    section(t('gpu.remap.title'), t('gpu.remap.lead'), remapHost),
    tl.el,
  ];
  const el = h(
    'div',
    { class: 'stack-v gpu-health' },
    h('div', { class: 'row gap wrap between' }, h('h2', t('gpu.health')), h('span', { class: 'muted small' }, t('gpu.health_hint'))),
    noteHost,
    ...sections,
  );
  let last = 0,
    busy = false,
    failures = new Map(),
    truncated = false;

  function note() {
    swap(
      noteHost,
      ...[...failures.values()].slice(0, 2).map((m) => h('p', { class: 'note warn', role: 'status' }, m)),
      truncated ? h('p', { class: 'note info', role: 'status' }, t('series.truncated')) : null,
    );
  }

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
    if (busy) return;
    const w = win();
    if (!force && Date.now() - last < (GAP_S[w] ?? 30) * 1000) return;
    busy = true;
    last = Date.now();
    try {
      const [busyA, memA, violA, snap, events] = await Promise.all([
        ask('busy', () => scope.api.series('dcgm', { keys: BUSY_KEYS, window: w, split: true })),
        ask('mem', () => scope.api.series('dcgm', { keys: MEM_KEYS, window: w, split: true })),
        ask('viol', () => scope.api.series('dcgm', { keys: VIOL_KEYS, window: w, split: true })),
        ask('metrics', () => scope.api.metrics('dcgm')),
        ask('events', () => scope.api.events('dcgm', 500)),
      ]);
      if (!scope.alive || w !== win()) return;
      if (busyA) {
        const groups = groupByGpu(busyA);
        busyCards.sync(groups);
        for (const g of groups) busyCards.get(g.gpu).update(g, busyA);
      }
      if (memA) {
        const groups = groupByGpu(memA);
        const meta = { from: memA.from, to: memA.to, stepS: memA.step_s };
        const models = groups.map((g) => memoryModel(g, meta)).filter((m) => m.any);
        mem.update({
          rows: models.map((m) => ({
            id: m.gpu,
            label: t('gpu.card_title', { gpu: m.gpu }),
            values: m.latest,
            totalText: m.total != null ? t('gpu.mem.total', { value: gib(m.total) }) : t('gpu.mem.partial'),
            note: null,
          })),
          time: models.map((m) => ({ label: gpuTitle(m), ts: m.ts, bucketS: m.bucketS, series: m.series })),
        });
        const pg = groups.filter((g) => powerModel(g, memA.step_s).has);
        powerCards.sync(pg);
        for (const g of pg) powerCards.get(g.gpu).update(g, memA);
        renderEnergy(energyHost, groups.map(energyModel), groups, win());
      }
      if (violA) {
        const groups = groupByGpu(violA).filter((g) => g.byKey.has('dcgm.violation_us_total'));
        violCards.sync(groups);
        for (const g of groups) violCards.get(g.gpu).update(g, violA);
      }
      if (snap?.available) renderRemap(remapHost, remapModel(snap.points));
      else if (snap) remapHost.replaceChildren(h('p', { class: 'muted' }, t('gpu.remap.unavailable')));
      if (events) {
        const to = busyA?.to ?? Date.now() / 1000;
        tl.update(events, { from: to - (WINDOW_SPAN_S[w] ?? 3600), to });
      }
      // more series than one request returns (32 per key): say so, the cards show the first GPUs
      truncated = [busyA, memA, violA].some((a) => a?.truncated);
      note();
    } finally {
      busy = false;
    }
  }
  shared.onWindow(() => load(true));
  load(true);
  scope.every(10000, () => load(false));
  return { el, refresh: () => load(true) };
}

const section = (title, lead, body) => h('section', { class: 'gpu-section stack-v' }, h('h3', { class: 'sub' }, title), h('p', { class: 'hint' }, lead), body);

// ---------------------------------------------------------------------------------------------- really busy?
function busyCard(scope, g, win) {
  const verdict = h('div', { class: 'row gap wrap busy-verdict' });
  const bars = h('div', { class: 'busy-bars' });
  const tiles = h('div', { class: 'stats' });
  const chart = linesCard(scope, { title: t('gpu.busy.chart'), panels: [{ id: 'busy', yFmt: (v) => `${Math.round(v * 100)}%`, valueFmt: pct }], win });
  const el = h(
    'section',
    { class: 'card gpu-card', 'aria-label': gpuTitle(g) },
    h('header', { class: 'card-head' }, h('h3', gpuTitle(g))),
    h('div', { class: 'card-body stack-v' }, verdict, bars, tiles, chart.el),
  );
  return {
    el,
    update(grp, answer) {
      const m = busyModel(grp);
      swap(verdict, badge(t(`gpu.busy.verdict_${m.verdict}`), VERDICT_KIND[m.verdict]), h('span', { class: 'muted small' }, t(`gpu.busy.say_${m.verdict}`)));
      swap(
        bars,
        ratioBar(t('gpu.busy.util'), m.util),
        ratioBar(t('gpu.busy.gr'), m.gr),
        ratioBar(t('gpu.busy.tensor'), m.tensor),
        ratioBar(t('gpu.busy.dram'), m.dram),
        m.encoder != null ? ratioBar(t('gpu.busy.enc'), m.encoder) : null,
        m.decoder != null ? ratioBar(t('gpu.busy.dec'), m.decoder) : null,
      );
      tiles.replaceChildren(stat(t('gpu.busy.sm_clock'), m.smClock == null ? '–' : fmtUnit(m.smClock, 'MHz'), t('gpu.busy.latest')));
      const line = (name, key, idx) => {
        const s = grp.byKey.get(key)?.[0];
        return s ? { name, idx, segments: valueSegments(s.points, answer.step_s) } : null;
      };
      chart.update({
        from: answer.from,
        to: answer.to,
        panels: [
          {
            lines: [
              line(t('gpu.busy.util'), 'dcgm.gpu_util', 0),
              line(t('gpu.busy.gr'), 'dcgm.gr_active', 1),
              line(t('gpu.busy.tensor'), 'dcgm.tensor_active', 2),
              line(t('gpu.busy.dram'), 'dcgm.dram_active', 3),
            ].filter(Boolean),
          },
        ],
      });
    },
  };
}

// ---------------------------------------------------------------------------------------------- power against its limit
function powerCard(scope, g, win) {
  const hintEl = h('p', { class: 'hint' });
  const chart = linesCard(scope, { title: gpuTitle(g), panels: [{ id: 'power', yFmt: (v) => `${Math.round(v)}`, valueFmt: watts }], win });
  chart.el.querySelector('.card-body').prepend(hintEl);
  return {
    el: chart.el,
    update(grp, answer) {
      const m = powerModel(grp, answer.step_s);
      const lines = [m.power.length ? { name: t('gpu.power.power'), idx: 0, segments: m.power } : null, m.limit.length ? { name: t('gpu.power.limit'), idx: 1, segments: m.limit } : null].filter(
        Boolean,
      );
      hintEl.textContent =
        m.limitW == null
          ? t('gpu.power.no_limit')
          : m.nearLimit
            ? t('gpu.power.near', { peak: watts(m.peak), limit: watts(m.limitW) })
            : t('gpu.power.headroom', { peak: watts(m.peak), limit: watts(m.limitW) });
      chart.update({ from: answer.from, to: answer.to, panels: [{ lines }] });
    },
  };
}

// ---------------------------------------------------------------------------------------------- energy over the window
function renderEnergy(host, models, groups, w) {
  const have = models.map((m, i) => (m ? { m, g: groups[i] } : null)).filter(Boolean);
  if (!have.length) {
    swap(host, h('p', { class: 'muted' }, t('gpu.energy.none')));
    return;
  }
  const kwh = (m) => (m.kwh >= 1 ? `${m.kwh.toFixed(2)} kWh` : `${m.wh.toFixed(m.wh >= 100 ? 0 : 1)} Wh`);
  const total = energyTotal(have.map((x) => x.m));
  const lower = have.some((x) => x.m.resets > 0);
  swap(
    host,
    h(
      'div',
      { class: 'stats' },
      ...have.map(({ m, g }) => stat(t('gpu.card_title', { gpu: g.gpu }), kwh(m), m.avgW == null ? null : t('gpu.energy.avg', { watts: watts(m.avgW) }))),
      have.length > 1 ? stat(t('gpu.energy.total'), kwh(total), total.avgW == null ? null : t('gpu.energy.avg', { watts: watts(total.avgW) })) : null,
    ),
    h('p', { class: 'muted small' }, t('gpu.energy.over', { window: windowPhrase(w) }), lower ? ` ${t('gpu.energy.reset_note')}` : ''),
  );
}

// ---------------------------------------------------------------------------------------------- throttle violations
function violCard(scope, g, win) {
  const verdict = h('div', { class: 'row gap wrap' });
  const table = h('div', { class: 'tablewrap' });
  const chart = linesCard(scope, { title: t('gpu.viol.chart'), panels: [{ id: 'viol', yFmt: (v) => `${+v.toFixed(1)}%`, valueFmt: (v) => (v == null ? '–' : `${+v.toFixed(2)}%`) }], win });
  const el = h(
    'section',
    { class: 'card gpu-card', 'aria-label': gpuTitle(g) },
    h('header', { class: 'card-head' }, h('h3', gpuTitle(g))),
    h('div', { class: 'card-body stack-v' }, verdict, chart.el, table),
  );
  return {
    el,
    update(grp, answer) {
      const m = violationModel(grp, answer.step_s);
      swap(verdict, m.quiet ? badge(t('gpu.viol.quiet'), 'ok') : badge(t('gpu.viol.throttled'), 'warn'), h('span', { class: 'muted small' }, m.quiet ? t('gpu.viol.quiet_say') : t('gpu.viol.say')));
      // only reasons that throttled are drawn (a flat zero line per reason is noise); every reason is in the table
      const lines = m.rows
        .filter((r) => r.pct > 0)
        .map((r) => ({ name: reasonName(r.reason), idx: VIOLATION_REASONS.indexOf(r.reason) < 0 ? 0 : VIOLATION_REASONS.indexOf(r.reason), segments: r.segments }));
      chart.update({ from: answer.from, to: answer.to, panels: [{ lines }], note: m.quiet ? null : m.rows.some((r) => r.resets > 0) ? t('gpu.viol.reset_note') : null });
      table.replaceChildren(
        h(
          'table',
          { class: 'compact', 'aria-label': t('gpu.viol.table', { gpu: g.gpu }) },
          h(
            'thead',
            h(
              'tr',
              h('th', { scope: 'col' }, t('gpu.viol.reason')),
              h('th', { scope: 'col', class: 'al-right' }, t('gpu.viol.pct')),
              h('th', { scope: 'col', class: 'al-right' }, t('gpu.viol.seconds')),
            ),
          ),
          h(
            'tbody',
            m.rows.map((r) =>
              h(
                'tr',
                h('td', reasonName(r.reason)),
                h('td', { class: 'num al-right' }, r.pct == null ? '–' : `${+r.pct.toFixed(2)}%`),
                h('td', { class: 'num al-right' }, r.seconds == null ? '–' : r.seconds < 120 ? `${+r.seconds.toFixed(1)} s` : fmtDuration(r.seconds)),
              ),
            ),
          ),
        ),
      );
    },
  };
}
function reasonName(reason) {
  const k = `gpu.viol.r_${reason}`;
  const s = t(k);
  return s === k ? reason : s;
}

// ---------------------------------------------------------------------------------------------- remapped rows
function renderRemap(host, rows) {
  if (!rows.length) {
    swap(host, h('p', { class: 'muted' }, t('gpu.remap.none')));
    return;
  }
  const n = (v) => (v == null ? '–' : fmtInt(v));
  swap(
    host,
    h(
      'div',
      { class: 'tablewrap' },
      h(
        'table',
        { class: 'compact', 'aria-label': t('gpu.remap.title') },
        h(
          'thead',
          h(
            'tr',
            h('th', { scope: 'col' }, t('gpu.f_gpu')),
            h('th', { scope: 'col', class: 'al-right' }, t('gpu.remap.correctable')),
            h('th', { scope: 'col', class: 'al-right' }, t('gpu.remap.uncorrectable')),
            h('th', { scope: 'col' }, t('gpu.remap.failure')),
          ),
        ),
        h(
          'tbody',
          rows.map((r) =>
            h(
              'tr',
              h('td', gpuTitle(r)),
              h('td', { class: 'num al-right' }, n(r.correctable)),
              h('td', { class: 'num al-right' }, n(r.uncorrectable)),
              h('td', r.failure == null ? '–' : r.failure ? badge(t('gpu.remap.failed'), 'bad') : badge(t('gpu.remap.ok'), 'ok')),
            ),
          ),
        ),
      ),
    ),
  );
}
