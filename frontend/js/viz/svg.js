// SVG element builder (the SVG twin of dom.js h()). Attributes are set with setAttribute; text goes in as text nodes,
// never markup; style attributes are refused (CSP): use classes and presentation attributes.
const NS = 'http://www.w3.org/2000/svg';
export function svg(tag, attrs, ...children) {
  const el = document.createElementNS(NS, tag);
  if (attrs && (typeof attrs !== 'object' || attrs instanceof Node || Array.isArray(attrs))) {
    children.unshift(attrs);
    attrs = null;
  }
  for (const [k, v] of Object.entries(attrs || {})) {
    if (v == null || v === false) continue;
    if (k === 'style') throw new Error('inline style attributes are forbidden by the CSP; use a CSS class');
    if (k === 'class') el.setAttribute('class', v);
    else if (k.startsWith('on') && typeof v === 'function') el.addEventListener(k.slice(2).toLowerCase(), v);
    else el.setAttribute(k, v === true ? '' : String(v));
  }
  for (const c of children.flat(Infinity)) {
    if (c == null || c === false) continue;
    el.append(c instanceof Node ? c : document.createTextNode(String(c)));
  }
  return el;
}
/** A <title> child: native tooltip and accessible name for a mark. */
export const titled = (el, text) => {
  el.append(svg('title', text));
  return el;
};
export const num = (v) => String(Math.round(v * 100) / 100);
