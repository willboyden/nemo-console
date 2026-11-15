// The "Gateway resources" card of the Capture tab (docs/ARCHITECTURE.md addendum 9.17), for every role: what the Relay gateway
// PROCESS itself uses (CPU, memory, threads and file descriptors, disk, the ATOF filesystem) and what its Switchyard plugin does
// while routing (requests, failures, overhead, tokens). The figures are the console's own Relay metric keys: the latest values
// come from the metrics snapshot, the charts from the series route (at most 12 keys a call, so the cards are packed over two
// calls). Polled every 30 s while the browser tab is visible; a window of 1h, 24h or 7d (kept in the URL as `res_window`).
// When the capture answer says resource metrics are off, the card says so and shows the command that switches them on, and asks
// for nothing. Everything is text (h()); every chart has a text alternative (the lines card's data table) and an aria-label.
import { h } from '../dom.js';
import { t } from '../i18n.js';
import { fmtDuration } from '../format.js';
import { badge, btn, card, emptyBox, errorBox, fixBlock, select, skeleton, stat } from '../components/ui.js';
import { normalizeAnswer, valueSegments } from '../series-model.js';
import { linesCard } from '../charts/lines-card.js';
import { liveLoad } from './live.js';
import { snippetBlock, swap } from './ui.js';
import {
  RES_CARDS,
  RES_TILES,
  RES_WINDOWS,
  RES_DEFAULT_WINDOW,
  planCalls,
  resWindow,
  fmtResource,
  axisFormatter,
  axisStep,
  freshness,
  sampleAge,
  latestValues,
  hasResourceFigures,
  tileModel,
} from '../relay-runtime-model.js';

/**
 * What switches the gateway's resource sampler on: a component in Relay's plugins.toml (the generic remedy, copyable), and, as a second
 * line for this lab only, the lab's own helper command. The console never changes either; it shows them.
 */
export const RESOURCES_PLUGIN_SNIPPET = `[[components]]
kind = "resource_metrics"
enabled = true

[components.config]
measurement_scope = "application_process"

[components.config.polling]
enabled = true
interval_millis = 60000
`;
export const RESOURCES_ON_COMMAND = 'python3 -I ops/nemo-relay-levels.py resources on';
const REFRESH_MS = 30000;
const FRESH_KIND = { fresh: 'ok', stale: 'warn', unknown: 'muted' };
const FRESH_GLYPH = { fresh: '●', stale: '◔', unknown: '?' };
const baseKey = (k) => String(k ?? '').split('{')[0];
const fmtStep = (s) => (s < 60 ? `${+s.toFixed(1)} s` : `${+(s / 60).toFixed(1)} min`);
const lineName = (key) => t(`rt.res.l_${baseKey(key).replace(/^relay\./, '')}`);

/** The freshness chip: a glyph, a word and the age (never colour alone). */
export function freshChip(fr, intervalS) {
  // an age without a verdict (the sampling interval is not known) is a "last sample", not "no sample yet"
  const word = t(fr.state === 'unknown' && fr.ageS != null ? 'rt.res.fresh_unverified' : `rt.res.fresh_${fr.state}`);
  const text = fr.ageS == null ? word : `${word} · ${t('rt.res.age', { age: fmtDuration(fr.ageS) })}`;
  const tip =
    fr.state === 'unknown' && fr.ageS == null
      ? t('rt.res.fresh_unknown_tip')
      : fr.limitS == null
        ? t('rt.res.fresh_no_interval_tip')
        : t(`rt.res.fresh_${fr.state}_tip`, { limit: fmtDuration(fr.limitS), every: fmtDuration(intervalS) });
  return h('span', { class: `badge ${FRESH_KIND[fr.state]} res-fresh st-${fr.state}`, title: tip }, h('span', { 'aria-hidden': 'true' }, FRESH_GLYPH[fr.state]), text);
}

/**
 * @param opts.nav  the Capture tab's createNav() (the window lives in its query as `res_window`)
 * Returns {el, setCapture(resources, lastTs)}: `resources` is the capture answer's `resources` object (`enabled: false` shows the
 * empty state), null when it is not known (an older backend, or the capture request failed: the card then asks anyway); `lastTs` is the
 * `resource_metrics` row's `observed.last_ts` (epoch seconds of the newest sample mark, null when none was seen), which the freshness
 * chip is drawn from. Nothing is asked until the first call.
 */
export function resourcesCard(scope, { nav }) {
  let win = resWindow(nav.get('res_window', RES_DEFAULT_WINDOW));
  let cfg; // undefined = the capture answer is not in yet
  let cfgRaw = null;
  let lastTs = null; // the capture answer's newest sample mark (resource_metrics observed.last_ts)
  let last = null; // the last good {snap, answers}
  let failed = false; // a refresh failed since the last good answer (its note must go when one arrives)
  const plan = planCalls();

  // ---- the charts: one lines card per RES_CARDS entry, kept across refreshes so an open data table survives
  const charts = new Map(
    RES_CARDS.map((c) => [
      c.id,
      linesCard(scope, {
        title: t(`rt.res.card_${c.id}`),
        wide: !!c.wide,
        win: () => win,
        panels: c.panels.map((p) => ({
          id: `${c.id}.${p.id}`,
          title: c.panels.length > 1 ? t(`rt.res.panel_${p.id}`) : undefined,
          yFmt: axisFormatter(p.unit, 1), // replaced on every update by one that fits the data's own tick step
          valueFmt: (v) => fmtResource(p.unit, v),
          height: 150,
        })),
      }),
    ]),
  );

  // ---- the frame
  const chipHost = h('span', { class: 'res-chip' });
  const winSel = select(
    RES_WINDOWS.map((w) => ({ value: w, label: t(`window.${w}`) })),
    win,
    (v) => {
      win = resWindow(v);
      nav.set({ res_window: win === RES_DEFAULT_WINDOW ? null : win });
      live.forget();
      live.refresh();
    },
    { 'aria-label': t('rt.res.window') },
  );
  const refreshBtn = btn(t('common.refresh'), { size: 'sm', icon: 'restart', onClick: () => live.refresh() });
  const noteHost = h('div');
  const factsHost = h('div', { class: 'row gap wrap small res-facts' });
  const stripHost = h('div', { class: 'stats res-strip' });
  const emptyHost = h('div');
  const grid = h(
    'div',
    { class: 'grid two res-grid' },
    RES_CARDS.map((c) => charts.get(c.id).el),
  );
  const liveEl = h('div', { class: 'stack-v' }, h('p', { class: 'hint' }, t('rt.res.explain')), noteHost, factsHost, stripHost, emptyHost, grid);
  const offHost = h('div');
  const body = h('div', { class: 'stack-v', 'aria-busy': 'true' }, skeleton(3));
  const el = card(t('rt.res.title'), body, [chipHost, winSel, refreshBtn], { id: 'rt-resources' });
  el.classList.add('rt-res');
  const controls = [winSel, refreshBtn];
  const setControls = (on) => {
    for (const c of controls) c.hidden = !on;
  };
  setControls(false);

  // ---- states
  function showOff() {
    setControls(false);
    swap(chipHost);
    swap(
      body,
      swap(
        offHost,
        emptyBox(
          t('rt.res.off_title'),
          cfg?.configured ? t('rt.res.off_configured') : t('rt.res.off_hint'),
          h('div', { class: 'stack-v res-remedy' }, snippetBlock(RESOURCES_PLUGIN_SNIPPET, t('rt.res.off_snippet')), fixBlock(RESOURCES_ON_COMMAND, t('rt.res.off_lab'))),
        ),
      ),
    );
    body.removeAttribute('aria-busy');
  }
  /** The freshness chip from the newest sample mark's age: fresh up to 3 sampling intervals, "no sample yet" when none was seen. */
  function drawChip() {
    if (cfg === undefined || cfg?.enabled === false) return;
    const interval = cfg?.interval_s ?? null;
    swap(chipHost, freshChip(freshness(sampleAge(lastTs, Date.now() / 1000), interval), interval));
  }
  function showLive() {
    setControls(true);
    if (body.firstChild !== liveEl) swap(body, liveEl);
  }

  function render({ snap, answers }) {
    const latest = latestValues(snap);
    const interval = cfg?.interval_s ?? null;
    drawChip();
    swap(noteHost, snap && snap.available === false && snap.error ? h('p', { class: 'note warn', role: 'status' }, snap.error) : null);
    swap(
      factsHost,
      cfg?.scope ? h('span', { class: 'chip' }, t('rt.res.scope', { scope: cfg.scope })) : null,
      typeof interval === 'number' ? h('span', { class: 'chip' }, t('rt.res.interval', { every: fmtDuration(interval) })) : null,
      cfg && cfg.gpu != null ? h('span', { class: 'chip' }, t(cfg.gpu ? 'rt.res.gpu_on' : 'rt.res.gpu_off')) : null,
      cfg && cfg.network != null ? h('span', { class: 'chip' }, t(cfg.network ? 'rt.res.net_on' : 'rt.res.net_off')) : null,
    );
    swap(
      stripHost,
      RES_TILES.map((tile) => {
        const m = tileModel(tile, latest);
        const label = t(`rt.res.t_${tile.id}`);
        const sub = m.sub == null ? (m.hot ? t('rt.res.hot') : null) : t(`rt.res.s_${tile.id}`, { v: m.sub });
        const s = stat(label, m.value, sub);
        if (m.hot) s.classList.add('hot');
        return s;
      }),
    );
    let any = hasResourceFigures(latest);
    plan.forEach((call, i) => {
      const a = answers[i];
      const series = normalizeAnswer(a);
      for (const id of call.cards) {
        const def = RES_CARDS.find((c) => c.id === id);
        const panels = def.panels.map((p) => {
          const lines = p.lines
            .map((key, k) => {
              const s = series.find((x) => baseKey(x.key) === key);
              const segments = s ? valueSegments(s.points, a?.step_s) : [];
              return segments.some((seg) => seg.length) ? { name: lineName(key), idx: k, segments } : null;
            })
            .filter(Boolean);
          // the axis labels follow the step the chart will draw its ticks with, so a tiny CPU figure reads 0, 0.2 m, 0.4 m and not 0, 0, 0
          return { lines, yFmt: axisFormatter(p.unit, axisStep(lines)) };
        });
        const has = panels.some((p) => p.lines.length);
        any ||= has;
        charts.get(id).update({
          from: a?.from,
          to: a?.to,
          panels,
          caption: typeof a?.step_s === 'number' ? t('series.step', { step: fmtStep(a.step_s) }) : '',
          note: id === 'routing' && !has ? t('rt.res.routing_none') : null,
        });
      }
    });
    swap(emptyHost, any ? null : emptyBox(t('rt.res.none_title'), t('rt.res.none_hint', { every: typeof interval === 'number' ? fmtDuration(interval) : t('rt.res.a_minute') })));
    grid.hidden = !any;
    stripHost.hidden = !any;
    body.removeAttribute('aria-busy');
  }

  // ---- polling
  const live = liveLoad(scope, {
    load: async (signal) => {
      const [snap, ...answers] = await Promise.all([
        scope.api.metrics('relay', { signal }).catch((e) => {
          if (e?.name === 'AbortError') throw e;
          return null;
        }),
        ...plan.map((call) => scope.api.series('relay', { keys: call.keys, window: win, split: false }, { signal })),
      ]);
      return { snap, answers };
    },
    ms: REFRESH_MS,
    enabled: () => cfg !== undefined && cfg?.enabled !== false,
    onData: (data, { changed }) => {
      last = data;
      showLive();
      if (changed || failed || body.getAttribute('aria-busy') === 'true') render(data);
      else drawChip(); // the chip ages with the clock even when the figures are the same
      failed = false;
    },
    onError: (e, { quiet }) => {
      // a failed refresh keeps what is on screen; the first load, or a button press, says what went wrong
      if (last && quiet) return;
      failed = true;
      showLive();
      swap(last ? noteHost : body, last ? h('p', { class: 'note warn', role: 'status' }, t('rt.res.refresh_failed')) : errorBox(e, () => live.refresh()));
      body.removeAttribute('aria-busy');
    },
  });

  return {
    el,
    /**
     * The capture answer's `resources` (or null when unknown) and the newest sample mark's time. A change of `resources` re-draws; the
     * same answer again does nothing, and a new `lastTs` alone only redraws the chip.
     */
    setCapture(next, ts) {
      lastTs = typeof ts === 'number' && Number.isFinite(ts) ? ts : null;
      const raw = JSON.stringify(next ?? null);
      if (cfg !== undefined && raw === cfgRaw) {
        drawChip();
        return;
      }
      const wasOff = cfg?.enabled === false;
      cfg = next ?? null;
      cfgRaw = raw;
      if (cfg?.enabled === false) {
        showOff();
        return;
      }
      showLive();
      drawChip();
      if (last && !wasOff)
        render(last); // only the interval or the facts changed
      else {
        live.forget();
        live.refresh();
      }
    },
  };
}
