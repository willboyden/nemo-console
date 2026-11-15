// A cache of per-key fetches for the wave-3 run detail cards (addendum 9.15). Pure (no DOM), unit-tested in tests/fetch-cache.test.mjs.
/**
 * A cache of per-key fetches. `load(key, onChange)` starts the fetch of a key once and returns its entry {state: 'loading' | 'ok' | 'fail', value, error}; `onChange` is called when
 * a fetch that was loading settles. A failure is kept for `retryAfterMs` (so a missing route is not asked for again on every redraw) and is then fetched again by the next `load`; `retry`
 * forgets the entry at once. `peek(key)` reads without starting anything ({state: 'none'} for a key never loaded).
 */
export function fetchCache(fetcher, { retryAfterMs = 15000, now = () => Date.now() } = {}) {
  const entries = new Map();
  const touch = (key, onChange) => {
    let e = entries.get(key);
    if (e && !(e.state === 'fail' && now() - e.at > retryAfterMs)) {
      if (e.state === 'loading' && onChange) e.listeners.add(onChange);
      return e;
    }
    e = { state: 'loading', value: null, error: null, at: now(), listeners: new Set(onChange ? [onChange] : []) };
    entries.set(key, e);
    const done = () => {
      const fs = [...e.listeners];
      e.listeners.clear();
      for (const f of fs) f();
    };
    Promise.resolve()
      .then(() => fetcher(key))
      .then(
        (v) => {
          Object.assign(e, { state: 'ok', value: v });
          done();
        },
        (err) => {
          Object.assign(e, { state: 'fail', error: err, at: now() });
          done();
        },
      );
    return e;
  };
  return {
    /** Start (or reuse) the fetch of `key`; `onChange` is called once when it settles (not at all if it already has). Returns the entry {state, value, error}. */
    load: (key, onChange) => touch(key, onChange),
    peek: (key) => entries.get(key) ?? { state: 'none', value: null, error: null },
    retry: (key, onChange) => {
      entries.delete(key);
      return touch(key, onChange);
    },
    size: () => entries.size,
  };
}
