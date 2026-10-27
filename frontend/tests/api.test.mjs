import test from 'node:test';
import assert from 'node:assert/strict';
import { createClient, ApiError, buildQuery, CSRF_HEADER } from '../js/api.js';
import { setMessages } from '../js/i18n.js';
import { describeError } from '../js/errors.js';
import fs from 'node:fs';

const en = JSON.parse(fs.readFileSync(new URL('../i18n/en.json', import.meta.url), 'utf8'));
setMessages(en);
const res = (status, body, ct = 'application/json') => ({ ok: status < 400, status, headers: { get: () => ct }, json: async () => body, text: async () => String(body) });

test('every non-GET sends X-Console: 1; GET does not', async () => {
  const calls = [];
  const api = createClient({ fetchImpl: async (u, o) => { calls.push({ u, o }); return res(200, {}); } });
  await api.health(); await api.validate('switchyard', 'routes', 'x'); await api.apply('switchyard', 'routes', { content: 'x', base_sha256: 'a', restart: true }); await api.rollback('relay', 'routes', 'rev_1');
  await api.applyParams('dynamo', {}); await api.runAction('dynamo', 'up', {}, true); await api.saveSettings({}); await api.previewParams('dynamo', {});
  assert.equal(CSRF_HEADER, 'X-Console');
  assert.equal(calls[0].o.headers['X-Console'], undefined);
  for (const c of calls.slice(1)) assert.equal(c.o.headers['X-Console'], '1', `${c.o.method} ${c.u}`);
  assert.deepEqual(calls.slice(1).map((c) => c.o.method), ['POST', 'POST', 'POST', 'PUT', 'POST', 'PUT', 'POST']);
});
test('apply and rollback always send confirm:true; the body carries base_sha256', async () => {
  let body; const api = createClient({ fetchImpl: async (u, o) => { body = JSON.parse(o.body); return res(200, {}); } });
  await api.apply('switchyard', 'routes', { content: 'c', base_sha256: 'abc', restart: false }); assert.deepEqual(body, { content: 'c', base_sha256: 'abc', restart: false, confirm: true });
  await api.rollback('switchyard', 'routes', 'rev_2'); assert.deepEqual(body, { revision_id: 'rev_2', confirm: true });
  await api.applyParams('dynamo', { a: 1 }); assert.deepEqual(body, { values: { a: 1 }, confirm: true });
});
test('disruptive actions send confirm:true, safe ones confirm:false; paths are URL-encoded', async () => {
  const seen = []; const api = createClient({ fetchImpl: async (u, o) => { seen.push([u, JSON.parse(o.body)]); return res(200, {}); } });
  await api.runAction('dynamo', 'preflight', {}, false); await api.runAction('dynamo', 'up', { stop_sharing_engine: true }, true);
  assert.deepEqual(seen, [['/api/v1/systems/dynamo/actions/preflight', { params: {}, confirm: false }], ['/api/v1/systems/dynamo/actions/up', { params: { stop_sharing_engine: true }, confirm: true }]]);
  const urls = []; const a2 = createClient({ fetchImpl: async (u) => { urls.push(u); return res(200, {}); } }); await a2.config('x/../y', 'a b');
  assert.equal(urls[0], '/api/v1/systems/x%2F..%2Fy/config/a%20b');
});
test('errors: {error:{code,message}} becomes ApiError; 401 triggers onUnauthorized; network failure is status 0', async () => {
  let unauth = 0;
  const api = createClient({ onUnauthorized: () => unauth++, fetchImpl: async () => res(401, { error: { code: 'unauthorized', message: 'bearer token required' } }) });
  await assert.rejects(api.overview(), (e) => e instanceof ApiError && e.status === 401 && e.code === 'unauthorized' && e.message === 'bearer token required'); assert.equal(unauth, 1);
  const off = createClient({ fetchImpl: async () => { throw new TypeError('fetch failed'); } });
  await assert.rejects(off.health(), (e) => e.status === 0 && e.code === 'network');
});
test('the bearer comes only from getKey and is sent as Authorization', async () => {
  let h; const api = createClient({ getKey: () => 'tok', fetchImpl: async (u, o) => { h = o.headers; return res(200, {}); } }); await api.health(); assert.equal(h.Authorization, 'Bearer tok');
  const none = createClient({ getKey: () => '', fetchImpl: async (u, o) => { h = o.headers; return res(200, {}); } }); await none.health(); assert.equal(h.Authorization, undefined);
});
test('buildQuery drops empty values', () => assert.equal(buildQuery({ a: 1, b: '', c: null, d: 'x y' }), '?a=1&d=x+y'));

test('describeError maps 401/403/409/413/415/421/429/503 (+502, network) to distinct actionable messages', () => {
  const msgs = new Set();
  for (const status of [401, 403, 409, 413, 415, 421, 429, 502, 503]) {
    const m = describeError({ status, message: `HTTP ${status}` }); msgs.add(m);
    assert.ok(m.length > 30, String(status));
  }
  assert.equal(msgs.size, 9);
  assert.match(describeError({ status: 401 }), /bearer token/i); assert.match(describeError({ status: 409 }), /reload/i); assert.match(describeError({ status: 421 }), /127\.0\.0\.1/); assert.match(describeError({ status: 413 }), /1 MiB/);
  assert.match(describeError({ status: 0, code: 'network' }), /Cannot reach/); assert.equal(describeError({ status: 500, message: 'boom' }), 'boom'); assert.equal(describeError(null), 'Something went wrong');
  assert.match(describeError({ status: 429, message: 'at most 8 concurrent event streams' }), /at most 8/);
});

test('after a 401 nothing is sent with the same key until the hold is cleared; /health stays open', async () => {
  let key = null; let unauth = 0; const sent = [];
  const api = createClient({ getKey: () => key, onUnauthorized: () => unauth++,
    fetchImpl: async (u) => { sent.push(u); return key === 'good' ? res(200, {}) : res(401, { error: { code: 'unauthorized', message: 'no' } }); } });
  await assert.rejects(api.overview(), (e) => e.status === 401);
  await assert.rejects(api.overview(), (e) => e.status === 401 && e.message === 'sign in to continue');
  await assert.rejects(api.status('relay'), (e) => e.status === 401);
  assert.equal(sent.length, 1, 'held requests never reach the server');
  assert.equal(unauth, 3, 'every held call still asks for a sign-in');
  await api.health().catch(() => {});
  assert.equal(sent.length, 2, '/health is not held');
  key = 'wrong';
  await assert.rejects(api.overview(), (e) => e.status === 401);
  assert.equal(sent.length, 3, 'a different key is tried');
  await assert.rejects(api.overview(), (e) => e.status === 401);
  assert.equal(sent.length, 3, 'and held once it is refused too');
  key = 'good'; await api.overview();
  assert.equal(sent.length, 4);
  key = 'wrong'; await assert.rejects(api.overview()); assert.equal(sent.length, 5);
  api.clearAuthHold(); await assert.rejects(api.overview());
  assert.equal(sent.length, 6, 'clearAuthHold lets the same key be tried again (signing in again with it)');
});
