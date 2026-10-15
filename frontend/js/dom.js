// Safe DOM construction: text goes through textContent/createTextNode, never innerHTML.
const PROPS = new Set(['value', 'checked', 'selected', 'indeterminate', 'disabled', 'open']);
export function h(tag, attrs, ...children) {
  const el = document.createElement(tag);
  if (attrs && (typeof attrs !== 'object' || attrs instanceof Node || Array.isArray(attrs))) { children.unshift(attrs); attrs = null; }
  for (const [k, v] of Object.entries(attrs || {})) {
    if (v == null || v === false) continue;
    if (k === 'style') throw new Error('inline style attributes are forbidden by the CSP; use a CSS class or el.style.prop');
    if (k === 'class') el.className = v;
    else if (k === 'dataset') Object.assign(el.dataset, v);
    else if (k.startsWith('on') && typeof v === 'function') el.addEventListener(k.slice(2).toLowerCase(), v);
    else if (PROPS.has(k)) el[k] = v;
    else el.setAttribute(k, v === true ? '' : String(v));
  }
  append(el, children);
  return el;
}
export function append(el, children) {
  for (const c of children.flat(Infinity)) {
    if (c == null || c === false) continue;
    el.append(c instanceof Node ? c : document.createTextNode(String(c)));
  }
  return el;
}
export const clear = (el) => { el.replaceChildren(); return el; };
export const $ = (sel, root = document) => root.querySelector(sel);
export const $$ = (sel, root = document) => [...root.querySelectorAll(sel)];

// SVG icons are static path data (trusted constants), 24x24 stroke icons.
const ICONS = {
  overview: 'M12 4a2 2 0 100 .01M5 19a2 2 0 100 .01M19 19a2 2 0 100 .01M12 6v6M12 12l-6 5M12 12l6 5',
  switchyard: 'M4 6h6l4 6h6M4 18h6l4-6M17 4l3 2-3 2M17 16l3 2-3 2',
  relay: 'M3 12h4l2-6 4 12 2-6h6',
  dynamo: 'M13 2L4 14h7l-1 8 9-12h-7z',
  skills: 'M9 6h11M9 12h11M9 18h11M3.5 6l1.5 1.5L7.5 4.5M3.5 12l1.5 1.5L7.5 10.5M3.5 18l1.5 1.5L7.5 16.5',
  audit: 'M6 3h9l4 4v14H6zM14 3v5h5M9 13h7M9 17h7',
  settings: 'M12 15a3 3 0 100-6 3 3 0 000 6zM19 12l2-1-2-4-2 1-2-1-1-2H9L8 7 6 8 4 7 2 11l2 1v0l-2 1 2 4 2-1 2 1 1 2h4l1-2 2-1 2 1 2-4z',
  sun: 'M12 8a4 4 0 100 8 4 4 0 000-8zM12 2v2M12 20v2M2 12h2M20 12h2M5 5l1.5 1.5M17.5 17.5L19 19M5 19l1.5-1.5M17.5 6.5L19 5',
  moon: 'M20 14A8 8 0 1110 4a7 7 0 0010 10z',
  key: 'M14 10a4 4 0 11-3 4l-8 0v-3h2v-2h2l2-2M17 8h.01',
  close: 'M6 6l12 12M18 6L6 18',
  play: 'M7 4l13 8-13 8z', stop: 'M6 6h12v12H6z',
  restart: 'M4 12a8 8 0 108-8M4 4v6h6',
  trash: 'M4 7h16M9 7V4h6v3M6 7l1 13h10l1-13',
  copy: 'M9 9h11v11H9zM5 15V4h11',
  send: 'M4 12l16-8-6 16-3-7z',
  external: 'M14 4h6v6M20 4l-9 9M18 14v6H4V6h6',
  warn: 'M12 3l10 18H2zM12 10v5M12 18h.01',
  check: 'M4 12l5 5 11-11',
  plus: 'M12 5v14M5 12h14',
  lock: 'M6 11h12v9H6zM8 11V8a4 4 0 018 0v3',
  shield: 'M12 3l8 3v6c0 5-3.5 8-8 9-4.5-1-8-4-8-9V6z',
  history: 'M4 12a8 8 0 108-8M4 4v5h5M12 8v5l3 2',
};
export function icon(name, size = 18) {
  const ns = 'http://www.w3.org/2000/svg';
  const svg = document.createElementNS(ns, 'svg');
  svg.setAttribute('viewBox', '0 0 24 24'); svg.setAttribute('width', size); svg.setAttribute('height', size);
  svg.setAttribute('fill', 'none'); svg.setAttribute('stroke', 'currentColor'); svg.setAttribute('stroke-width', '1.8');
  svg.setAttribute('stroke-linecap', 'round'); svg.setAttribute('stroke-linejoin', 'round');
  svg.setAttribute('aria-hidden', 'true'); svg.classList.add('icon');
  const p = document.createElementNS(ns, 'path'); p.setAttribute('d', ICONS[name] || ICONS.overview);
  svg.append(p);
  return svg;
}
// Only for markup produced by markdown.js / charts.js (both escape all dynamic text).
export function setTrustedHtml(el, html) { el.innerHTML = html; return el; }

export function download(filename, text, type = 'text/plain') {
  const url = URL.createObjectURL(new Blob([text], { type }));
  const a = h('a', { href: url, download: filename });
  document.body.append(a); a.click(); a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}
export async function copyText(text) {
  try { await navigator.clipboard.writeText(text); return true; } catch {
    const ta = h('textarea', { value: text, class: 'sr-only' });
    document.body.append(ta); ta.select();
    let ok = false; try { ok = document.execCommand('copy'); } catch { /* ignore */ }
    ta.remove(); return ok;
  }
}
