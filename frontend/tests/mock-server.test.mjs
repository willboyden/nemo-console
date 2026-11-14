// The mock must behave like the contract, otherwise the UI is built against fiction.
import test, { before, after } from 'node:test';
import assert from 'node:assert/strict';
import crypto from 'node:crypto';
import http from 'node:http';
import { server, _state } from '../dev/mock-server.mjs';
import { SSEParser } from '../js/sse.js';

let base;
const H = { 'X-Console': '1', 'Content-Type': 'application/json' };
before(
  () =>
    new Promise((r) =>
      server.listen(0, '127.0.0.1', () => {
        base = `http://127.0.0.1:${server.address().port}`;
        r();
      }),
    ),
);
after(() => {
  server.closeAllConnections?.();
  server.close();
});
const dev = (u) => fetch(u, { headers: { 'X-Console': '1' } });
const get = (p, o) => fetch(`${base}/api/v1${p}`, o);
const post = (p, body, h = H, method = 'POST') => fetch(`${base}/api/v1${p}`, { method, headers: h, body: JSON.stringify(body) });
const sha = (s) => crypto.createHash('sha256').update(s).digest('hex');

test('security headers on API and static responses; strict CSP with no unsafe-inline', async () => {
  for (const u of [`${base}/api/v1/health`, `${base}/index.html`, `${base}/nope`]) {
    const r = await fetch(u);
    const csp = r.headers.get('content-security-policy');
    assert.match(csp, /default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; frame-ancestors 'none'/);
    assert.ok(!/unsafe/.test(csp));
    assert.equal(r.headers.get('x-content-type-options'), 'nosniff');
    assert.equal(r.headers.get('referrer-policy'), 'no-referrer');
    assert.equal(r.headers.get('x-frame-options'), 'DENY');
  }
  assert.equal((await get('/health')).headers.get('cache-control'), 'no-store');
  assert.equal((await fetch(`${base}/dev/mock-server.mjs`)).status, 404);
  assert.equal((await fetch(`${base}/package.json`)).status, 404);
  assert.ok([400, 404].includes((await fetch(`${base}/..%2fpackage.json`)).status));
});
test('non-GET without X-Console is 403; bad Origin is 403; wrong content type 415; wrong Host 421; bad JSON 422', async () => {
  let r = await post('/systems/switchyard/actions/dry_run_config', {}, { 'Content-Type': 'application/json' });
  assert.equal(r.status, 403);
  assert.equal((await r.json()).error.code, 'csrf');
  r = await post('/systems/switchyard/actions/dry_run_config', {}, { ...H, Origin: 'http://evil.example' });
  assert.equal(r.status, 403);
  r = await post('/systems/switchyard/actions/dry_run_config', {}, { ...H, 'Content-Type': 'text/plain' });
  assert.equal(r.status, 415);
  // fetch() will not let a caller override Host, so use node:http for the DNS-rebinding check.
  const code = await new Promise((ok) =>
    http.get({ host: '127.0.0.1', port: server.address().port, path: '/api/v1/health', headers: { Host: 'evil.example' } }, (x) => {
      x.resume();
      ok(x.statusCode);
    }),
  );
  assert.equal(code, 421);
  r = await fetch(`${base}/api/v1/settings`, { method: 'PUT', headers: H, body: '{nope' });
  assert.equal(r.status, 422);
});
test('errors are {error:{code,message}}; unknown things are 404; a body over 1 MiB is 413', async () => {
  const r = await get('/systems/nope/status');
  assert.equal(r.status, 404);
  const b = await r.json();
  assert.ok(b.error.code && b.error.message);
  const big = await fetch(`${base}/api/v1/systems/switchyard/config/routes/validate`, { method: 'POST', headers: H, body: JSON.stringify({ content: 'x'.repeat(1100000) }) }).catch((e) => ({
    status: 413,
    err: e,
  }));
  assert.equal(big.status, 413);
});
test('a down Dynamo is 200 with health=down and metrics.available=false (never a 5xx)', async () => {
  await dev(`${base}/__dev/scenario?dynamo=down`);
  const st = await (await get('/systems/dynamo/status')).json();
  assert.equal(st.health, 'down');
  assert.ok(st.problems[0].fix);
  const m = await (await get('/systems/dynamo/metrics')).json();
  assert.equal(m.available, false);
  assert.deepEqual(m.points, []);
  assert.ok(m.error);
  assert.equal((await post('/systems/dynamo/playground/chat', { model: 'unsloth/Qwen3.8-27B-NVFP4', messages: [{ role: 'user', content: 'x' }] })).status, 503);
  await dev(`${base}/__dev/scenario?dynamo=loading`);
  const ld = await (await get('/systems/dynamo/status')).json();
  assert.equal(ld.health, 'degraded');
  assert.match(ld.summary, /loading/i);
  await dev(`${base}/__dev/scenario?dynamo=off`);
  const off = await (await get('/systems/dynamo/status')).json();
  assert.equal(off.health, 'off');
  assert.deepEqual(
    off.problems.map((p) => p.severity),
    ['info'],
  );
  assert.equal((await (await get('/health')).json()).systems.dynamo, 'off');
  await dev(`${base}/__dev/scenario?dynamo=up`);
});
test('config: read, validate (authoritative flag), 409 on stale sha, rollback outcome, read-only 403, secret refusal', async () => {
  const f = await (await get('/systems/switchyard/config/routes')).json();
  assert.equal(f.sha256, sha(f.content));
  assert.equal(f.spec.editable, true);
  const bad = f.content.replace('capable_target = "flash-next-auto"', 'capable_target = "nope"');
  const v = await (await post('/systems/switchyard/config/routes/validate', { content: bad })).json();
  assert.equal(v.ok, false);
  assert.match(v.diagnostics[0].message, /unknown target nope/);
  assert.ok(v.diagnostics[0].line > 0);
  assert.equal(v.authoritative, true);
  const rv = await (await post('/systems/relay/config/routes/validate', { content: 'schema_version = 1\n' })).json();
  assert.equal(rv.authoritative, false);
  assert.ok(rv.note);
  let r = await post('/systems/switchyard/config/routes/apply', { content: f.content + '# x\n', base_sha256: 'deadbeef', restart: true, confirm: true });
  assert.equal(r.status, 409);
  r = await post('/systems/switchyard/config/routes/apply', { content: f.content, base_sha256: f.sha256, restart: true });
  assert.equal(r.status, 422); // confirm missing
  r = await post('/systems/switchyard/config/routes/apply', { content: f.content + '# sk-abcdefghijklmnopqrstuv\n', base_sha256: f.sha256, restart: true, confirm: true });
  assert.equal(r.status, 422);
  assert.equal((await r.json()).error.code, 'secret_in_content');
  const rb = await (await post('/systems/switchyard/config/routes/apply', { content: f.content + '\n# unhealthy\n', base_sha256: f.sha256, restart: true, confirm: true })).json();
  assert.equal(rb.ok, false);
  assert.equal(rb.rolled_back, true);
  assert.equal(rb.post_check, 'unhealthy');
  assert.ok(rb.revision_id);
  assert.equal((await (await get('/systems/switchyard/config/routes')).json()).sha256, f.sha256, 'the file is restored after auto-rollback');
  const ok = await (await post('/systems/switchyard/config/routes/apply', { content: f.content + '# ok\n', base_sha256: f.sha256, restart: true, confirm: true })).json();
  assert.equal(ok.ok, true);
  assert.equal(ok.post_check, 'healthy');
  assert.equal(ok.new_sha256, sha(f.content + '# ok\n'));
  const hist = await (await get('/systems/switchyard/config/routes/history')).json();
  assert.ok(hist.length >= 3 && hist[0].id && hist[0].sha256);
  r = await post('/systems/relay/config/plugins/apply', { content: 'x', base_sha256: 'a', restart: false, confirm: true });
  assert.equal(r.status, 403);
  const spec = await (await get('/systems/relay/config')).json();
  const ro = spec.filter((s) => !s.editable);
  assert.equal(ro.length, 2);
  assert.ok(ro.every((s) => s.why_readonly));
  await dev(`${base}/__dev/touch`);
  r = await post('/systems/switchyard/config/routes/apply', { content: f.content, base_sha256: ok.new_sha256, restart: false, confirm: true });
  assert.equal(r.status, 409);
});
test('actions: disruptive need confirm; dynamo up is refused while the sharing engine runs unless stop_sharing_engine', async () => {
  let r = await post('/systems/switchyard/actions/restart', { params: {}, confirm: false });
  assert.equal(r.status, 422);
  r = await post('/systems/switchyard/actions/dry_run_config', { params: {}, confirm: false });
  assert.equal(r.status, 200);
  await dev(`${base}/__dev/scenario?dynamo=down`);
  await dev(`${base}/__dev/vllm?run=1`);
  const pf = await (await post('/systems/dynamo/actions/preflight', { params: {}, confirm: false })).json();
  assert.match(pf.output, /shares_card: ai-lab-vllm-qwen38-vllm-1 \(running\)/);
  assert.match(pf.output, /qwen3\.8-27b-1m/);
  assert.equal(pf.problems[0].severity, 'warn');
  let up = await (await post('/systems/dynamo/actions/up', { params: {}, confirm: true })).json();
  assert.equal(up.ok, false);
  assert.ok(up.problems[0].fix);
  up = await (await post('/systems/dynamo/actions/up', { params: { stop_sharing_engine: true }, confirm: true })).json();
  assert.equal(up.ok, true);
  assert.match(up.output, /stopped ai-lab-vllm-qwen38-vllm-1/);
  assert.equal((await (await get('/systems/dynamo/status')).json()).health, 'degraded');
  await dev(`${base}/__dev/scenario?dynamo=up`);
  await dev(`${base}/__dev/vllm?run=0`);
});
test('params: catalog, preview renders, "[set]" is never accepted back, secrets are not in the preview', async () => {
  const p = await (await get('/systems/dynamo/params')).json();
  assert.ok(p.catalog.length > 8);
  assert.equal(p.values.hf_token, '[set]');
  assert.deepEqual((await (await get('/systems/switchyard/params')).json()).catalog, []);
  const pv = await (await post('/systems/dynamo/params/preview', { values: { gpu_memory_utilization: 0.97, max_num_seqs: 32 } })).json();
  assert.match(pv.rendered, /--gpu-memory-utilization/);
  assert.ok(pv.warnings.length);
  assert.equal((await post('/systems/dynamo/params/preview', { values: { hf_token: '[set]' } })).status, 422);
  assert.equal((await post('/systems/dynamo/params/preview', { values: { gpu_memory_utilization: 5 } })).status, 422);
  assert.equal((await post('/systems/dynamo/params', { values: { max_num_seqs: 8 } }, H, 'PUT')).status, 422); // confirm missing
  assert.ok(!(await (await post('/systems/dynamo/params/preview', { values: { hf_token: 'hf_realsecretvalue' } })).text()).includes('hf_realsecretvalue'));
});
test('settings never reveal tokens ([set, N chars]); audit records mutations with content redacted', async () => {
  const s = await (await get('/settings')).json();
  for (const v of Object.values(s.tokens)) assert.match(v, /^\[(set, \d+ chars|unset)\]$/);
  assert.ok(s.poll_interval_s && s.playground_key_env && !('intervals' in s) && 'restart_required' in s);
  assert.equal((await fetch(`${base}/api/v1/settings`, { method: 'PUT', headers: H, body: JSON.stringify({ urls: { litellm: 'https://evil.example' } }) })).status, 422);
  assert.equal((await fetch(`${base}/api/v1/settings`, { method: 'PUT', headers: H, body: JSON.stringify({ playground_key_env: { switchyard: 'sk-abc lowercase' } }) })).status, 422);
  const a = await (await get('/audit?limit=50')).json();
  assert.ok(a.length);
  assert.ok(a.every((x) => x.id && x.path && x.role && !('action' in x)));
  assert.ok(a.some((x) => /\/config\/routes\/apply$/.test(x.path)));
  assert.ok(a.every((x) => !JSON.stringify(x).includes('# unhealthy')));
  assert.ok(a.every((x) => x.params?.content === undefined || x.params.content === '[redacted]'));
});
test('viewer role: GETs work, every mutation is 403, audit is 403', async () => {
  await dev(`${base}/__dev/role?set=viewer`);
  assert.equal((await get('/overview')).status, 200);
  assert.equal((await get('/audit')).status, 403);
  assert.equal((await get('/health').then((r) => r.json())).role, 'viewer');
  assert.equal((await post('/systems/switchyard/actions/restart', { params: {}, confirm: true })).status, 403);
  await dev(`${base}/__dev/role?set=admin`);
});
test('SSE: events stream delivers SystemEvent JSON with metadata only; 9th stream is 429; playground streams deltas + served-by headers', async () => {
  const ctrls = [];
  const open = async () => {
    const c = new AbortController();
    ctrls.push(c);
    return fetch(`${base}/api/v1/systems/switchyard/events/stream`, { signal: c.signal });
  };
  const r = await open();
  assert.equal(r.headers.get('content-type'), 'text/event-stream');
  const reader = r.body.getReader(),
    dec = new TextDecoder(),
    parser = new SSEParser();
  let evs = [];
  const t0 = Date.now();
  while (!evs.length && Date.now() - t0 < 8000) {
    const { value } = await reader.read();
    evs = parser.feed(dec.decode(value));
  }
  const ev = JSON.parse(evs[0].data);
  assert.equal(ev.system, 'switchyard');
  assert.equal(ev.kind, 'routing.decision');
  assert.ok(ev.data.route && ev.data.tier);
  assert.ok(!('prompt' in ev.data) && !('content' in ev.data) && !('messages' in ev.data));
  for (let i = 0; i < 7; i++) await open();
  const over = await open();
  assert.equal(over.status, 429);
  assert.equal((await over.json()).error.code, 'too_many_streams');
  ctrls.forEach((c) => c.abort());
  const pg = await post('/systems/switchyard/playground/chat', { model: 'auto-local', messages: [{ role: 'user', content: 'hi' }], max_tokens: 5, stream: true });
  assert.ok(pg.headers.get('x-served-by-route'));
  assert.ok(pg.headers.get('x-served-by-tier'));
  const txt = await pg.text();
  assert.match(txt, /"content":"Routing"/);
  assert.match(txt, /data: \[DONE\]/);
  assert.equal((await post('/systems/switchyard/playground/chat', { model: 'auto-local', messages: [{ role: 'user', content: 'x' }], max_tokens: 99999 })).status, 422);
  const rl = await post('/systems/relay/playground/chat', { model: 'auto-local', messages: [{ role: 'user', content: 'x' }], max_tokens: 3 });
  assert.match(await rl.text(), /content_block_delta/);
});
test('bearer mode: 401 without the key (health stays open)', async () => {
  // exercised via the fail-injection helper so this file does not need a second server
  await dev(`${base}/__dev/fail?status=401&n=1`);
  const r = await get('/overview');
  assert.equal(r.status, 401);
  assert.equal((await r.json()).error.code, 'unauthorized');
  // 421 is skipped here: fetch() transparently retries a 421 on a fresh connection (spec behaviour), which would hide the injected failure; the real 421 is covered above via node:http.
  for (const st of [403, 409, 413, 415, 429, 503]) {
    await dev(`${base}/__dev/fail?status=${st}&n=1`);
    assert.equal((await get('/overview')).status, st);
  }
});

test('dev helpers need X-Console; static server refuses traversal spellings and wrong Host/port; non-loopback --host needs MOCK_KEY', async () => {
  assert.equal((await fetch(`${base}/__dev/role?set=viewer`)).status, 403);
  assert.equal((await get('/health').then((r) => r.json())).role, 'admin');
  const raw = (p, host) =>
    new Promise((ok) =>
      http.get({ host: '127.0.0.1', port: server.address().port, path: p, headers: host ? { Host: host } : {} }, (x) => {
        x.resume();
        ok(x.statusCode);
      }),
    );
  for (const p of ['/css/..%2fdev/mock-server.mjs', '/css/%2e%2e/dev/mock-server.mjs', '/css/..%5cdev/x', '/js/..//dev/mock-server.mjs', '/css\\x']) assert.equal(await raw(p), 400, p);
  assert.equal(await raw('/dev/mock-server.mjs'), 404);
  assert.equal(await raw('/css/base.css'), 200);
  assert.equal(await raw('/css/base.css', 'localhost:1'), 421);
  assert.equal(await raw('/css/base.css', 'evil.example'), 421);
  assert.equal(await raw('/__dev/role?set=admin', 'evil.example'), 421);
  const { spawnSync } = await import('node:child_process');
  const r = spawnSync(process.execPath, ['dev/mock-server.mjs', '--host', '0.0.0.0', '--port', '0'], {
    cwd: new URL('..', import.meta.url).pathname,
    env: { ...process.env, MOCK_KEY: '' },
    timeout: 5000,
  });
  assert.equal(r.status, 2);
  assert.match(String(r.stderr), /refusing --host 0\.0\.0\.0/);
});

// ---- Relay trace explorer (docs/ARCHITECTURE.md addendum 9.12): every route answers with exactly the documented keys.
const keysOf = (o) => Object.keys(o).sort();
const same = (o, list, what) => assert.deepEqual(keysOf(o), [...list].sort(), what);
const K = {
  index: 'state generation last_refresh_ts build_ms files events spans open_spans marks chunk_marks traces sessions trajectories coverage truncated limits content problems',
  file: 'name bytes scanned_bytes compressed events skipped_lines state first_ts last_ts',
  trace:
    'trace_id root_name kind client session_key relay_session_id start_ts end_ts duration_ms status spans llm_calls tool_calls tool_errors marks subagents models input_tokens output_tokens cached_tokens reasoning_tokens total_tokens max_input_tokens ttfc_ms routing error has_trajectory',
  routing: 'algorithm route decision served_model overhead_ms errors',
  span: 'id parent_id name category subtype role flags start_ts end_ts duration_ms status depth turn_index llm tool error content_available size_bytes',
  llm: 'model served_model provider api stream input_tokens output_tokens cached_tokens cache_write_tokens reasoning_tokens total_tokens ttfc_ms tokens_per_s finish_reason messages tool_defs tool_calls_requested requested_tools max_tokens reasoning_effort chunks chunk_offsets_ms max_chunk_gap_ms correlation',
  mark: 'id parent_id ts name category schema severity data',
  content: 'span_id category source request response tool headers truncated redactions',
  request: 'model params system messages tools messages_total messages_returned',
  part: 'type text name call_id chars truncated',
  session: 'session_key client kind first_ts last_ts span_ms traces llm_calls tool_calls errors models input_tokens output_tokens cached_tokens max_input_tokens',
  call: 'ts trace_id span_id model input_tokens cached_tokens output_tokens reasoning_tokens duration_ms ttfc_ms status',
  analytics: 'range bucket_s totals series by_model by_client by_kind histograms heatmap tools tool_requests routing errors scatter',
  totals:
    'traces sessions llm_calls tool_calls errors error_rate input_tokens output_tokens cached_tokens reasoning_tokens cache_hit_ratio p50_ms p95_ms p99_ms ttfc_p50_ms ttfc_p95_ms tokens_per_s_p50',
  series: 'ts traces llm_calls errors input_tokens output_tokens cached_tokens p50_ms p95_ms ttfc_p50_ms by_model',
  byModel: 'model llm_calls errors input_tokens output_tokens cached_tokens reasoning_tokens p50_ms p95_ms ttfc_p50_ms ttfc_p95_ms tokens_per_s_p50 tokens_per_s_p95',
  hist: 'duration_ms ttfc_ms input_tokens output_tokens tokens_per_s chunk_gap_ms',
  routingA: 'requests by_algorithm decisions judge overhead_p50_ms overhead_p95_ms failures flows',
  errorsA: 'by_type by_status finish_reasons recent',
  scatter: 'trace_id span_id ts duration_ms input_tokens output_tokens model status',
  traj: 'id source file bytes mtime schema_version agent session_id steps tool_calls prompt_tokens completion_tokens cached_tokens first_ts last_ts trace_id',
  step: 'step_id ts source model message_chars reasoning_chars tool_calls observations metrics duration_ms span_id status',
  stepContent: 'step_id message reasoning tool_calls observations truncated redactions',
  capture: 'relay_version levels projections hints',
  level: 'id label enabled state detail observed',
};
const kw = (k) => K[k].split(' ');
const J = async (p) => {
  const r = await get(`/systems/relay${p}`);
  return [r.status, await r.json()];
};

test('9.12: the index state, the trace list and one trace carry exactly the documented keys', async () => {
  let [st, ix] = await J('/traces/index');
  assert.equal(st, 200);
  same(ix, kw('index'), 'IndexState');
  assert.equal(ix.state, 'ready');
  same(ix.files[0], kw('file'), 'file');
  same(ix.coverage, ['from_ts', 'to_ts']);
  same(ix.limits, ['scan_bytes', 'max_spans', 'max_line_bytes']);
  assert.ok(ix.problems.every((p) => p.code && p.message));
  let list;
  [st, list] = await J('/traces?window=all&limit=200');
  same(list, ['range', 'total', 'offset', 'limit', 'items', 'facets']);
  same(list.range, ['from', 'to']);
  assert.ok(list.total >= 40 && list.items.length === list.total);
  for (const it of list.items) same(it, kw('trace'), 'TraceSummary');
  // facets are pinned: exactly these four lists of {value, count}, counted over the window BEFORE any filter, sorted, at most 50
  assert.deepEqual(keysOf(list.facets), ['client', 'kind', 'model', 'status']);
  for (const f of ['client', 'model', 'kind', 'status']) {
    assert.ok(list.facets[f].length > 0 && list.facets[f].length <= 50, `facet ${f}`);
    for (const x of list.facets[f]) {
      assert.deepEqual(keysOf(x), ['count', 'value'], `facet ${f} item`);
      assert.ok(Number.isInteger(x.count) && x.count > 0);
    }
    const counts = list.facets[f].map((x) => x.count);
    assert.deepEqual(
      counts,
      [...counts].sort((a, b) => b - a),
      `facet ${f} is sorted by count descending`,
    );
  }
  assert.deepEqual(list.facets.client.at(-1), { value: null, count: 1 }, 'a null client is counted under value null');
  assert.equal(
    list.facets.client.reduce((a, x) => a + x.count, 0),
    list.total,
  );
  const filtered = (await J('/traces?window=all&status=error&client=opencode&limit=200'))[1];
  assert.deepEqual(filtered.facets, list.facets, 'choosing filters does not change the facets');
  assert.deepEqual(new Set(list.items.map((x) => x.client).filter(Boolean)), new Set(['opencode', 'claude-cli', 'example-agent']));
  assert.ok(
    list.items.some((x) => x.client === null),
    'a trace with no recognisable client',
  );
  assert.ok(new Set(list.items.flatMap((x) => x.models)).size >= 4);
  assert.ok(list.items.some((x) => x.status === 'open') && list.items.some((x) => x.status === 'error'));
  const routed = list.items.filter((x) => x.routing);
  assert.ok(routed.length >= 4 && routed.some((x) => x.routing.errors > 0), 'routed traces, one with a routing failure');
  same(routed[0].routing, kw('routing'));
  assert.ok(
    list.items.some((x) => x.subagents > 0 && x.tool_calls > 0),
    'a trace with tools and a subagent',
  );
  assert.ok(list.items.some((x) => x.kind === 'gateway') && list.items.some((x) => x.kind !== 'gateway'));
  const one = list.items.find((x) => x.subagents > 0);
  let d;
  [st, d] = await J(`/traces/${one.trace_id}`);
  same(d, ['trace', 'spans', 'marks', 'truncated']);
  same(d.trace, kw('trace'));
  assert.ok(d.spans.length === one.spans && d.spans.every((s) => keysOf(s).join() === [...kw('span')].sort().join()));
  assert.ok(d.spans.filter((s) => s.llm).every((s) => keysOf(s.llm).join() === [...kw('llm')].sort().join()));
  assert.ok(d.marks.length && d.marks.every((m) => keysOf(m).join() === [...kw('mark')].sort().join()));
  assert.ok(
    d.spans.some((s) => s.llm?.chunk_offsets_ms.length > 0 && s.llm.ttfc_ms != null),
    'a streaming span with chunk cadence',
  );
});

test('9.12: list filters, sort, pagination and window/anchor are honoured; bad values are 422 invalid_request', async () => {
  const L = async (qs) => (await J(`/traces?${qs}`))[1];
  const all = await L('window=all&limit=200');
  const err = await L('window=all&status=error&limit=200');
  assert.ok(err.total >= 3 && err.items.every((x) => x.status === 'error'));
  assert.ok((await L('window=all&client=opencode&limit=200')).items.every((x) => x.client === 'opencode'));
  assert.ok((await L('window=all&model=judge-small&limit=200')).items.every((x) => x.models.includes('judge-small')));
  assert.ok((await L('window=all&min_ms=15000&limit=200')).items.every((x) => x.duration_ms >= 15000));
  const page = await L('window=all&limit=5&offset=5&sort=duration&order=desc');
  assert.equal(page.items.length, 5);
  assert.equal(page.offset, 5);
  const durs = (await L('window=all&sort=duration&order=desc&limit=200')).items.map((x) => x.duration_ms).filter((v) => v != null);
  assert.deepEqual(
    durs,
    [...durs].sort((a, b) => b - a),
  );
  const w1 = await L('window=1h&anchor=latest');
  assert.ok(w1.total < all.total, 'a short window keeps fewer traces');
  const now1 = await L('window=1h&anchor=now');
  assert.ok(now1.range.to > w1.range.to - 1, 'anchor=now ends at the current time');
  assert.ok((await L(`window=all&q=${all.items[3].session_key}`)).total >= 1);
  for (const bad of ['window=2d', 'anchor=never', 'limit=0', 'limit=500', 'sort=colour', 'order=up', 'status=maybe', 'min_ms=-1', `q=${'x'.repeat(81)}`, 'q=has%20space']) {
    const [s, b] = await J(`/traces?${bad}`);
    assert.equal(s, 422, bad);
    assert.equal(b.error.code, 'invalid_request', bad);
  }
});

test('9.12: unknown and malformed ids; other systems have no explorer', async () => {
  assert.equal((await J('/traces/00000000-0000-7000-8000-000000000000'))[0], 404);
  assert.equal((await J('/traces/not-a-uuid'))[0], 422);
  assert.equal((await J('/sessions/no-such-session'))[0], 404);
  assert.equal((await J('/trajectories/00000000-0000-7000-8000-000000000000'))[0], 404);
  assert.equal((await J('/trajectories/not-a-uuid'))[0], 422);
  const r = await get('/systems/switchyard/traces');
  assert.equal(r.status, 404);
  assert.equal((await r.json()).error.code, 'not_found');
  const w = await fetch(`${base}/api/v1/systems/relay/traces`, { method: 'POST', headers: H, body: '{}' });
  assert.equal(w.status, 404, 'the explorer routes are GET only');
});

test('9.12: span content is admin only, can be switched off, and its shapes match', async () => {
  const list = (await J('/traces?window=all&limit=200'))[1].items;
  const withLlm = list.find((x) => x.subagents > 0);
  const tr = (await J(`/traces/${withLlm.trace_id}`))[1];
  const llm = tr.spans.find((s) => s.category === 'llm');
  const tool = tr.spans.find((s) => s.category === 'tool');
  let [st, c] = await J(`/traces/${withLlm.trace_id}/spans/${llm.id}/content`);
  assert.equal(st, 200);
  same(c, kw('content'));
  same(c.request, kw('request'));
  assert.ok(c.request.system.every((p) => keysOf(p).join() === [...kw('part')].sort().join()));
  assert.ok(c.request.messages.length && c.request.messages_returned === c.request.messages.length);
  assert.ok(c.request.messages.some((m) => m.parts.some((p) => p.type === 'reasoning')) && c.request.messages.some((m) => m.parts.some((p) => p.type === 'tool_call')));
  assert.deepEqual(c.source, { file: 'relay-events.jsonl', compressed: false });
  assert.ok(!Object.keys(c.headers).some((h) => /authorization|cookie|key|token/i.test(h)), 'only allowlisted headers');
  [st, c] = await J(`/traces/${withLlm.trace_id}/spans/${tool.id}/content`);
  assert.ok(c.tool.arguments && c.tool.result);
  assert.equal((await J(`/traces/${withLlm.trace_id}/spans/${llm.id}/content?max_chars=10`))[0], 422);
  assert.equal((await J(`/traces/${withLlm.trace_id}/spans/${llm.id}/content?messages=middle`))[0], 422);
  // 409: the source line of the oldest trace's first call is gone
  const oldest = (await J('/traces?window=all&limit=1&sort=start&order=asc'))[1].items[0];
  const oldSpan = (await J(`/traces/${oldest.trace_id}`))[1].spans.find((s) => s.category === 'llm');
  [st, c] = await J(`/traces/${oldest.trace_id}/spans/${oldSpan.id}/content`);
  assert.equal(st, 409);
  assert.equal(c.error.code, 'content_unavailable');
  // switched off
  await dev(`${base}/__dev/traces?content=off`);
  [st, c] = await J(`/traces/${withLlm.trace_id}/spans/${llm.id}/content`);
  assert.equal(st, 403);
  assert.equal(c.error.code, 'content_disabled');
  assert.equal((await J('/traces/index'))[1].content, 'off');
  await dev(`${base}/__dev/traces?content=admin`);
  // viewer
  await dev(`${base}/__dev/role?set=viewer`);
  [st, c] = await J(`/traces/${withLlm.trace_id}/spans/${llm.id}/content`);
  assert.equal(st, 403);
  assert.equal(c.error.code, 'forbidden');
  assert.equal((await J(`/traces/${withLlm.trace_id}`))[0], 200, 'metadata stays readable for a viewer');
  await dev(`${base}/__dev/role?set=admin`);
});

test('9.12: sessions, with context that grows; analytics, trajectories and capture carry the documented keys', async () => {
  const [, s] = await J('/sessions?window=all&limit=200');
  same(s, ['range', 'total', 'offset', 'limit', 'items']);
  for (const it of s.items) same(it, kw('session'), 'SessionSummary');
  const big = s.items.sort((a, b) => b.traces - a.traces)[0];
  assert.ok(big.traces >= 8);
  const [st, d] = await J(`/sessions/${encodeURIComponent(big.session_key)}`);
  assert.equal(st, 200);
  same(d, ['session', 'traces', 'calls']);
  same(d.session, kw('session'));
  assert.ok(d.calls.every((c) => keysOf(c).join() === [...kw('call')].sort().join()));
  const inputs = d.calls.map((c) => c.input_tokens);
  assert.ok(inputs.at(-1) > inputs[0] * 3, 'the context grows over the session');
  assert.ok(d.calls.at(-1).cached_tokens / d.calls.at(-1).input_tokens > d.calls[0].cached_tokens / d.calls[0].input_tokens, 'and the cache share grows with it');
  const a = (await J('/analytics?window=all'))[1];
  same(a, kw('analytics'));
  same(a.totals, kw('totals'));
  same(a.series, kw('series'));
  same(a.series.by_model[0], ['model', 'llm_calls', 'tokens']);
  same(a.by_model[0], kw('byModel'));
  same(a.histograms, kw('hist'));
  for (const h of Object.values(a.histograms)) assert.equal(h.edges.length, h.counts.length + 1);
  same(a.heatmap, ['activity', 'latency']);
  same(a.heatmap.activity, ['days', 'hours', 'counts']);
  same(a.heatmap.latency, ['ts', 'edges_ms', 'counts']);
  // pinned orientation: latency counts[time][bucket], activity counts[day][hour]
  assert.equal(a.heatmap.latency.counts.length, a.heatmap.latency.ts.length);
  assert.ok(a.heatmap.latency.counts.every((row) => row.length === a.heatmap.latency.edges_ms.length - 1));
  assert.equal(a.heatmap.activity.counts.length, a.heatmap.activity.days.length);
  assert.ok(a.heatmap.activity.counts.every((row) => row.length === 24) && a.heatmap.activity.hours.length === 24);
  same(a.routing, kw('routingA'));
  assert.ok(a.routing.flows.length && a.routing.decisions.length && a.routing.failures.length);
  same(a.errors, kw('errorsA'));
  assert.ok(a.scatter.length > 0 && a.scatter.length <= 600);
  same(a.scatter[0], kw('scatter'));
  for (const k of ['ts', 'traces', 'llm_calls', 'errors', 'input_tokens', 'output_tokens', 'cached_tokens', 'p50_ms', 'p95_ms', 'ttfc_p50_ms']) assert.equal(a.series[k].length, a.series.ts.length, k);
  assert.ok((await J('/analytics?window=all&client=opencode'))[1].totals.traces < a.totals.traces, 'filters narrow the totals');
  assert.equal((await J('/analytics?buckets=5'))[0], 422);
  const [, tj] = await J('/trajectories');
  same(tj, ['total', 'offset', 'limit', 'items']);
  assert.equal(tj.total, 6);
  assert.deepEqual(new Set(tj.items.map((x) => x.source)), new Set(['gateway', 'example-agent']));
  for (const it of tj.items) same(it, kw('traj'));
  assert.equal((await J('/trajectories?source=example-agent'))[1].total, 3);
  const linkedId = tj.items.find((x) => x.trace_id).trace_id;
  const byTrace = await J(`/trajectories?trace=${linkedId}`);
  assert.equal(byTrace[0], 200);
  assert.equal(byTrace[1].total, 1);
  assert.ok(byTrace[1].items.every((x) => x.trace_id === linkedId));
  assert.equal((await J('/trajectories?trace=00000000-0000-7000-8000-000000000000'))[1].total, 0, 'a trace without a trajectory: an empty list, not an error');
  assert.equal((await J('/trajectories?trace=not-a-uuid'))[0], 422);
  const linked = tj.items.find((x) => x.trace_id);
  assert.ok(linked);
  const [, td] = await J(`/trajectories/${linked.id}`);
  same(td, ['trajectory', 'steps', 'truncated']);
  assert.ok(td.steps.every((x) => keysOf(x).join() === [...kw('step')].sort().join()));
  const sc = (await J(`/trajectories/${linked.id}/steps/${td.steps[2].step_id}/content`))[1];
  same(sc, kw('stepContent'));
  const cap = (await J('/capture'))[1];
  same(cap, kw('capture'));
  assert.deepEqual(
    cap.levels.map((l) => l.id),
    ['atof', 'atif', 'otel_traces', 'otel_logs', 'otel_metrics', 'payloads', 'redaction', 'resource_metrics', 'pricing'],
  );
  for (const l of cap.levels) same(l, kw('level'));
  assert.deepEqual(cap.projections, { gen_ai: true, openinference: true, full: true });
  // pinned keys per level, in both variants (a level that is off still carries every key)
  const DETAIL = {
    atof: ['sinks'],
    atif: ['agent_name'],
    otel_traces: ['endpoints'],
    otel_logs: ['derived', 'endpoints', 'minimum_severity'],
    otel_metrics: ['derived', 'endpoints', 'export_interval_ms'],
    payloads: ['mode'],
    redaction: ['detectors'],
    resource_metrics: ['interval_ms', 'polling'],
    pricing: [],
  };
  const OBSERVED = { atof: ['bytes', 'events', 'files', 'last_ts'], atif: ['bytes', 'last_ts', 'trajectories'] };
  for (const variant of ['full', 'gen_ai']) {
    await dev(`${base}/__dev/traces?capture=${variant}`);
    const cv = (await J('/capture'))[1];
    for (const l of cv.levels) {
      assert.deepEqual(keysOf(l.detail), DETAIL[l.id], `${variant} ${l.id} detail`);
      if (OBSERVED[l.id]) assert.deepEqual(keysOf(l.observed), OBSERVED[l.id], `${variant} ${l.id} observed`);
      else if (l.id.startsWith('otel_') || ['redaction', 'resource_metrics', 'pricing'].includes(l.id)) assert.deepEqual(l.observed, {}, `${l.id}: the console cannot observe it, so observed is {}`);
    }
    const lv = Object.fromEntries(cv.levels.map((l) => [l.id, l]));
    assert.deepEqual(keysOf(lv.atof.detail.sinks[0]), ['file', 'mode', 'type']);
    for (const e of lv.otel_traces.detail.endpoints) {
      assert.deepEqual(keysOf(e), ['projection', 'reachable', 'service_name', 'target', 'transport']);
      assert.match(e.target, /^[\w.-]+:\d+$/, 'target is host:port');
      assert.ok(e.reachable === null || typeof e.reachable === 'boolean');
    }
    for (const id of ['otel_logs', 'otel_metrics']) for (const e of lv[id].detail.endpoints) assert.deepEqual(keysOf(e), ['reachable', 'target', 'transport']);
    assert.ok(['full', 'turn_filtered'].includes(lv.payloads.detail.mode));
    assert.deepEqual(lv.pricing.detail, {});
    assert.ok(
      lv.otel_traces.detail.endpoints.some((e) => e.reachable === null),
      'an endpoint that is not on this machine is not checked',
    );
  }
  await dev(`${base}/__dev/traces?capture=full`);
  const dup = (await J('/capture'))[1].levels;
  assert.ok(
    dup.find((l) => l.id === 'otel_logs').detail.derived === true && dup.find((l) => l.id === 'otel_traces').detail.endpoints.length > 1,
    'derived logs beside several trace endpoints: the duplicate export case',
  );
  await dev(`${base}/__dev/traces?capture=gen_ai`);
  assert.deepEqual((await J('/capture'))[1].projections, { gen_ai: true, openinference: false, full: false });
  await dev(`${base}/__dev/traces?capture=full`);
});

test('9.12: the index states building, empty and disabled are selectable and the lists follow', async () => {
  for (const [state, expectTraces] of [
    ['building', (n) => n > 0 && n < 47],
    ['empty', (n) => n === 0],
    ['disabled', (n) => n === 0],
  ]) {
    await dev(`${base}/__dev/traces?index=${state}`);
    const ix = (await J('/traces/index'))[1];
    same(ix, kw('index'), state);
    assert.equal(ix.state, state);
    assert.ok(expectTraces((await J('/traces?window=all'))[1].total), `${state}: list`);
    assert.equal((await J('/analytics?window=all'))[0], 200, `${state}: analytics`);
    if (state === 'building') {
      const f = ix.files.find((x) => x.state === 'partial');
      assert.ok(f && f.scanned_bytes > 0 && f.scanned_bytes < f.bytes, 'a build in progress has a partial file');
    }
    if (state === 'empty') assert.deepEqual(ix.files, []);
  }
  await dev(`${base}/__dev/traces?index=ready`);
  assert.equal((await J('/traces/index'))[1].state, 'ready');
});

test('9.12: span flags are ATOF flags, roles are session | turn | subagent | null, a stale capture shows an old coverage', async () => {
  const list = (await J('/traces?window=all&limit=200'))[1].items;
  const tr = (await J(`/traces/${list.find((x) => x.subagents > 0).trace_id}`))[1];
  const ATOF = new Set(['streaming', 'parallel', 'remote', 'stateful', 'relocatable']);
  for (const sp of tr.spans) {
    assert.ok(
      sp.flags.every((f) => ATOF.has(f)),
      `flags ${sp.flags}`,
    );
    assert.ok([null, 'session', 'turn', 'subagent'].includes(sp.role), `role ${sp.role}`);
  }
  assert.ok(tr.spans.some((sp) => sp.role === 'subagent') && tr.spans.some((sp) => sp.role === 'turn') && tr.spans.some((sp) => sp.role === 'session'));
  assert.ok(tr.spans.some((sp) => sp.llm?.stream && sp.flags.includes('streaming')));
  const fresh = (await J('/traces/index'))[1];
  assert.ok(Date.now() / 1000 - fresh.coverage.to_ts < 3600, 'by default the newest event is recent');
  await dev(`${base}/__dev/traces?stale=1`);
  const stale = (await J('/traces/index'))[1];
  assert.equal(stale.state, 'ready');
  assert.ok(Date.now() / 1000 - stale.coverage.to_ts > 2 * 86400, 'a stale capture: the newest event is days old');
  assert.equal((await J('/traces?window=24h&anchor=now'))[1].total, 0, 'nothing in the last 24 hours');
  assert.ok((await J('/traces?window=24h&anchor=latest'))[1].total > 0, 'but the latest-event anchor still shows the last activity');
  await dev(`${base}/__dev/traces?stale=0`);
  assert.ok(Date.now() / 1000 - (await J('/traces/index'))[1].coverage.to_ts < 3600);
});
