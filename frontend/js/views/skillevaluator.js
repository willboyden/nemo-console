import { NcView } from '../components/base.js';
import { h, clear, append } from '../dom.js';
import { t } from '../i18n.js';
import { fmtInt } from '../format.js';
import { valueOf } from '../metrics.js';
import { card, stat, errorBox } from '../components/ui.js';
import { systemPage } from '../components/system-page.js';
import { componentsTable, endpointsList } from '../components/status-panel.js';
import { eventsPanel } from '../components/events-panel.js';
import { actionsPanel } from '../components/actions-panel.js';

const score = (x) => (x == null ? '–' : Number(x).toFixed(1));
const lift = (x) => (x == null ? '–' : `${x >= 0 ? '+' : ''}${Number(x).toFixed(3)}`);

function statusTab(scope, ctx) {
  const compHost = h('div'), statsHost = h('div', { class: 'stats' });
  ctx.onStatus((st) => {
    append(clear(compHost), [componentsTable(st.components), st.endpoints?.length ? h('h3', { class: 'sub' }, t('status.endpoints')) : null, endpointsList(st.endpoints)]);
  });
  async function loadStats() {
    try {
      const m = await scope.api.metrics('skillevaluator');
      if (!scope.alive) return;
      if (!m.available) { clear(statsHost).append(h('p', { class: 'note warn' }, m.error || t('se.metrics_unavailable'))); return; }
      const v = (k) => valueOf(m, k);
      clear(statsHost).append(stat(t('se.skills'), fmtInt(v('se.skills'))), stat(t('se.runs'), fmtInt(v('se.runs'))),
        stat(t('se.last_quality'), score(v('se.last_quality_score'))), stat(t('se.last_judge'), score(v('se.last_judge_score'))),
        stat(t('se.last_lift'), lift(v('se.last_skill_lift'))), stat(t('se.live_running'), v('se.live_running') == null ? '–' : t(v('se.live_running') ? 'common.yes' : 'common.no')));
    } catch (e) { if (scope.alive) clear(statsHost).append(errorBox(e, loadStats)); }
  }
  loadStats(); scope.every(10000, loadStats);
  return h('div', { class: 'stack-v' },
    card(t('se.state'), compHost),
    card(t('se.overview'), h('div', { class: 'stack-v' }, statsHost, h('p', { class: 'hint' }, t('se.tiers_hint')))),
    card(t('se.runs_list'), eventsPanel(scope, 'skillevaluator', { mode: 'generic' })));
}

class NcSkillEvaluator extends NcView {
  async setup() {
    systemPage(this, { system: 'skillevaluator', active: this.query?.tab, tabDefs: [
      { id: 'status', label: t('tab.status'), render: (scope, ctx) => statusTab(scope, ctx) },
      { id: 'evaluate', label: t('tab.evaluate'), render: (scope) => actionsPanel(scope, 'skillevaluator') },
    ] });
  }
}
customElements.define('nc-skillevaluator', NcSkillEvaluator);
