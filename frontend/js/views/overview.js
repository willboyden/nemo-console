import { NcView } from '../components/base.js';
import { h, clear } from '../dom.js';
import { t } from '../i18n.js';
import { healthChanges } from '../status-util.js';
import { card, healthBadge, compBadge, problemList, errorBox, skeleton, emptyBox, btn } from '../components/ui.js';
import { topologyGraph } from '../components/topology.js';
import { announce } from '../components/live.js';

class NcOverview extends NcView {
  setup() {
    this.topoHost = h('div', {}, skeleton(3)); this.tiles = h('div', { class: 'grid tiles' }, skeleton(3));
    this.append(h('h1', t('nav.overview')), card(t('overview.topology'), this.topoHost, btn(t('common.refresh'), { size: 'sm', icon: 'restart', onClick: () => this.load() })),
      h('h2', { class: 'section' }, t('overview.systems')), this.tiles);
    this.load(); this.scope.every(5000, () => this.load());
  }
  async load() {
    try {
      const ov = await this.scope.api.overview();
      if (!this.scope.alive) return;
      for (const c of healthChanges(this.prev, ov)) announce(t('sys.health_changed', { name: t(`sys.${c.id}`), health: t(`health.${c.to}`) }));
      this.prev = ov;
      clear(this.topoHost).append(topologyGraph(ov.topology));
      clear(this.tiles).append(...ov.systems.map((s) => this.tile(s)));
    } catch (e) { if (this.scope.alive && !this.prev) { clear(this.topoHost).append(errorBox(e, () => this.load())); clear(this.tiles).append(emptyBox(t('overview.no_data'))); } }
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
