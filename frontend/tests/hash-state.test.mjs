// URL-hash state of a tab (js/charts/hash-state.js): deep links are read, clicks start clean, writes keep `tab`.
import test from 'node:test';
import assert from 'node:assert/strict';
import { createHashState, cleanQuery, hashFor } from '../js/charts/hash-state.js';

const fake = (hash = '') => {
  const calls = [];
  return {
    loc: { hash },
    hist: {
      replaceState: (_s, _t, url) => calls.push(['replace', url]),
      pushState: (_s, _t, url) => calls.push(['push', url]),
    },
    calls,
  };
};
const noDefer = () => {};

test('cleanQuery drops tab, empty and false values and stringifies the rest', () => {
  assert.deepEqual(cleanQuery({ tab: 'x', a: 1, b: '', c: null, d: false, e: undefined, f: 'ok' }), { a: '1', f: 'ok' });
  assert.deepEqual(cleanQuery(null), {});
  assert.equal(hashFor('dcgm', 'status', { window: '7d', max: '1', x: '' }), '#/dcgm?tab=status&window=7d&max=1');
});

test('a deep link is read on the first render and ignored after a click on the tab', () => {
  const f = fake('#/aiperf?tab=history&run=r1&vs=r2&f.model=glm');
  const a = createHashState({ path: 'aiperf', tab: 'history', initial: true, loc: f.loc, hist: f.hist, defer: noDefer });
  assert.deepEqual(a.all(), { run: 'r1', vs: 'r2', 'f.model': 'glm' });
  assert.equal(a.get('run'), 'r1');
  assert.equal(a.get('metric', 'ttft'), 'ttft');
  const b = createHashState({ path: 'aiperf', tab: 'history', initial: false, loc: f.loc, hist: f.hist, defer: noDefer });
  assert.deepEqual(b.all(), {});
});

test('set merges a patch, removes null / empty, writes the URL with the tab first, and tells the listeners', () => {
  const f = fake('#/dcgm?tab=status&window=24h');
  const s = createHashState({ path: 'dcgm', tab: 'status', loc: f.loc, hist: f.hist, defer: noDefer });
  const heard = [];
  const off = s.onChange((p) => heard.push(p));
  s.set({ max: '1' });
  s.set({ window: null });
  s.set({ 's.dcgm.gpu_util': 'gpu=0,model=x|gpu=1,model=x' }, { push: true });
  assert.deepEqual(f.calls, [
    ['replace', '#/dcgm?tab=status&window=24h&max=1'],
    ['replace', '#/dcgm?tab=status&max=1'],
    ['push', '#/dcgm?tab=status&max=1&s.dcgm.gpu_util=gpu%3D0%2Cmodel%3Dx%7Cgpu%3D1%2Cmodel%3Dx'],
  ]);
  assert.equal(heard.length, 3);
  off();
  s.set({ max: null });
  assert.equal(heard.length, 3, 'a removed listener hears nothing');
});

test('the written URL round-trips through the router parser (so a reload restores the state)', async () => {
  const { parseHash } = await import('../js/router.js');
  const f = fake('');
  const s = createHashState({ path: 'dcgm', tab: 'status', loc: f.loc, hist: f.hist, defer: noDefer });
  const pick = 'gpu=0,model=NVIDIA RTX|gpu=1,model=NVIDIA RTX';
  s.set({ 's.dcgm.gpu_util': pick, window: '7d' });
  const url = f.calls.at(-1)[1];
  const q = parseHash(url).query;
  assert.equal(q['s.dcgm.gpu_util'], pick);
  assert.equal(q.window, '7d');
  assert.equal(q.tab, 'status');
});

test('clear removes every key with a prefix in one write; keys limits what a deep link may bring', () => {
  const f = fake('#/gym?tab=history&f.model=a&f.verdict=failed&run=r');
  const s = createHashState({ path: 'gym', tab: 'history', loc: f.loc, hist: f.hist, defer: noDefer });
  s.clear('f.');
  assert.deepEqual(s.all(), { run: 'r' });
  assert.equal(f.calls.at(-1)[1], '#/gym?tab=history&run=r');
  const k = createHashState({ path: 'gym', tab: 'history', keys: ['run', /^f\./], loc: fake('#/gym?tab=history&run=r&evil=1&f.model=a').loc, hist: f.hist, defer: noDefer });
  assert.deepEqual(k.all(), { run: 'r', 'f.model': 'a' });
});

test('the real write is deferred (the page frame writes ?tab= first), and a history API that throws does not break the state', async () => {
  const f = fake('#/gym?tab=history&run=r');
  let deferred = null;
  const s = createHashState({ path: 'gym', tab: 'history', loc: f.loc, hist: f.hist, defer: (fn) => (deferred = fn) });
  assert.equal(f.calls.length, 0);
  deferred();
  assert.deepEqual(f.calls, [['replace', '#/gym?tab=history&run=r']]);
  const broken = createHashState({
    path: 'gym',
    tab: 'history',
    loc: { hash: '' },
    hist: {
      replaceState: () => {
        throw new Error('embedded');
      },
    },
    defer: noDefer,
  });
  broken.set({ run: 'x' });
  assert.equal(broken.get('run'), 'x');
  assert.equal(s.href({ vs: 'y' }), '#/gym?tab=history&run=r&vs=y');
});
