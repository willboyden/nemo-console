// URL-hash state of a tab: `#/<path>?tab=<tab>&window=7d&max=1&run=<id>...` (addendum 9.13). The tab keeps its query in memory
// and mirrors it into the URL with history.replaceState, which does not fire `hashchange`, so the page is not remounted;
// Back/Forward do fire it and remount the page, which reads the restored URL. The generic form of js/rt/nav.js (Relay's own
// state, which also remembers a window and an anchor across tabs); this one knows nothing about any system.
import { parseHash, buildHash } from '../router.js';

/** The query without `tab` and without empty values; every value is a string. */
export function cleanQuery(o) {
  const out = {};
  for (const [k, v] of Object.entries(o || {})) if (k !== 'tab' && v != null && v !== '' && v !== false) out[k] = String(v);
  return out;
}

/** `#/dcgm?tab=status&window=7d` from a path (no slash), a tab and a query. */
export const hashFor = (path, tab, query) => buildHash(`/${path}`, { tab, ...cleanQuery(query) });

/**
 * @param path     route path without the slash (`dcgm`)
 * @param tab      this tab's id
 * @param initial  true on the first render of the page (a deep link's query is meant), false after a click on the tab
 *                 (which starts clean); systemPage hands this over as ctx.initial
 * @param loc,hist injectable for tests
 * @param keys     when given, only these query keys are kept from the URL on a deep link
 */
export function createHashState({ path, tab, initial = true, keys = null, loc = globalThis.location, hist = globalThis.history, defer = queueMicrotask }) {
  let q = initial ? cleanQuery(parseHash(loc?.hash || '').query) : {};
  if (keys) q = Object.fromEntries(Object.entries(q).filter(([k]) => keys.some((p) => (p instanceof RegExp ? p.test(k) : p === k))));
  const listeners = new Set();
  const write = (push) => {
    try {
      hist[push ? 'pushState' : 'replaceState'](null, '', hashFor(path, tab, q));
    } catch {
      /* some embeds forbid history changes: the in-memory state still works */
    }
  };
  const state = {
    path,
    tab,
    all: () => ({ ...q }),
    get: (k, dflt = '') => q[k] ?? dflt,
    has: (k) => k in q,
    /** Merge a patch (null / '' / false removes a key) into the state and the URL; listeners hear the patch. */
    set(patch, { push = false } = {}) {
      q = cleanQuery({ ...q, ...patch });
      write(push);
      for (const f of [...listeners]) f(patch);
    },
    /** Drop every key matching `test` (a RegExp or a prefix string) in one write. */
    clear(test) {
      const hit = (k) => (test instanceof RegExp ? test.test(k) : k.startsWith(test));
      q = Object.fromEntries(Object.entries(q).filter(([k]) => !hit(k)));
      write(false);
    },
    onChange(f) {
      listeners.add(f);
      return () => listeners.delete(f);
    },
    href: (patch = {}) => hashFor(path, tab, { ...q, ...patch }),
    sync: () => write(false),
  };
  // The page frame writes `?tab=<id>` after it renders the panel; write the real query once that is done.
  defer(() => state.sync());
  return state;
}
