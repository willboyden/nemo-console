import { NcView } from '../components/base.js';
import { h, clear } from '../dom.js';
import { t } from '../i18n.js';
import { healthChanges } from '../status-util.js';
import { card, errorBox, skeleton, emptyBox, btn } from '../components/ui.js';
import { topologyGraph } from '../components/topology.js';
import { announce } from '../components/live.js';
import { consolePage } from '../components/system-page.js';
import { createTile } from '../overview/tile.js';
import { problemsCard } from '../overview/problems-card.js';
import { SPARK_STALE_MS } from '../overview-model.js';

class NcOverview extends NcView {
  setup() {
    consolePage(this, { page: 'overview', active: this.query?.tab, tabDefs: [{ id: 'status', label: t('tab.status'), render: (scope) => this.statusTab(scope) }] });
  }
  /**
   * Topology + the problem counts + one card per system, polled every 5 s while this tab is open. A tile is created once and updated in place; its sparkline (addendum 9.16) is fetched
   * lazily when the tile scrolls near the viewport (one batched series request per system) and refreshed at most once a minute while it stays in view.
   */
  statusTab(scope) {
    const topoHost = h('div', {}, skeleton(3)),
      tiles = h('div', { class: 'grid tiles' }, skeleton(3));
    const problems = problemsCard(scope);
    problems.el.hidden = true;
    const views = new Map(); // system -> tile
    const inView = new Set();
    // ---- lazy sparklines: a tile asks for its data when it is near the viewport (without IntersectionObserver, every tile asks, one at a time)
    const io =
      typeof IntersectionObserver === 'undefined'
        ? null
        : new IntersectionObserver(
            (entries) => {
              for (const e of entries) {
                const sys = e.target.dataset.system;
                if (e.isIntersecting) {
                  inView.add(sys);
                  views.get(sys)?.ensure();
                } else inView.delete(sys);
              }
            },
            { rootMargin: '160px' },
          );
    scope.own(() => io?.disconnect());
    scope.every(SPARK_STALE_MS, () => {
      for (const sys of inView) views.get(sys)?.ensure();
    });
    let drawn = '';
    const load = async () => {
      try {
        const ov = await scope.api.overview();
        if (!scope.alive) return;
        for (const c of healthChanges(this.prev, ov)) announce(t('sys.health_changed', { name: t(`sys.${c.id}`), health: t(`health.${c.to}`) }));
        this.prev = ov;
        clear(topoHost).append(topologyGraph(ov.topology));
        problems.update(ov.systems);
        problems.el.hidden = false;
        for (const s of ov.systems) {
          let v = views.get(s.system);
          if (!v) {
            v = createTile(scope, s.system);
            v.el.dataset.system = s.system;
            views.set(s.system, v);
            io?.observe(v.el);
            if (!io) queueMicrotask(() => v.ensure());
          }
          v.update(s);
        }
        const order = ov.systems.map((s) => s.system).join('|');
        if (order !== drawn) {
          drawn = order;
          clear(tiles).append(...ov.systems.map((s) => views.get(s.system).el));
        }
      } catch (e) {
        if (scope.alive && !this.prev) {
          clear(topoHost).append(errorBox(e, load));
          clear(tiles).append(emptyBox(t('overview.no_data')));
        }
      }
    };
    load();
    scope.every(5000, load);
    return h(
      'div',
      { class: 'stack-v' },
      card(t('overview.topology'), topoHost, btn(t('common.refresh'), { size: 'sm', icon: 'restart', onClick: load })),
      problems.el,
      h('h2', { class: 'section' }, t('overview.systems')),
      tiles,
    );
  }
}
customElements.define('nc-overview', NcOverview);
