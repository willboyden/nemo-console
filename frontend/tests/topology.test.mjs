import test from 'node:test';
import assert from 'node:assert/strict';
import { computeRanks, layoutTopology, describeTopology, wrapLabel } from '../js/topology-layout.js';

const nodes = ['c1', 'c2', 'relay', 'sy', 'lite', 'e1', 'e2', 'dyn'].map((id) => ({ id, label: id.toUpperCase() }));
const edges = [['c1', 'relay'], ['c2', 'relay'], ['relay', 'sy'], ['sy', 'lite'], ['lite', 'e1'], ['lite', 'e2'], ['lite', 'dyn']].map(([src, dst], i) => ({ src, dst, active: i !== 6 }));
test('ranks follow the longest path from sources', () => {
  const r = computeRanks(nodes, edges); assert.deepEqual([r.get('c1'), r.get('relay'), r.get('sy'), r.get('lite'), r.get('e1'), r.get('dyn')], [0, 1, 2, 3, 4, 4]);
});
test('cycles and unknown ids terminate', () => {
  const r = computeRanks([{ id: 'a' }, { id: 'b' }], [{ src: 'a', dst: 'b' }, { src: 'b', dst: 'a' }, { src: 'a', dst: 'zzz' }, { src: 'a', dst: 'a' }]); assert.ok(r.get('a') >= 0 && r.get('b') >= 0);
});
test('layout: columns increase in x, nodes in a column never overlap, edges reference laid-out nodes, no NaN', () => {
  const l = layoutTopology(nodes, edges), by = Object.fromEntries(l.nodes.map((n) => [n.id, n]));
  assert.ok(by.c1.x < by.relay.x && by.relay.x < by.sy.x && by.sy.x < by.lite.x && by.lite.x < by.e1.x); assert.equal(by.e1.x, by.dyn.x);
  assert.ok(Math.abs(by.e1.y - by.e2.y) >= by.e1.h); assert.equal(l.edges.length, 7);
  assert.ok(l.nodes.every((n) => n.x >= 0 && n.y >= 0 && n.x + n.w <= l.width && n.y + n.h <= l.height)); assert.ok(!/NaN/.test(JSON.stringify(l)));
  assert.equal(l.edges.filter((e) => e.active === false).length, 1);
});
test('empty topology and text alternative', () => {
  assert.deepEqual(layoutTopology([], []).nodes, []);
  const d = describeTopology(nodes, edges, { active: 'active', inactive: 'inactive' }); assert.equal(d.length, 7); assert.match(d[6], /DYN: inactive/); assert.match(d[0], /^C1 -> RELAY: active/);
});

test('wrapLabel wraps to two lines and ellipsises long real-world labels', () => {
  assert.deepEqual(wrapLabel('LiteLLM router'), ['LiteLLM router']);
  assert.deepEqual(wrapLabel('Open WebUI, opencode, agents)'), ['Open WebUI,', 'opencode, agents)']);
  const w = wrapLabel('vLLM Qwen3.8 (shares the Workstation card with Dynamo)'); assert.equal(w.length, 2); assert.ok(w.every((l) => l.length <= 20)); assert.match(w[1], /…$/);
  assert.deepEqual(wrapLabel(''), []);
});
