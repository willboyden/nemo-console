import { NcView } from '../components/base.js';
import { h, clear } from '../dom.js';
import { t } from '../i18n.js';
import { healthChanges } from '../status-util.js';
import { card, healthBadge, compBadge, problemList, errorBox, skeleton, emptyBox, btn } from '../components/ui.js';
import { topologyGraph } from '../components/topology.js';
import { announce } from '../components/live.js';
import { consolePage } from '../components/system-page.js';

class NcOverview extends NcView {
  setup() {
    consolePage(this, { page: 'overview', active: this.query?.tab, tabDefs: [{ id: 'status', label: t('tab.status'), render: (scope) => this.statusTab(scope) }] });
  }
  /** Topology + one card per system, polled every 5 s while this tab is open. */
  statusTab(scope) {
    const topoHost = h('div', {}, skeleton(3)), tiles = h('div', { class: 'grid tiles' }, skeleton(3));
    const load = async () => {
      try {
        const ov = await scope.api.overview();
        if (!scope.alive) return;
        for (const c of healthChanges(this.prev, ov)) announce(t('sys.health_changed', { name: t(`sys.${c.id}`), health: t(`health.${c.to}`) }));
        this.prev = ov;
        clear(topoHost).append(topologyGraph(ov.topology));
        clear(tiles).append(...ov.systems.map((x) => this.tile(x)));
      } catch (e) { if (scope.alive && !this.prev) { clear(topoHost).append(errorBox(e, load)); clear(tiles).append(emptyBox(t('overview.no_data'))); } }
    };
    load(); scope.every(5000, load);
    return h('div', { class: 'stack-v' }, card(t('overview.topology'), topoHost, btn(t('common.refresh'), { size: 'sm', icon: 'restart', onClick: load })),
      h('h2', { class: 'section' }, t('overview.systems')), tiles);
  }
  tile(s) {
    return h('section', { class: `card tile ${s.health}`, 'aria-label': t(`sys.${s.system}`) },
      h('header', { class: 'card-head' }, h('h3', h('a', { href: `#/${s.system}` }, t(`sys.${s.system}`))), healthBadge(s.health)),
      h('div', { class: 'card-body' }, h('div', { class: 'muted small' }, s.version ? `v${s.version}` : t('overview.no_version')), h('p', { class: 'status-summary' }, s.summary),
        h('div', { class: 'row gap wrap' }, s.components.map((c) => h('span', { class: 'comp-chip' }, h('code', c.name), ' ', compBadge(c.state)))),
        s.problems.length ? h('div', {}, h('h4', { class: 'sub' }, t('overview.problems', { count: s.problems.length })), problemList(s.problems)) : h('p', { class: 'note ok' }, t('overview.no_problems')),
        h('div', { class: 'row end' }, h('a', { class: 'btn', href: `#/${s.system}` }, t('overview.open')))));
  }
}
customElements.define('nc-overview', NcOverview);
