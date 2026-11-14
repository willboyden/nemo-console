// Width-aware SVG charts: build(width) is called with the host's real width, so text is drawn at its true size at every
// layout (a fixed viewBox would scale the labels up on a wide card and down on a narrow one). Redrawn when the host resizes.
import { h } from '../dom.js';

/** Put `first` (built at a default width) into host and redraw it whenever the host's width changes by more than a few px. */
export function mountFit(scope, host, build, first, { minW = 240 } = {}) {
  host.replaceChildren(first);
  if (typeof ResizeObserver === 'undefined') return;
  let last = 0;
  const ro = new ResizeObserver(() => {
    const w = Math.round(host.clientWidth);
    if (w < 80 || Math.abs(w - last) < 6) return;
    last = w;
    const n = build(Math.max(minW, w));
    if (n) host.replaceChildren(n);
  });
  ro.observe(host);
  scope.own(() => ro.disconnect());
}

/** A host div that draws build(width) (or `empty` when build returns null). Returns the host; null build -> the empty node. */
export function fitted(scope, build, empty, defaultW = 560) {
  const host = h('div', { class: 'chart-wrap-lg' });
  const first = build(defaultW);
  if (!first) return empty;
  mountFit(scope, host, build, first);
  return host;
}
