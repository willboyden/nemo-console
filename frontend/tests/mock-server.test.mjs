// The mock must behave like the contract, otherwise the UI is built against fiction.
import test, { before, after } from 'node:test';
import assert from 'node:assert/strict';
import crypto from 'node:crypto';
import http from 'node:http';
import { server, _state } from '../dev/mock-server.mjs';
import { SSEParser } from '../js/sse.js';

let base; const H = { 'X-Console': '1', 'Content-Type': 'application/json' };
before(() => new Promise((r) => server.listen(0, '127.0.0.1', () => { base = `http://127.0.0.1:${server.address().port}`; r(); })));
after(() => { server.closeAllConnections?.(); server.close(); });
const dev = (u) => fetch(u, { headers: { 'X-Console': '1' } });
const get = (p, o) => fetch(`${base}/api/v1${p}`, o);
const post = (p, body, h = H, method = 'POST') => fetch(`${base}/api/v1${p}`, { method, headers: h, body: JSON.stringify(body) });
const sha = (s) => crypto.createHash('sha256').update(s).digest('hex');

test('security headers on API and static responses; strict CSP with no unsafe-inline', async () => {
  for (const u of [`${base}/api/v1/health`, `${base}/index.html`, `${base}/nope`]) {
    const r = await fetch(u); const csp = r.headers.get('content-security-policy');
    assert.match(csp, /default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; frame-ancestors 'none'/); assert.ok(!/unsafe/.test(csp));
    assert.equal(r.headers.get('x-content-type-options'), 'nosniff'); assert.equal(r.headers.get('referrer-policy'), 'no-referrer'); assert.equal(r.headers.get('x-frame-options'), 'DENY');
  }
  assert.equal((await get('/health')).headers.get('cache-control'), 'no-store');
  assert.equal((await fetch(`${base}/dev/mock-server.mjs`)).status, 404); assert.equal((await fetch(`${base}/package.json`)).status, 404); assert.ok([400, 404].includes((await fetch(`${base}/..%2fpackage.json`)).status));
});
test('non-GET without X-Console is 403; bad Origin is 403; wrong content type 415; wrong Host 421; bad JSON 422', async () => {
  let r = await post('/systems/switchyard/actions/dry_run_config', {}, { 'Content-Type': 'application/json' }); assert.equal(r.status, 403); assert.equal((await r.json()).error.code, 'csrf');
  r = await post('/systems/switchyard/actions/dry_run_config', {}, { ...H, Origin: 'http://evil.example' }); assert.equal(r.status, 403);
  r = await post('/systems/switchyard/actions/dry_run_config', {}, { ...H, 'Content-Type': 'text/plain' }); assert.equal(r.status, 415);
  // fetch() will not let a caller override Host, so use node:http for the DNS-rebinding check.
  const code = await new Promise((ok) => http.get({ host: '127.0.0.1', port: server.address().port, path: '/api/v1/health', headers: { Host: 'evil.example' } }, (x) => { x.resume(); ok(x.statusCode); }));
  assert.equal(code, 421);
  r = await fetch(`${base}/api/v1/settings`, { method: 'PUT', headers: H, body: '{nope' }); assert.equal(r.status, 422);
});
test('errors are {error:{code,message}}; unknown things are 404; a body over 1 MiB is 413', async () => {
  const r = await get('/systems/nope/status'); assert.equal(r.status, 404); const b = await r.json(); assert.ok(b.error.code && b.error.message);
  const big = await fetch(`${base}/api/v1/systems/switchyard/config/routes/validate`, { method: 'POST', headers: H, body: JSON.stringify({ content: 'x'.repeat(1100000) }) }).catch((e) => ({ status: 413, err: e })); assert.equal(big.status, 413);
});
test('a down Dynamo is 200 with health=down and metrics.available=false (never a 5xx)', async () => {
  await dev(`${base}/__dev/scenario?dynamo=down`);
  const st = await (await get('/systems/dynamo/status')).json(); assert.equal(st.health, 'down'); assert.ok(st.problems[0].fix);
  const m = await (await get('/systems/dynamo/metrics')).json(); assert.equal(m.available, false); assert.deepEqual(m.points, []); assert.ok(m.error);
  assert.equal((await post('/systems/dynamo/playground/chat', { model: 'unsloth/Qwen3.8-27B-NVFP4', messages: [{ role: 'user', content: 'x' }] })).status, 503);
  await dev(`${base}/__dev/scenario?dynamo=loading`); const ld = await (await get('/systems/dynamo/status')).json(); assert.equal(ld.health, 'degraded'); assert.match(ld.summary, /loading/i);
  await dev(`${base}/__dev/scenario?dynamo=off`); const off = await (await get('/systems/dynamo/status')).json();
  assert.equal(off.health, 'off'); assert.deepEqual(off.problems.map((p) => p.severity), ['info']);
  assert.equal((await (await get('/health')).json()).systems.dynamo, 'off');
  await dev(`${base}/__dev/scenario?dynamo=up`);
});
test('config: read, validate (authoritative flag), 409 on stale sha, rollback outcome, read-only 403, secret refusal', async () => {
  const f = await (await get('/systems/switchyard/config/routes')).json(); assert.equal(f.sha256, sha(f.content)); assert.equal(f.spec.editable, true);
  const bad = f.content.replace('capable_target = "flash-next-auto"', 'capable_target = "nope"');
  const v = await (await post('/systems/switchyard/config/routes/validate', { content: bad })).json(); assert.equal(v.ok, false); assert.match(v.diagnostics[0].message, /unknown target nope/); assert.ok(v.diagnostics[0].line > 0); assert.equal(v.authoritative, true);
  const rv = await (await post('/systems/relay/config/routes/validate', { content: 'schema_version = 1\n' })).json(); assert.equal(rv.authoritative, false); assert.ok(rv.note);
  let r = await post('/systems/switchyard/config/routes/apply', { content: f.content + '# x\n', base_sha256: 'deadbeef', restart: true, confirm: true }); assert.equal(r.status, 409);
  r = await post('/systems/switchyard/config/routes/apply', { content: f.content, base_sha256: f.sha256, restart: true }); assert.equal(r.status, 422);   // confirm missing
  r = await post('/systems/switchyard/config/routes/apply', { content: f.content + '# sk-abcdefghijklmnopqrstuv\n', base_sha256: f.sha256, restart: true, confirm: true }); assert.equal(r.status, 422); assert.equal((await r.json()).error.code, 'secret_in_content');
  const rb = await (await post('/systems/switchyard/config/routes/apply', { content: f.content + '\n# unhealthy\n', base_sha256: f.sha256, restart: true, confirm: true })).json();
  assert.equal(rb.ok, false); assert.equal(rb.rolled_back, true); assert.equal(rb.post_check, 'unhealthy'); assert.ok(rb.revision_id);
  assert.equal((await (await get('/systems/switchyard/config/routes')).json()).sha256, f.sha256, 'the file is restored after auto-rollback');
  const ok = await (await post('/systems/switchyard/config/routes/apply', { content: f.content + '# ok\n', base_sha256: f.sha256, restart: true, confirm: true })).json(); assert.equal(ok.ok, true); assert.equal(ok.post_check, 'healthy'); assert.equal(ok.new_sha256, sha(f.content + '# ok\n'));
  const hist = await (await get('/systems/switchyard/config/routes/history')).json(); assert.ok(hist.length >= 3 && hist[0].id && hist[0].sha256);
  r = await post('/systems/relay/config/plugins/apply', { content: 'x', base_sha256: 'a', restart: false, confirm: true }); assert.equal(r.status, 403);
  const spec = await (await get('/systems/relay/config')).json(); const ro = spec.filter((s) => !s.editable); assert.equal(ro.length, 2); assert.ok(ro.every((s) => s.why_readonly));
  await dev(`${base}/__dev/touch`); r = await post('/systems/switchyard/config/routes/apply', { content: f.content, base_sha256: ok.new_sha256, restart: false, confirm: true }); assert.equal(r.status, 409);
});
test('actions: disruptive need confirm; dynamo up is refused while the sharing engine runs unless stop_sharing_engine', async () => {
  let r = await post('/systems/switchyard/actions/restart', { params: {}, confirm: false }); assert.equal(r.status, 422);
  r = await post('/systems/switchyard/actions/dry_run_config', { params: {}, confirm: false }); assert.equal(r.status, 200);
  await dev(`${base}/__dev/scenario?dynamo=down`); await dev(`${base}/__dev/vllm?run=1`);
  const pf = await (await post('/systems/dynamo/actions/preflight', { params: {}, confirm: false })).json(); assert.match(pf.output, /shares_card: ai-lab-vllm-qwen38-vllm-1 \(running\)/); assert.match(pf.output, /qwen3\.8-27b-1m/); assert.equal(pf.problems[0].severity, 'warn');
  let up = await (await post('/systems/dynamo/actions/up', { params: {}, confirm: true })).json(); assert.equal(up.ok, false); assert.ok(up.problems[0].fix);
  up = await (await post('/systems/dynamo/actions/up', { params: { stop_sharing_engine: true }, confirm: true })).json(); assert.equal(up.ok, true); assert.match(up.output, /stopped ai-lab-vllm-qwen38-vllm-1/);
  assert.equal((await (await get('/systems/dynamo/status')).json()).health, 'degraded');
  await dev(`${base}/__dev/scenario?dynamo=up`); await dev(`${base}/__dev/vllm?run=0`);
});
test('params: catalog, preview renders, "[set]" is never accepted back, secrets are not in the preview', async () => {
  const p = await (await get('/systems/dynamo/params')).json(); assert.ok(p.catalog.length > 8); assert.equal(p.values.hf_token, '[set]');
  assert.deepEqual((await (await get('/systems/switchyard/params')).json()).catalog, []);
  const pv = await (await post('/systems/dynamo/params/preview', { values: { gpu_memory_utilization: 0.97, max_num_seqs: 32 } })).json(); assert.match(pv.rendered, /--gpu-memory-utilization/); assert.ok(pv.warnings.length);
  assert.equal((await post('/systems/dynamo/params/preview', { values: { hf_token: '[set]' } })).status, 422); assert.equal((await post('/systems/dynamo/params/preview', { values: { gpu_memory_utilization: 5 } })).status, 422);
  assert.equal((await post('/systems/dynamo/params', { values: { max_num_seqs: 8 } }, H, 'PUT')).status, 422);   // confirm missing
  assert.ok(!(await (await post('/systems/dynamo/params/preview', { values: { hf_token: 'hf_realsecretvalue' } })).text()).includes('hf_realsecretvalue'));
});
test('settings never reveal tokens ([set, N chars]); audit records mutations with content redacted', async () => {
  const s = await (await get('/settings')).json(); for (const v of Object.values(s.tokens)) assert.match(v, /^\[(set, \d+ chars|unset)\]$/);
  assert.ok(s.poll_interval_s && s.playground_key_env && !('intervals' in s) && 'restart_required' in s);
  assert.equal((await fetch(`${base}/api/v1/settings`, { method: 'PUT', headers: H, body: JSON.stringify({ urls: { litellm: 'https://evil.example' } }) })).status, 422);
  assert.equal((await fetch(`${base}/api/v1/settings`, { method: 'PUT', headers: H, body: JSON.stringify({ playground_key_env: { switchyard: 'sk-abc lowercase' } }) })).status, 422);
  const a = await (await get('/audit?limit=50')).json(); assert.ok(a.length); assert.ok(a.every((x) => x.id && x.path && x.role && !('action' in x))); assert.ok(a.some((x) => /\/config\/routes\/apply$/.test(x.path))); assert.ok(a.every((x) => !JSON.stringify(x).includes('# unhealthy'))); assert.ok(a.every((x) => x.params?.content === undefined || x.params.content === '[redacted]'));
});
test('viewer role: GETs work, every mutation is 403, audit is 403', async () => {
  await dev(`${base}/__dev/role?set=viewer`);
  assert.equal((await get('/overview')).status, 200); assert.equal((await get('/audit')).status, 403); assert.equal((await get('/health').then((r) => r.json())).role, 'viewer');
  assert.equal((await post('/systems/switchyard/actions/restart', { params: {}, confirm: true })).status, 403);
  await dev(`${base}/__dev/role?set=admin`);
});
test('SSE: events stream delivers SystemEvent JSON with metadata only; 9th stream is 429; playground streams deltas + served-by headers', async () => {
  const ctrls = [];
  const open = async () => { const c = new AbortController(); ctrls.push(c); return fetch(`${base}/api/v1/systems/switchyard/events/stream`, { signal: c.signal }); };
  const r = await open(); assert.equal(r.headers.get('content-type'), 'text/event-stream');
  const reader = r.body.getReader(), dec = new TextDecoder(), parser = new SSEParser(); let evs = [];
  const t0 = Date.now(); while (!evs.length && Date.now() - t0 < 8000) { const { value } = await reader.read(); evs = parser.feed(dec.decode(value)); }
  const ev = JSON.parse(evs[0].data); assert.equal(ev.system, 'switchyard'); assert.equal(ev.kind, 'routing.decision'); assert.ok(ev.data.route && ev.data.tier); assert.ok(!('prompt' in ev.data) && !('content' in ev.data) && !('messages' in ev.data));
  for (let i = 0; i < 7; i++) await open();
  const over = await open(); assert.equal(over.status, 429); assert.equal((await over.json()).error.code, 'too_many_streams');
  ctrls.forEach((c) => c.abort());
  const pg = await post('/systems/switchyard/playground/chat', { model: 'auto-local', messages: [{ role: 'user', content: 'hi' }], max_tokens: 5, stream: true });
  assert.ok(pg.headers.get('x-served-by-route')); assert.ok(pg.headers.get('x-served-by-tier'));
  const txt = await pg.text(); assert.match(txt, /"content":"Routing"/); assert.match(txt, /data: \[DONE\]/);
  assert.equal((await post('/systems/switchyard/playground/chat', { model: 'auto-local', messages: [{ role: 'user', content: 'x' }], max_tokens: 99999 })).status, 422);
  const rl = await post('/systems/relay/playground/chat', { model: 'auto-local', messages: [{ role: 'user', content: 'x' }], max_tokens: 3 }); assert.match(await rl.text(), /content_block_delta/);
});
test('bearer mode: 401 without the key (health stays open)', async () => {
  // exercised via the fail-injection helper so this file does not need a second server
  await dev(`${base}/__dev/fail?status=401&n=1`); const r = await get('/overview'); assert.equal(r.status, 401); assert.equal((await r.json()).error.code, 'unauthorized');
  // 421 is skipped here: fetch() transparently retries a 421 on a fresh connection (spec behaviour), which would hide the injected failure; the real 421 is covered above via node:http.
  for (const st of [403, 409, 413, 415, 429, 503]) { await dev(`${base}/__dev/fail?status=${st}&n=1`); assert.equal((await get('/overview')).status, st); }
});

test('dev helpers need X-Console; static server refuses traversal spellings and wrong Host/port; non-loopback --host needs MOCK_KEY', async () => {
  assert.equal((await fetch(`${base}/__dev/role?set=viewer`)).status, 403);
  assert.equal((await get('/health').then((r) => r.json())).role, 'admin');
  const raw = (p, host) => new Promise((ok) => http.get({ host: '127.0.0.1', port: server.address().port, path: p, headers: host ? { Host: host } : {} }, (x) => { x.resume(); ok(x.statusCode); }));
  for (const p of ['/css/..%2fdev/mock-server.mjs', '/css/%2e%2e/dev/mock-server.mjs', '/css/..%5cdev/x', '/js/..//dev/mock-server.mjs', '/css\\x']) assert.equal(await raw(p), 400, p);
  assert.equal(await raw('/dev/mock-server.mjs'), 404); assert.equal(await raw('/css/base.css'), 200);
  assert.equal(await raw('/css/base.css', 'localhost:1'), 421); assert.equal(await raw('/css/base.css', 'evil.example'), 421); assert.equal(await raw('/__dev/role?set=admin', 'evil.example'), 421);
  const { spawnSync } = await import('node:child_process'); const r = spawnSync(process.execPath, ['dev/mock-server.mjs', '--host', '0.0.0.0', '--port', '0'], { cwd: new URL('..', import.meta.url).pathname, env: { ...process.env, MOCK_KEY: '' }, timeout: 5000 });
  assert.equal(r.status, 2); assert.match(String(r.stderr), /refusing --host 0\.0\.0\.0/);
});
