// One persistent polite live region for async results (apply outcomes, health flips, action results).
let el = null;
function region() {
  if (el && el.isConnected) return el;
  el = document.createElement('div');
  el.className = 'sr-only'; el.id = 'live-region';
  el.setAttribute('role', 'status'); el.setAttribute('aria-live', 'polite'); el.setAttribute('aria-atomic', 'true');
  document.body.append(el);
  return el;
}
export function announce(text) {
  const r = region();
  r.textContent = '';
  // Re-set on the next tick so identical consecutive messages are still announced.
  setTimeout(() => { r.textContent = text; }, 30);
}
