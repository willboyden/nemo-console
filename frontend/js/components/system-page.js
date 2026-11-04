// Page frames with tabs. helpTabs(): ARIA tabs with a scope per panel, the active tab mirrored into the hash query
// (history.replaceState: no remount, deep links work), and two help tabs after the page's own, Getting started and
// FAQ (help-panel.js). systemPage(): a managed system's page, helpTabs under a polled header (health, summary,
// problems). consolePage(): the same tabs for the console's own pages (Overview, Audit, Settings), no header.
import { h, clear, append } from '../dom.js';
import { t } from '../i18n.js';
import { problemList, tabs, errorBox } from './ui.js';
import { statusSummary } from './status-panel.js';
import { announce } from './live.js';
import { isLoading } from '../status-util.js';
import { gettingStartedPanel, faqPanel } from './help-panel.js';

/**
 * @param scope   the view's scope; each panel gets a child scope that is disposed when another tab opens
 * @param path    route path without the slash (`dynamo`, `settings`), for the `#/<path>?tab=<id>` mirror
 * @param helpKey the entry in i18n/help.en.json; name: the page's display name, for the tab list and help headings
 * @param panelCtx (panelScope) => the second argument handed to each tab's render
 */
export function helpTabs(scope, { path, helpKey, name, tabDefs: ownTabs, active, panelCtx = () => ({}) }) {
  // Help links may switch tabs; a link to a tab the page did not build (optional Config / Playground) is not shown.
  const nav = {
    hasTab: (id) => tabDefs.some((d) => d.id === id),
    tabLabel: (id) => tabDefs.find((d) => d.id === id)?.label ?? id,
    goTab: (id) => tablist.selectTab(id),
  };
  const help = { key: helpKey, name, nav };
  const tabDefs = [...ownTabs,
    { id: 'start', label: t('tab.start'), render: (s) => gettingStartedPanel(s, help) },
    { id: 'faq', label: t('tab.faq'), render: (s) => faqPanel(s, help) }];
  const panel = h('div', { id: 'tabpanel', role: 'tabpanel', tabindex: '-1', class: 'tabpanel' });
  let panelScope = null;
  function show(id) {
    panelScope?.dispose(); panelScope = scope.child();
    const def = tabDefs.find((d) => d.id === id) || tabDefs[0];
    clear(panel).append(def.render(panelScope, panelCtx(panelScope)));
    try { history.replaceState(null, '', `#/${path}?tab=${def.id}`); } catch { /* ignore */ }
    panel.setAttribute('aria-labelledby', `tab-${def.id}`);
  }
  const startId = tabDefs.some((d) => d.id === active) ? active : tabDefs[0].id;
  const tablist = tabs(tabDefs.map((d) => ({ id: d.id, label: d.label })), startId, show, t('sys.tabs', { name }));
  return { tablist, panel, show, open: () => show(startId) };
}

export function systemPage(view, { system, tabDefs, active }) {
  const scope = view.scope;
  const head = h('div', { class: 'sys-head' });
  let last = null;
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
  const area = helpTabs(scope, { path: system, helpKey: system, name: t(`sys.${system}`), tabDefs, active,
    panelCtx: (ps) => ({ lastStatus: () => last, onStatus: (f) => { cbs.add(f); ps.own(() => cbs.delete(f)); if (last) f(last); } }) });
  view.append(h('h1', t(`sys.${system}`)), head, area.tablist, area.panel);
  refreshHead(); scope.every(5000, refreshHead);
  area.open();
  return { refreshHead, show: area.show };
}

/** Overview, Audit, Settings: the page's own content as the first tab, then the help tabs. `page` is the route path. */
export function consolePage(view, { page, tabDefs, active }) {
  const area = helpTabs(view.scope, { path: page, helpKey: page, name: t(`nav.${page}`), tabDefs, active });
  view.append(h('h1', t(`nav.${page}`)), area.tablist, area.panel);
  area.open();
  return area;
}
