// SVG topology graph. Nodes are coloured by health AND carry a glyph + text; inactive edges are dashed.
// All text is set with textContent (labels come from the server).
import { h } from '../dom.js';
import { t } from '../i18n.js';
import { layoutTopology, describeTopology, wrapLabel } from '../topology-layout.js';
import { healthGlyph } from '../status-util.js';

const NS = 'http://www.w3.org/2000/svg';
function s(tag, attrs, ...kids) {
  const el = document.createElementNS(NS, tag);
  for (const [k, v] of Object.entries(attrs || {})) if (v != null) el.setAttribute(k, String(v));
  for (const k of kids.flat()) if (k != null) el.append(k instanceof Node ? k : document.createTextNode(String(k)));
  return el;
}
const ROUTE = { switchyard: '#/switchyard', relay: '#/relay', dynamo: '#/dynamo' };

export function topologyGraph(topology) {
  const lay = layoutTopology(topology.nodes, topology.edges);
  const defs = s('defs', null, s('marker', { id: 'arrow', viewBox: '0 0 10 10', refX: 9, refY: 5, markerWidth: 7, markerHeight: 7, orient: 'auto-start-reverse' }, s('path', { d: 'M0 0L10 5L0 10z', class: 'arrowhead' })));
  const lines = lay.edges.map((e) => s('path', { d: e.path, class: `edge-line ${e.active === false ? 'inactive' : 'active'}`, 'marker-end': 'url(#arrow)' }));
  // Labels are drawn after ALL lines, on a background chip, so no later edge line can cross them.
  const labels = lay.edges.filter((e) => e.label).flatMap((e) => [s('rect', { x: e.lx - e.label.length * 2.7 - 4, y: e.ly - 9, width: e.label.length * 5.4 + 8, height: 13, rx: 3, class: 'edge-chip' }), s('text', { x: e.lx, y: e.ly + 1, class: 'edge-label', 'text-anchor': 'middle' }, e.label)]);
  const edges = [...lines, ...labels];
  const nodes = lay.nodes.map((n) => {
    const hl = n.health || 'unknown';
    const inner = [
      s('rect', { x: n.x, y: n.y, width: n.w, height: n.h, rx: 8, class: 'node-box' }),
      s('title', null, n.label),
      ...(() => { const ls = wrapLabel(n.label); return [...ls.map((l, i) => s('text', { x: n.x + n.w / 2, y: n.y + 17 + i * 13, 'text-anchor': 'middle', class: 'node-label' }, l)),
        s('text', { x: n.x + n.w / 2, y: n.y + 17 + ls.length * 13 + 3, 'text-anchor': 'middle', class: 'node-health' }, `${healthGlyph(hl)} ${t(`health.${hl}`)}`)]; })(),
    ];
    const g = s('g', { class: `node ${n.kind} ${hl}${n.system ? ' managed' : ''}` }, ...inner);
    if (n.system && ROUTE[n.system]) { const a = s('a', { href: ROUTE[n.system], 'aria-label': t('topo.node_link', { name: n.label, health: t(`health.${hl}`) }) }); a.append(g); return a; }
    g.setAttribute('role', 'img'); g.setAttribute('aria-label', `${n.label}: ${t(`health.${hl}`)}`);
    return g;
  });
  const svg = s('svg', { class: 'topology', viewBox: `0 0 ${lay.width} ${lay.height}`, role: 'group', 'aria-label': t('topo.aria'), width: lay.width, height: lay.height }, defs, edges, nodes);
  svg.style.maxWidth = `${Math.round(lay.width * 1.15)}px`;   // never scale the labels up beyond ~1.15x
  const textAlt = h('details', { class: 'chart-data' }, h('summary', t('topo.text_alt')),
    h('ul', { class: 'topo-alt' }, [...topology.nodes.map((n) => h('li', `${n.label}: ${t(`health.${n.health || 'unknown'}`)}`)), ...describeTopology(topology.nodes, topology.edges, { active: t('topo.active'), inactive: t('topo.inactive') }).map((x) => h('li', x))]));
  return h('figure', { class: 'topo-fig' }, h('div', { class: 'topo-scroll' }, svg), h('figcaption', { class: 'small muted' }, t('topo.legend')), textAlt);
}
