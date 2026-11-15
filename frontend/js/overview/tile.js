// One system tile of the Overview (addendum 9.16): health, version, summary, components and problems as before, plus a 1 h sparkline of the system's key metric and, where the system
// has a `*.up`-style metric, an up / down strip. The tile is created once and UPDATED in place every 5 s (the overview poll), so the sparkline is not thrown away with the card; the
// sparkline's data is fetched lazily (when the tile scrolls into view) and again at most once a minute: ONE batched series request per system. A tile with no samples says so in words
// (no flat line at 0). Everything from the server is text.
import { h } from '../dom.js';
import { t } from '../i18n.js';
import { describeError } from '../errors.js';
import { healthBadge, compBadge, problemList } from '../components/ui.js';
import { sparkline } from '../charts/sparkline.js';
import { stripChart } from '../charts/strip-chart.js';
import { stripSummary } from '../charts/strip-parts.js';
import { SPARKS, SPARK_STALE_MS, SPARK_WINDOW, sparkRequest, sparkModel, tileStrip, fmtSparkValue } from '../overview-model.js';

/**
 * @param scope   the tab's scope (the API client, `own` for cleanup)
 * Returns {system, el, update(s), ensure(force), visible(bool)}.
 */
export function createTile(scope, system) {
  const head = h('header', { class: 'card-head' });
  const meta = h('div', { class: 'muted small' });
  const summary = h('p', { class: 'status-summary' });
  const comps = h('div', { class: 'row gap wrap' });
  const problems = h('div');
  const sparkLabel = h('span', { class: 'spark-label muted small' });
  const sparkLatest = h('strong', { class: 'spark-latest num' });
  const sparkHost = h('div', { class: 'spark-host' });
  const stripHost = h('div', { class: 'spark-strip' });
  const stripText = h('p', { class: 'spark-strip-text muted small' });
  const spec = SPARKS[system];
  const spark = spec
    ? h(
        'div',
        { class: 'tile-spark', 'aria-label': t('ovw.spark_region', { name: t(`sys.${system}`) }) },
        h('div', { class: 'row gap between spark-row' }, sparkLabel, sparkLatest),
        sparkHost,
        spec.up ? h('div', { class: 'spark-up' }, stripHost, stripText) : null,
      )
    : null;
  const el = h(
    'section',
    { class: 'card tile', 'aria-label': t(`sys.${system}`) },
    head,
    h('div', { class: 'card-body' }, meta, summary, comps, spark, problems, h('div', { class: 'row end' }, h('a', { class: 'btn', href: `#/${system}` }, t('overview.open')))),
  );

  // ---- the sparkline's own state
  let model = null,
    stripModel = null,
    state = 'idle', // idle | loading | ok | none | fail
    error = '',
    at = 0,
    seq = 0;
  const label = () => (spec ? t(`ovw.spark.${system}`) : '');

  function drawSpark() {
    if (!spec) return;
    sparkLabel.textContent = t('ovw.spark_label', { name: label(), window: t('ovw.spark_window') });
    if (state === 'idle' || state === 'loading') {
      sparkLatest.textContent = '';
      sparkHost.replaceChildren(h('p', { class: 'muted small spark-note', role: 'status' }, t('common.loading')));
    } else if (state === 'fail') {
      sparkLatest.textContent = '';
      sparkHost.replaceChildren(h('p', { class: 'muted small spark-note', role: 'status' }, t('ovw.spark_failed', { reason: error })));
    } else if (!model?.any) {
      sparkLatest.textContent = '–';
      sparkHost.replaceChildren(h('p', { class: 'muted small spark-note', role: 'status' }, t('ovw.spark_none', { window: t('ovw.spark_window') })));
    } else {
      const latest = fmtSparkValue(model.latest, spec.unit, model.per);
      sparkLatest.textContent = latest;
      const words = t('ovw.spark_alt', {
        name: label(),
        latest,
        min: fmtSparkValue(model.min, spec.unit, model.per),
        max: fmtSparkValue(model.max, spec.unit, model.per),
        window: t('ovw.spark_window'),
      });
      const w = Math.max(120, Math.round(sparkHost.clientWidth || 220));
      sparkHost.replaceChildren(sparkline({ values: model.values, width: w, height: 36, ariaLabel: words }));
    }
    if (spec.up) {
      if (stripModel && !stripModel.empty) {
        const sum = stripSummary(stripModel, t('ovw.spark_window'), t(`sys.${system}`));
        stripText.textContent = sum;
        const w = Math.max(120, Math.round(stripHost.clientWidth || 220));
        stripHost.replaceChildren(stripChart({ cells: stripModel.cells, width: w, height: 18, axis: false, ariaLabel: t('ovw.strip_alt', { summary: sum }) }));
      } else {
        stripText.textContent = state === 'ok' || state === 'none' ? t('ovw.strip_none', { window: t('ovw.spark_window') }) : '';
        stripHost.replaceChildren();
      }
    }
  }

  /** Fetch the sparkline (and strip) once when the tile first needs it and again after SPARK_STALE_MS. One request. */
  async function ensure(force = false) {
    if (!spec) return;
    if (!force && (state === 'loading' || (at && Date.now() - at < SPARK_STALE_MS))) return;
    const my = ++seq;
    const first = state === 'idle';
    state = first ? 'loading' : state;
    if (first) drawSpark();
    try {
      const a = await scope.api.series(system, sparkRequest(system));
      if (!scope.alive || my !== seq) return;
      model = sparkModel(a, system);
      stripModel = tileStrip(a, system);
      state = model.any ? 'ok' : 'none';
      at = Date.now();
    } catch (e) {
      if (!scope.alive || my !== seq) return;
      at = Date.now(); // a failing system is not asked again for a minute either
      if (state !== 'ok') {
        state = 'fail';
        error = describeError(e);
      }
    }
    drawSpark();
  }

  let lastW = 0;
  if (spec && typeof ResizeObserver !== 'undefined') {
    const ro = new ResizeObserver(() => {
      const w = Math.round(sparkHost.clientWidth);
      if (w < 80 || Math.abs(w - lastW) < 6 || state === 'idle' || state === 'loading') return;
      lastW = w;
      drawSpark();
    });
    ro.observe(sparkHost);
    scope.own(() => ro.disconnect());
  }

  /** The tile's plain content from an overview system; the sparkline is left alone. */
  function update(s) {
    el.className = `card tile ${s.health}`;
    head.replaceChildren(h('h3', h('a', { href: `#/${s.system}` }, t(`sys.${s.system}`))), healthBadge(s.health));
    meta.textContent = s.version ? `v${s.version}` : t('overview.no_version');
    summary.textContent = s.summary ?? '';
    comps.replaceChildren(...s.components.map((c) => h('span', { class: 'comp-chip' }, h('code', c.name), ' ', compBadge(c.state))));
    problems.replaceChildren(
      ...(s.problems.length ? [h('h4', { class: 'sub' }, t('overview.problems', { count: s.problems.length })), problemList(s.problems)] : [h('p', { class: 'note ok' }, t('overview.no_problems'))]),
    );
  }

  drawSpark();
  return { system, el, update, ensure, window: SPARK_WINDOW };
}
