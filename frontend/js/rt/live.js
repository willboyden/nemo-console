// Polite polling for the explorer: one request at a time, a newer request cancels an older one, nothing while the browser
// tab is hidden (a refresh runs when it is shown again), identical answers are not re-rendered, and the scope owns it all.
export function liveLoad(scope, { load, onData, onError, ms = 0, enabled = () => true }) {
  let ctl = null,
    seq = 0,
    last = null;
  const run = async ({ quiet = false } = {}) => {
    ctl?.abort();
    ctl = new AbortController();
    const my = ++seq;
    try {
      const data = await load(ctl.signal);
      if (!scope.alive || my !== seq) return;
      const raw = JSON.stringify(data);
      const changed = raw !== last;
      last = raw;
      onData(data, { changed, quiet });
    } catch (e) {
      if (e?.name === 'AbortError' || !scope.alive || my !== seq) return;
      onError?.(e, { quiet });
    }
  };
  if (ms) scope.every(ms, () => !document.hidden && enabled() && run({ quiet: true }));
  scope.listen(document, 'visibilitychange', () => !document.hidden && enabled() && run({ quiet: true }));
  scope.own(() => ctl?.abort());
  return { refresh: run, forget: () => (last = null) };
}
