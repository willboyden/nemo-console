import test from 'node:test';
import assert from 'node:assert/strict';
import { SSEParser, backoff, openStream, parseErrorBody } from '../js/sse.js';

test('parses basic events, event names, ids, multi-line data, comments', () => {
  const p = new SSEParser();
  const ev = p.feed(': hi\nid: 7\nevent: log\ndata: a\ndata: b\n\ndata: {"x":1}\n\n');
  assert.deepEqual(ev, [{ event: 'log', data: 'a\nb', id: '7' }, { event: 'message', data: '{"x":1}', id: '7' }]);
});

test('handles chunk boundaries anywhere, including inside a line and CRLF', () => {
  const src = 'data: hello\r\n\r\nevent: x\r\ndata: world\r\n\r\n';
  for (let cut = 1; cut < src.length; cut++) {
    const p = new SSEParser();
    const out = [...p.feed(src.slice(0, cut)), ...p.feed(src.slice(cut))];
    assert.deepEqual(out.map((e) => [e.event, e.data]), [['message', 'hello'], ['x', 'world']], `cut at ${cut}`);
  }
});

test('strips exactly one leading space; keeps empty data lines; ignores events without data', () => {
  const p = new SSEParser();
  assert.deepEqual(p.feed('data:  two\n\n').map((e) => e.data), [' two']);
  assert.deepEqual(p.feed('event: ping\n\n'), []);
  assert.deepEqual(p.feed('data\n\n').map((e) => e.data), ['']);
});

test('backoff is exponential and capped', () => {
  assert.deepEqual([0, 1, 2, 3].map((a) => backoff(a)), [500, 1000, 2000, 4000]);
  assert.equal(backoff(50), 15000);
});

const streamRes = (chunks, status = 200) => ({ ok: status < 400, status, body: { getReader() { let i = 0; return { read: async () => (i < chunks.length ? { value: new TextEncoder().encode(chunks[i++]), done: false } : { done: true }), cancel: async () => {} }; } } });

test('openStream emits parsed JSON events and stops on [DONE]', async () => {
  const got = [];
  await new Promise((resolve) => openStream('/x', { fetchImpl: async () => streamRes(['data: {"a":1}\n\nda', 'ta: [DONE]\n\n']), onEvent: (e) => got.push(e), onClose: resolve }));
  assert.deepEqual(got.map((e) => e.event), ['message', 'done']);
  assert.deepEqual(got[0].data, { a: 1 });
});

test('openStream does not retry on 4xx and reports the status', async () => {
  let calls = 0; const errs = [];
  await new Promise((resolve) => openStream('/x', { fetchImpl: async () => { calls++; return streamRes([], 401); }, onError: (e) => errs.push(e.status), onClose: resolve }));
  assert.equal(calls, 1); assert.deepEqual(errs, [401]);
});

test('openStream reconnects on 5xx / network error with Last-Event-ID, then close() stops it', async () => {
  const seen = []; let n = 0; let handle;
  await new Promise((resolve) => {
    handle = openStream('/x', { sleep: async () => {}, onClose: resolve,
      fetchImpl: async (u, o) => { seen.push(o.headers['Last-Event-ID']); n++; if (n === 1) return streamRes(['id: 5\ndata: 1\n\n']); if (n === 2) throw new Error('boom'); handle.close(); return streamRes([]); },
      onEvent: () => {} });
  });
  assert.deepEqual(seen, [undefined, '5', '5']);
});

test('reconnect:false ends after the stream closes', async () => {
  let calls = 0;
  await new Promise((resolve) => openStream('/x', { reconnect: false, fetchImpl: async () => { calls++; return streamRes(['data: 1\n\n']); }, onClose: resolve }));
  assert.equal(calls, 1);
});

test('openStream surfaces the problem+json detail on 4xx (and sends POST body/headers as given)', async () => {
  let call; const errs = [];
  await new Promise((resolve) => openStream('/chat', { method: 'POST', body: '{"a":1}', headers: { 'X-Engine-Console': '1' }, reconnect: false,
    fetchImpl: async (u, o) => { call = o; return { ok: false, status: 409, text: async () => '{"detail":"instance q is loading","code":"instance_not_ready"}' }; },
    onError: (e) => errs.push(e), onClose: resolve }));
  assert.equal(call.method, 'POST'); assert.equal(call.headers['X-Engine-Console'], '1');
  assert.equal(errs[0].message, 'instance q is loading'); assert.equal(errs[0].code, 'instance_not_ready'); assert.equal(errs[0].status, 409);
});

test('openStream calls onResponse with the response so headers (X-Served-By-Route) are readable', async () => {
  const enc = new TextEncoder();
  const body = new ReadableStream({ start(c) { c.enqueue(enc.encode('data: {"a":1}\n\ndata: [DONE]\n\n')); c.close(); } });
  const headers = new Headers({ 'X-Served-By-Route': 'auto-local' });
  let seen = null; const events = [];
  await new Promise((resolve) => openStream('/x', { method: 'POST', reconnect: false, fetchImpl: async () => ({ ok: true, status: 200, headers, body }), onResponse: (r) => { seen = r.headers.get('X-Served-By-Route'); }, onEvent: (e) => events.push(e), onClose: resolve }));
  assert.equal(seen, 'auto-local'); assert.equal(events[0].data.a, 1); assert.equal(events.at(-1).event, 'done');
});

test('parseErrorBody reads {error:{code,message}} first, then RFC 7807 keys, then a bare status', () => {
  assert.deepEqual(parseErrorBody('{"error":{"code":"too_many_streams","message":"at most 8"}}', 429), { message: 'at most 8', code: 'too_many_streams' });
  assert.deepEqual(parseErrorBody('{"detail":"instance loading","code":"loading"}', 503), { message: 'instance loading', code: 'loading' });
  assert.deepEqual(parseErrorBody('{"title":"Nope"}', 403), { message: 'Nope', code: undefined });
  assert.deepEqual(parseErrorBody('<html>', 502), { message: 'stream 502', code: undefined });
  assert.deepEqual(parseErrorBody('', 500), { message: 'stream 500', code: undefined }); assert.deepEqual(parseErrorBody('null', 500).message, 'stream 500');
});
test('4xx stream failures (401/403/429) are final: one request, real message surfaced, no retry', async () => {
  for (const status of [401, 403, 429]) {
    let calls = 0, err = null;
    await new Promise((resolve) => openStream('/x', { sleep: async () => {}, fetchImpl: async () => { calls++; return { ok: false, status, text: async () => JSON.stringify({ error: { code: status === 429 ? 'too_many_streams' : 'x', message: `msg ${status}` } }) }; }, onError: (e) => { err = e; }, onClose: resolve }));
    assert.equal(calls, 1, `status ${status}`); assert.equal(err.message, `msg ${status}`); assert.equal(err.status, status);
  }
});
test('5xx is retried with backoff', async () => {
  let calls = 0;
  await new Promise((resolve) => { const h = openStream('/x', { sleep: async () => { if (calls >= 3) h.close(); }, fetchImpl: async () => { calls++; return { ok: false, status: 503, text: async () => '{}' }; }, onClose: resolve }); });
  assert.ok(calls >= 3);
});
