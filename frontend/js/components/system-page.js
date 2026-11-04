// Shared page frame for a managed system: header (health/summary/problems, polled), ARIA tabs with a scope per panel,
// and the active tab mirrored into the hash query (history.replaceState: no remount, deep links work). Every page
// also gets two help tabs at the end, Getting started and FAQ (help-panel.js), whatever tabs its view defines.
import { h, clear, append } from '../dom.js';
import { t } from '../i18n.js';
import { healthBadge, problemList, tabs, errorBox } from './ui.js';
import { statusSummary } from './status-panel.js';
import { announce } from './live.js';
import { isLoading } from '../status-util.js';
import { gettingStartedPanel, faqPanel } from './help-panel.js';

export function systemPage(view, { system, tabDefs: ownTabs, active }) {
  const scope = view.scope;
  // Help links may switch tabs; a link to a tab the page did not build (optional Config / Playground) is not shown.
  const nav = {
    hasTab: (id) => tabDefs.some((d) => d.id === id),
    tabLabel: (id) => tabDefs.find((d) => d.id === id)?.label ?? id,
    goTab: (id) => tablist.selectTab(id),
  };
  const tabDefs = [...ownTabs,
    { id: 'start', label: t('tab.start'), render: (s) => gettingStartedPanel(s, system, nav) },
    { id: 'faq', label: t('tab.faq'), render: (s) => faqPanel(s, system, nav) }];
  const head = h('div', { class: 'sys-head' }), panel = h('div', { id: 'tabpanel', role: 'tabpanel', tabindex: '-1', class: 'tabpanel' });
  let last = null, panelScope = null, current = null;
  const cbs = new Set();

  async function refreshHead() {
    try {
      const st = await scope.api.status(system);
      if (!scope.alive) return;
      if (last && last.health !== st.health) announce(t('sys.health_changed', { name: t(`sys.${system}`), health: t(`health.${st.health}`) }));
      last = st;
      append(clear(head), [statusSummary(st), isLoading(st) ? h('p', { class: 'note info', role: 'status' }, h('strong', t('sys.loading')), ' ', t('sys.loading_hint')) : null, problemList(st.problems)]);
      for (const f of cbs) f(st);
    } catch (e) { if (scope.alive) clear(head).append(errorBox(e, refreshHead)); }
  }
  function show(id) {
    panelScope?.dispose(); panelScope = scope.child(); current = id;
    const def = tabDefs.find((d) => d.id === id) || tabDefs[0];
    clear(panel).append(def.render(panelScope, { lastStatus: () => last, onStatus: (f) => { cbs.add(f); panelScope.own(() => cbs.delete(f)); if (last) f(last); } }));
    try { history.replaceState(null, '', `#/${system}?tab=${def.id}`); } catch { /* ignore */ }
    panel.setAttribute('aria-labelledby', `tab-${def.id}`);
  }
  const startId = tabDefs.some((d) => d.id === active) ? active : tabDefs[0].id;
  const tablist = tabs(tabDefs.map((d) => ({ id: d.id, label: d.label })), startId, show, t('sys.tabs', { name: t(`sys.${system}`) }));
  view.append(h('h1', t(`sys.${system}`)), head, tablist, panel);
  refreshHead(); scope.every(5000, refreshHead);
  show(startId);
  return { refreshHead, show };
}
