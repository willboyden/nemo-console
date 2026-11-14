// The Fleet tab of the GPU telemetry page (addendum 9.14): a heatmap of every GPU of every DCGM host Prometheus scrapes, rows = host / GPU,
// columns = time, colour = the chosen metric (utilisation, temperature, power, memory used), from `GET /systems/dcgm/fleet`. A metric and
// a window picker (both in the URL hash), a legend with the scale, hover (or Left / Right on a focused row) = value and time, a host strip
// (up / down), and a table of every row as the text alternative. A host that is down is a row of gaps; Prometheus unreachable, an empty
// answer and a backend that predates the route each have their own message.
import { h } from '../dom.js';
import { t } from '../i18n.js';
import { badge, btn, emptyBox, errorBox, skeleton } from '../components/ui.js';
import { createHashState } from '../charts/hash-state.js';
import { segPicker } from '../charts/controls.js';
import { matrixHeatmap } from '../charts/heatmap.js';
import { dataTable, swap, windowPhrase } from '../charts/parts.js';
import { shapeFleet, hostsFromSnapshot, fleetDomain, fleetFormat, fleetMetric, fleetWindow, FLEET_METRICS, FLEET_WINDOWS, DEFAULT_FLEET } from '../dcgm-model.js';

const REFRESH_MS = 60000;

export function fleetTab(scope, ctx) {
  const hash = createHashState({ path: 'dcgm', tab: 'fleet', initial: ctx?.initial !== false });
  let metric = fleetMetric(hash.get('metric')),
    win = fleetWindow(hash.get('window'));
  let model = null,
    lastSig = '',
    seq = 0;
  const hostsHost = h('div', { class: 'row gap wrap fleet-hosts' });
  const noteHost = h('div', { class: 'stack-v' });
  const tableHost = h('div');
  const stateHost = h('div');
  const heat = matrixHeatmap(scope, {
    build: () => (model && !model.empty ? { rows: model.rows.map((r) => ({ ...r, label: rowLabel(r), group: r.host })), ts: model.ts, bucketS: model.binS } : null),
    domain: () => fleetDomain(model.metric || metric, model.unit, model.rows),
    fmt: () => fleetFormat(model?.metric || metric, model?.unit),
    ariaLabel: () => t('gpu.fleet.alt', { metric: t(`gpu.fleet.m_${metric}`).toLowerCase(), window: windowPhrase(win), rows: model.rows.length, hosts: new Set(model.rows.map((r) => r.host)).size }),
  });
  heat.el.hidden = true;

  const metricPicker = segPicker({
    values: FLEET_METRICS,
    current: () => metric,
    labelOf: (m) => t(`gpu.fleet.m_${m}`),
    label: t('gpu.fleet.metric'),
    onPick: (m) => {
      metric = m;
      hash.set({ metric: m === DEFAULT_FLEET.metric ? null : m });
      metricPicker.redraw();
      load(true);
    },
  });
  const windowPicker = segPicker({
    values: FLEET_WINDOWS,
    current: () => win,
    labelOf: (w) => w,
    titleOf: (w) => t(`window.${w}`),
    label: t('routing.window'),
    onPick: (w) => {
      win = w;
      hash.set({ window: w === DEFAULT_FLEET.window ? null : w });
      windowPicker.redraw();
      load(true);
    },
  });

  const rowLabel = (r) => (r.gpu == null ? r.host : t('gpu.fleet.row', { host: r.host, gpu: r.gpu }));

  function renderHosts(rows) {
    const hosts = [...new Map(rows.map((r) => [r.host, r])).values()];
    swap(
      hostsHost,
      hosts.length ? h('span', { class: 'muted small' }, t('gpu.fleet.hosts')) : null,
      ...hosts.map((r) => badge(r.down ? t('gpu.fleet.host_down', { host: r.host }) : t('gpu.fleet.host_up', { host: r.host }), r.down ? 'bad' : 'ok')),
    );
  }

  function renderTable() {
    const f = fleetFormat(model.metric || metric, model.unit);
    const fv = (v) => (v == null ? null : f(v));
    tableHost.replaceChildren(
      dataTable(t('gpu.fleet.table'), [t('gpu.fleet.col_gpu'), t('gpu.fleet.col_model'), t('series.latest'), t('series.avg'), t('series.max')], () =>
        model.rows.map((r) => [rowLabel(r) + (r.down ? ` (${t('heat.host_down')})` : ''), r.model || null, fv(r.stats.latest), fv(r.stats.avg), fv(r.stats.max)]),
      ),
    );
  }

  async function load(force = false) {
    const my = ++seq;
    if (!model) swap(stateHost, skeleton(3));
    let answer, snap;
    try {
      [answer, snap] = await Promise.all([scope.api.fleet({ metric, window: win }), scope.api.metrics('dcgm').catch(() => null)]);
    } catch (e) {
      if (!scope.alive || my !== seq) return;
      model = null;
      lastSig = '';
      heat.redraw(); // build() gives nothing now: the old drawing goes away
      heat.el.hidden = true;
      tableHost.replaceChildren();
      swap(hostsHost);
      swap(noteHost);
      swap(stateHost, e?.status === 404 ? emptyBox(t('gpu.fleet.unsupported'), t('gpu.fleet.unsupported_hint')) : errorBox(e, () => load(true)));
      return;
    }
    if (!scope.alive || my !== seq) return;
    const hostsUp = snap?.available ? hostsFromSnapshot(snap.points) : [];
    const maxCols = Math.max(48, Math.min(200, Math.floor((heat.el.clientWidth || 700) / 4.5)));
    const next = shapeFleet(answer, { maxCols, hostsUp });
    const sig = JSON.stringify([next.metric, next.window, next.from, next.to, next.rows.map((r) => [r.id, r.down, r.values]), next.problem, next.truncated]);
    if (!force && sig === lastSig) return;
    lastSig = sig;
    model = next;
    swap(stateHost);
    swap(
      noteHost,
      model.problem
        ? h(
            'p',
            { class: 'note warn', role: 'status' },
            h('strong', t('gpu.fleet.problem')),
            ' ',
            model.problem.message || model.problem.code,
            model.problem.code ? h('code', { class: 'muted small' }, ` ${model.problem.code}`) : null,
          )
        : null,
      model.truncated ? h('p', { class: 'note info', role: 'status' }, t('gpu.fleet.truncated', { max: 64 })) : null,
      !model.empty && model.binS > model.stepS ? h('p', { class: 'muted small' }, t('gpu.fleet.merged', { step: stepText(model.stepS), bin: stepText(model.binS) })) : null,
    );
    renderHosts(model.rows);
    if (model.empty) {
      heat.redraw(); // build() gives nothing now: the old drawing goes away
      heat.el.hidden = true;
      tableHost.replaceChildren();
      // Prometheus unreachable: the note above says why (and no hosts are known); otherwise the window simply has no samples
      if (!model.problem) swap(stateHost, emptyBox(t('gpu.fleet.empty'), t('gpu.fleet.empty_hint', { window: windowPhrase(win) })));
      return;
    }
    heat.el.hidden = false;
    heat.redraw();
    renderTable();
  }

  load(true);
  scope.every(REFRESH_MS, () => load(false));
  return h(
    'div',
    { class: 'stack-v' },
    h('p', { class: 'hint' }, t('gpu.fleet.lead')),
    h(
      'section',
      { class: 'card chart-card fleet-card', 'aria-label': t('gpu.fleet.title') },
      h(
        'header',
        { class: 'card-head' },
        h('h2', t('gpu.fleet.title')),
        h('div', { class: 'row gap wrap' }, metricPicker, windowPicker, btn(t('common.refresh'), { size: 'sm', icon: 'restart', onClick: () => load(true) })),
      ),
      h('div', { class: 'card-body stack-v' }, hostsHost, noteHost, stateHost, heat.el, tableHost),
    ),
  );
}

const stepText = (s) => (s < 60 ? `${s} s` : s < 3600 ? `${+(s / 60).toFixed(1)} min` : `${+(s / 3600).toFixed(1)} h`);
