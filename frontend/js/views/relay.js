import { NcView } from '../components/base.js';
import { h, clear, append } from '../dom.js';
import { t } from '../i18n.js';
import { fmtInt, fmtBytes } from '../format.js';
import { valueOf } from '../metrics.js';
import { attestation } from '../status-util.js';
import { badge, card, stat, errorBox } from '../components/ui.js';
import { systemPage } from '../components/system-page.js';
import { statusBody, componentsTable, endpointsList } from '../components/status-panel.js';
import { eventsPanel } from '../components/events-panel.js';
import { configEditor } from '../components/config-editor.js';
import { playgroundPanel } from '../components/playground.js';
import { actionsPanel } from '../components/actions-panel.js';

const ATT_KIND = { verified: 'ok', reported: 'info', failed: 'bad', unknown: 'warn', unreported: 'muted' };

function statusTab(scope, ctx) {
  const attHost = h('div', { class: 'row gap wrap' }), compHost = h('div'), statsHost = h('div', { class: 'stats' });
  ctx.onStatus((st) => {
    const a = attestation(st);
    append(clear(attHost), [badge(a.state === 'reported' ? a.value : t(`relay.att_${a.state}`), ATT_KIND[a.state], a.detail || undefined), a.detail ? h('span', { class: 'muted small' }, a.detail) : null]);
    append(clear(compHost), [componentsTable(st.components), st.endpoints?.length ? h('h3', { class: 'sub' }, t('status.endpoints')) : null, endpointsList(st.endpoints)]);
  });
  async function loadStats() {
    try {
      const m = await scope.api.metrics('relay');
      if (!scope.alive) return;
      if (!m.available) { clear(statsHost).append(h('p', { class: 'note warn' }, m.error || t('relay.metrics_unavailable'))); return; }
      const v = (k) => valueOf(m, k);
      const yn = (x) => (x == null ? '–' : x ? t('common.yes') : t('common.no'));
      clear(statsHost).append(stat(t('relay.gateway_up'), yn(v('relay.gateway_up'))), stat(t('relay.plugin_loaded'), yn(v('relay.plugin_loaded'))), stat(t('relay.atof_files'), fmtInt(v('relay.atof_files')), fmtBytes(v('relay.atof_bytes'))),
        stat(t('relay.atif'), fmtInt(v('relay.atif_trajectories'))), stat(t('relay.exporters'), fmtInt(v('relay.exporters_enabled'))));
    } catch (e) { if (scope.alive) clear(statsHost).append(errorBox(e, loadStats)); }
  }
  loadStats(); scope.every(10000, loadStats);
  return h('div', { class: 'stack-v' }, card(t('relay.attestation'), attHost), card(t('relay.gateway_state'), compHost), card(t('relay.atof_atif'), h('div', { class: 'stack-v' }, statsHost, h('p', { class: 'hint' }, t('relay.counts_hint')))),
    card(t('relay.events'), eventsPanel(scope, 'relay', { mode: 'generic' })));
}

class NcRelay extends NcView {
  async setup() {
    // The Playground tab exists only if the backend offers one for this system.
    let pg = null; try { pg = await this.scope.api.playground('relay'); } catch { /* tab simply absent */ }
    if (!this.scope.alive) return;
    const tabDefs = [
      { id: 'status', label: t('tab.status'), render: (scope, ctx) => statusTab(scope, ctx) },
      { id: 'config', label: t('tab.config'), render: (scope) => configEditor(scope, 'relay') },
      { id: 'diagnostics', label: t('tab.diagnostics'), render: (scope) => actionsPanel(scope, 'relay') },
    ];
    if (pg) tabDefs.push({ id: 'playground', label: t('tab.playground'), render: (scope) => playgroundPanel(scope, 'relay') });
    systemPage(this, { system: 'relay', active: this.query?.tab, tabDefs });
  }
}
customElements.define('nc-relay', NcRelay);
