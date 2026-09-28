import { NcView } from '../components/base.js';
import { h, clear } from '../dom.js';
import { t } from '../i18n.js';
import { fmtInt, fmtMs, fmtRatio, fmtTick, debounce } from '../format.js';
import { valueOf, keyWith } from '../metrics.js';
import { isLoading, parsePreflight } from '../status-util.js';
import { pruneValues, changedKeys, needsRestart, isRedacted } from '../param-form.js';
import { badge, btn, adminBtn, card, stat, select, errorBox, emptyBox, skeleton, problemList } from '../components/ui.js';
import { lineChartCard } from '../components/chart-card.js';
import { systemPage } from '../components/system-page.js';
import { statusDetails } from '../components/status-panel.js';
import { playgroundPanel } from '../components/playground.js';
import { actionsPanel, actionResultView } from '../components/actions-panel.js';
import { renderParamForm } from '../components/param-field.js';
import { confirmEffect } from '../components/dialog.js';
import { isAdmin } from '../app-context.js';
import { toastError } from '../components/toast.js';
import { announce } from '../components/live.js';
import { outcomeBanner } from '../components/config-editor.js';
import { describeError } from '../errors.js';

const WINDOWS = ['15m', '1h', '24h'];
const SERIES = [
  { id: 'ttft', title: 'dyn.ttft', keys: [['p50', keyWith('dyn.ttft_ms', { q: 'p50' })], ['p95', keyWith('dyn.ttft_ms', { q: 'p95' })]], fmt: fmtMs },
  { id: 'itl', title: 'dyn.itl', keys: [['p50', keyWith('dyn.itl_ms', { q: 'p50' })], ['p95', keyWith('dyn.itl_ms', { q: 'p95' })]], fmt: fmtMs },
  { id: 'inflight', title: 'dyn.inflight', keys: [['in-flight', 'dyn.in_flight']], fmt: fmtTick },
  { id: 'kv', title: 'dyn.kv', keys: [['kv-cache', 'dyn.kv_cache_usage']], fmt: (v) => fmtRatio(v, 0) },
];

function statusTab(scope, ctx) {
  let win = '15m';
  const compHost = h('div'), chartsHost = h('div', { class: 'grid two' }), statsHost = h('div', { class: 'stats' }), note = h('div');
  ctx.onStatus((st) => { clear(compHost).append(statusDetails(st)); });
  const sel = select(WINDOWS.map((w) => ({ value: w, label: t(`window.${w}`) })), win, (v) => { win = v; loadCharts(); }, { 'aria-label': t('routing.window') });
  async function loadStats() {
    try {
      const m = await scope.api.metrics('dynamo');
      if (!scope.alive) return;
      if (!m.available) { clear(statsHost); clear(note).append(h('p', { class: 'note warn', role: 'status' }, m.error || t('dyn.metrics_unavailable'))); return; }
      clear(note);
      const v = (k, l) => valueOf(m, k, l);
      clear(statsHost).append(stat(t('dyn.requests'), fmtInt(v('dyn.requests_total'))), stat(t('dyn.in_flight'), fmtInt(v('dyn.in_flight'))), stat(t('dyn.ttft_p95'), fmtMs(v('dyn.ttft_ms', { q: 'p95' }))), stat(t('dyn.itl_p95'), fmtMs(v('dyn.itl_ms', { q: 'p95' }))),
        stat(t('dyn.kv'), fmtRatio(v('dyn.kv_cache_usage'), 0)), stat(t('dyn.models'), fmtInt(v('dyn.models'))));
    } catch (e) { if (scope.alive) clear(note).append(errorBox(e, loadStats)); }
  }
  async function loadCharts() {
    try {
      const cards = await Promise.all(SERIES.map(async (s) => {
        const res = await Promise.all(s.keys.map(([name, key]) => scope.api.history('dynamo', key, win).then((r) => ({ name, points: r.points || [] })).catch(() => ({ name, points: [] }))));
        return lineChartCard({ title: t(`chart.${s.title}`), series: res, yFormat: s.fmt, ariaLabel: t('dyn.chart_aria', { title: t(`chart.${s.title}`), win: t(`window.${win}`) }), empty: t('dyn.chart_empty') });
      }));
      if (scope.alive) clear(chartsHost).append(...cards);
    } catch (e) { if (scope.alive) clear(chartsHost).append(errorBox(e, loadCharts)); }
  }
  loadStats(); loadCharts(); scope.every(10000, loadStats); scope.every(15000, loadCharts);
  return h('div', { class: 'stack-v' }, card(t('dyn.serving'), compHost), h('div', { class: 'row gap wrap between' }, h('h2', t('dyn.metrics')), sel), note, statsHost, chartsHost);
}

function paramsTab(scope) {
  const host = h('div', { class: 'stack-v' }, skeleton(4));
  scope.api.params('dynamo').then(build).catch((e) => { if (scope.alive) { clear(host); host.append(errorBox(e)); } });
  function build(res) {
    if (!scope.alive) return;
    clear(host);
    const specs = res.catalog || [];
    if (!specs.length) { host.append(emptyBox(t('params.unsupported'))); return; }
    const applied = res.values || {};
    let draft = { ...applied };
    const formHost = h('div'), prevHost = h('div', { 'aria-live': 'polite' }), outHost = h('div', { 'aria-live': 'polite' });
    let search = '', adv = false, seq = 0;
    const rerender = () => renderParamForm(formHost, { specs, values: draft, search, showAdvanced: adv, onChange: (k, v) => { if (v === undefined) delete draft[k]; else draft[k] = v; preview(); updateApply(); } });
    const preview = debounce(async () => {
      const my = ++seq;
      try {
        const r = await scope.api.previewParams('dynamo', pruneValues(specs, draft));
        if (!scope.alive || my !== seq) return;
        clear(prevHost).append(h('pre', { class: 'cmd', tabindex: '0', 'aria-label': t('params.preview_label') }, r.rendered || t('params.preview_empty')),
          (r.warnings || []).length ? h('ul', { class: 'problems' }, r.warnings.map((w) => h('li', { class: 'problem warn' }, badge(t('sev.warn'), 'warn'), ' ', w))) : null);
      } catch (e) { if (scope.alive && my === seq) clear(prevHost).append(h('p', { class: 'note bad', role: 'alert' }, describeError(e))); }
    }, 250);
    const q = h('input', { type: 'search', 'aria-label': t('params.search'), placeholder: t('params.search'), onInput: (e) => { search = e.target.value; rerender(); } });
    const advCb = h('input', { type: 'checkbox', onChange: (e) => { adv = e.target.checked; rerender(); } });
    const applyBtn = adminBtn(t('params.apply'), { kind: 'primary', icon: 'check', onClick: onApply });
    const changed = () => changedKeys(specs, applied, draft);
    function updateApply() { applyBtn.disabled = !isAdmin() || !changed().length; }
    async function onApply() {
      const keys = changed();
      if (!keys.length) return;
      const restart = needsRestart(specs, keys);
      const ok = await confirmEffect({ title: t('params.confirm_title'), effect: restart ? t('params.effect_restart') : t('params.effect_live'), danger: restart, confirmLabel: t('params.apply'),
        lines: [...keys.map((k) => `${k}: ${JSON.stringify(applied[k] ?? null)} -> ${JSON.stringify(draft[k] ?? null)}`), t('params.confirm_override')] });
      if (!ok) return;
      applyBtn.disabled = true;
      try {
        const r = await scope.api.applyParams('dynamo', pruneValues(specs, draft));
        clear(outHost).append(outcomeBanner(r)); announce(r.ok ? t('apply.ok') : t('apply.failed'));
        if (r.ok) { const fresh = await scope.api.params('dynamo'); Object.keys(applied).forEach((k) => delete applied[k]); Object.assign(applied, fresh.values || {}); draft = { ...applied }; rerender(); preview(); }
      } catch (e) { toastError(e); clear(outHost).append(h('p', { class: 'note bad', role: 'alert' }, describeError(e))); }
      finally { updateApply(); }
    }
    rerender(); preview(); updateApply();
    host.append(h('p', { class: 'note info' }, t('params.explain')), card(t('params.title'), h('div', { class: 'stack-v' }, h('div', { class: 'row gap wrap bottom' }, q, h('label', { class: 'check' }, advCb, h('span', t('params.show_advanced')))), formHost)),
      card(t('params.preview'), prevHost, applyBtn), outHost);
  }
  return host;
}

function lifecycleTab(scope) {
  const preHost = h('div', { 'aria-live': 'polite' }), outHost = h('div', { 'aria-live': 'polite' });
  let lastPre = null;
  async function preflight() {
    clear(preHost).append(skeleton(2));
    try {
      const res = await scope.api.runAction('dynamo', 'preflight', {}, false);
      if (!scope.alive) return null;
      lastPre = res; clear(preHost).append(preflightView(res)); announce(t('dyn.preflight_done')); return res;
    } catch (e) { if (scope.alive) clear(preHost).append(errorBox(e, preflight)); return null; }
  }
  async function lifecycle(id, danger) {
    const res = await preflight();
    if (!res) return;
    const pf = parsePreflight(res.output);
    let stopBox = null;
    const lines = [];
    if (pf.container) lines.push(t('dyn.confirm_shares', { name: pf.container, state: pf.sharingRunning ? t('comp.running') : t('comp.stopped') }));
    if (pf.darkAliases.length) lines.push(t('dyn.confirm_dark', { aliases: pf.darkAliases.join(', ') }));
    lines.push(t('dyn.confirm_other_cards'));
    const params = {};
    if (id === 'up' && pf.sharingRunning) stopBox = h('input', { type: 'checkbox', id: 'stop-sharing' });
    const extra = stopBox ? h('div', { class: 'note warn' }, h('label', { class: 'check' }, stopBox, h('span', t('dyn.stop_sharing', { name: pf.container }))), h('div', { class: 'hint' }, t('dyn.stop_sharing_hint'))) : null;
    const specs = await scope.api.actions('dynamo').catch(() => []);
    const spec = specs.find((s) => s.id === id);
    const ok = await confirmEffect({ title: t('dyn.confirm_title', { name: spec?.label || id }), effect: spec?.description || t(`dyn.effect_${id}`), danger: true, confirmLabel: spec?.label || id, lines, extra });
    if (!ok) return;
    if (stopBox) params.stop_sharing_engine = stopBox.checked;
    clear(outHost).append(skeleton(1));
    try {
      const r = await scope.api.runAction('dynamo', id, params, true);
      if (!scope.alive) return;
      clear(outHost).append(actionResultView(r, spec?.label || id)); announce(t(r.ok ? 'act.done_ok' : 'act.done_failed', { name: spec?.label || id }));
      preflight();
    } catch (e) { if (scope.alive) { clear(outHost).append(h('div', { class: 'note bad', role: 'alert' }, describeError(e))); toastError(e); } }
  }
  // Preflight is a POST (admin-only per the contract), so a viewer gets an explanation instead of a 403.
  if (isAdmin()) preflight(); else preHost.append(h('p', { class: 'note info' }, t('dyn.preflight_viewer')));
  const controls = h('div', { class: 'row gap wrap' }, adminBtn(t('dyn.up'), { kind: 'primary', icon: 'play', onClick: () => lifecycle('up') }), adminBtn(t('dyn.down'), { kind: 'danger', icon: 'stop', onClick: () => lifecycle('down') }), adminBtn(t('dyn.restart'), { icon: 'restart', onClick: () => lifecycle('restart') }));
  return h('div', { class: 'stack-v' }, card(t('dyn.preflight'), preHost, adminBtn(t('dyn.rerun_preflight'), { size: 'sm', icon: 'restart', onClick: preflight })),
    card(t('dyn.lifecycle'), h('div', { class: 'stack-v' }, h('p', { class: 'muted' }, t('dyn.lifecycle_hint')), controls, outHost)),
    h('h2', { class: 'section' }, t('dyn.other_actions')), actionsPanel(scope, 'dynamo', { only: ['up_mock'] }));
}

export function preflightView(res) {
  const pf = parsePreflight(res.output);
  const shares = pf.container ? h('div', { class: `preflight-hero ${pf.sharingRunning ? 'warn' : 'ok'}` },
    h('div', h('div', { class: 'muted small' }, t('dyn.shares_card')), h('strong', { class: 'big-name' }, pf.container), ' ', badge(pf.sharingRunning ? t('comp.running') : t('comp.stopped'), pf.sharingRunning ? 'warn' : 'ok')),
    pf.darkAliases.length ? h('div', h('div', { class: 'muted small' }, t('dyn.go_dark')), h('div', { class: 'row gap wrap' }, pf.darkAliases.map((a) => badge(a, 'bad')))) : null) : null;
  return h('div', { class: 'stack-v preflight' }, h('div', { class: 'row gap wrap' }, badge(res.ok ? t('act.ok') : t('act.failed'), res.ok ? 'ok' : 'bad'), h('span', { class: 'muted small' }, t('dyn.preflight_note'))),
    shares, problemList(res.problems),
    h('details', { open: !shares }, h('summary', t('dyn.preflight_raw')), h('pre', { class: 'cmd output', tabindex: '0' }, res.output || t('act.no_output'))));
}

class NcDynamo extends NcView {
  setup() {
    systemPage(this, { system: 'dynamo', active: this.query?.tab, tabDefs: [
      { id: 'status', label: t('tab.status'), render: (scope, ctx) => statusTab(scope, ctx) },
      { id: 'params', label: t('tab.params'), render: (scope) => paramsTab(scope) },
      { id: 'lifecycle', label: t('tab.lifecycle'), render: (scope) => lifecycleTab(scope) },
      { id: 'playground', label: t('tab.playground'), render: (scope) => playgroundPanel(scope, 'dynamo') },
    ] });
  }
}
customElements.define('nc-dynamo', NcDynamo);
