// URL state of the Relay explorer tabs: `#/relay?tab=traces&trace=<id>&span=<id>&window=24h ...`. The tab keeps its query
// in memory and mirrors it into the URL with history.replaceState (filters) or pushState (opening a detail, so Back
// closes it). Neither fires `hashchange`, so the page is not remounted; Back/Forward do fire it and remount the page,
// which reads the restored URL. A link to another tab goes through `ctx.goTab` with a hand-off of its query.
import { parseHash, buildHash } from '../router.js';
import { h } from '../dom.js';

export const WINDOWS = ['1h', '6h', '24h', '7d', '30d', 'all'];
export const ANCHORS = ['latest', 'now'];
const memory = { window: '24h', anchor: 'latest' }; // the last choice survives switching tabs within one page load
let handoff = null;
const clean = (o) => Object.fromEntries(Object.entries(o || {}).filter(([k, v]) => k !== 'tab' && v != null && v !== ''));

export function createNav(tab, ctx) {
  const taken = handoff && handoff.tab === tab ? handoff.query : null;
  if (taken) handoff = null;
  // A click on the tab itself (ctx.initial === false) starts from a clean list; a page load or deep link keeps its URL.
  let q = clean(taken ?? (ctx?.initial === false ? {} : parseHash(location.hash).query));
  if (!WINDOWS.includes(q.window)) delete q.window;
  if (!ANCHORS.includes(q.anchor)) delete q.anchor;
  const listeners = new Set();
  const write = (push) => {
    try {
      history[push ? 'pushState' : 'replaceState'](null, '', buildHash('/relay', { tab, ...q }));
    } catch {
      /* some embeds forbid history changes: the in-memory state still works */
    }
  };
  const nav = {
    tab,
    q: () => q,
    get: (k, dflt = '') => q[k] ?? (k === 'window' || k === 'anchor' ? memory[k] : dflt),
    /** Merge a patch (null/'' removes a key) into the state and the URL; listeners hear about it. */
    set(patch, { push = false } = {}) {
      q = clean({ ...q, ...patch });
      if ('window' in patch && patch.window) memory.window = patch.window;
      if ('anchor' in patch && patch.anchor) memory.anchor = patch.anchor;
      write(push);
      for (const f of [...listeners]) f(patch);
    },
    /** Replace the whole query except window/anchor (a new view starts clean). */
    replaceAll(query, opts) {
      const keep = { window: q.window, anchor: q.anchor };
      q = clean({ ...keep, ...query });
      write(opts?.push);
      for (const f of [...listeners]) f(query);
    },
    onChange(f) {
      listeners.add(f);
      return () => listeners.delete(f);
    },
    /** `#/relay?...` for a link: tab defaults to this one; window and anchor are kept; patch overrides. */
    href(query = {}, tabId = tab) {
      const same = tabId === tab;
      const base = same ? q : { window: q.window ?? (memory.window !== '24h' ? memory.window : null), anchor: q.anchor ?? (memory.anchor !== 'latest' ? memory.anchor : null) };
      return buildHash('/relay', { tab: tabId, ...clean({ ...base, ...query }) });
    },
    /** Open a view: in this tab merge into the query (push); in another tab hand the query over and switch. */
    go(query = {}, tabId = tab) {
      if (tabId === tab) return nav.set(query, { push: true });
      const keep = { window: q.window, anchor: q.anchor };
      try {
        history.pushState(null, '', buildHash('/relay', { tab: tabId, ...clean({ ...keep, ...query }) }));
      } catch {
        /* ignore */
      }
      handoff = { tab: tabId, query: clean({ ...keep, ...query }) };
      ctx?.goTab?.(tabId);
    },
    sync: () => write(false),
  };
  // The page frame writes `?tab=<id>` after it renders the panel; write the real query once that is done.
  queueMicrotask(() => nav.sync());
  return nav;
}

/** An anchor that opens `query` (in `tabId`) without remounting the page; a modified click or middle click opens the URL. */
export function navLink(nav, query, attrs, children, tabId = nav.tab) {
  const a = h('a', { ...attrs, href: nav.href(query, tabId) }, children);
  a.addEventListener('click', (e) => {
    if (e.defaultPrevented || e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
    e.preventDefault();
    nav.go(query, tabId);
  });
  return a;
}
