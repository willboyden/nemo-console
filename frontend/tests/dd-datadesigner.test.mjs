// The datadesigner additions to the mock must behave like the contract addendum: records/files endpoints,
// script config files, the five actions, dd.run SSE events (metadata only), and the live-create busy check.
import test, { before, after } from 'node:test';
import assert from 'node:assert/strict';
import { server } from '../dev/mock-server.mjs';
import { SSEParser } from '../js/sse.js';

let base; const H = { 'X-Console': '1', 'Content-Type': 'application/json' };
before(() => new Promise((r) => server.listen(0, '127.0.0.1', () => { base = `http://127.0.0.1:${server.address().port}`; r(); })));
after(() => { server.closeAllConnections?.(); server.close(); });
const dev = (u) => fetch(u, { headers: { 'X-Console': '1' } });
const get = (p) => fetch(`${base}/api/v1${p}`);
const post = (p, body) => fetch(`${base}/api/v1${p}`, { method: 'POST', headers: H, body: JSON.stringify(body) });

test('status and metrics: launcher + router components, problems carry fixes, dd.* metric keys', async () => {
  const st = await (await get('/systems/datadesigner/status')).json();
  assert.equal(st.system, 'datadesigner'); assert.equal(st.health, 'up'); assert.equal(st.version, '0.9.3');
  assert.match(st.summary, /scripts · \d+ aliases/);
  const kinds = st.components.map((c) => `${c.kind}:${c.name}`);
  assert.ok(kinds.some((k) => k.startsWith('process:data-designer-local')));
  assert.ok(kinds.some((k) => k.startsWith('http:router')));
  assert.ok(st.problems.every((p) => p.code && p.message), 'problems have stable codes');
  assert.ok(st.problems.some((p) => p.fix && p.code === 'dd.no_key'));
  assert.equal(st.endpoints[0].name, 'router (generation)');
  const m = await (await get('/systems/datadesigner/metrics')).json();
  const keys = m.points.map((p) => p.key);
  for (const k of ['dd.scripts', 'dd.runs', 'dd.live_running', 'dd.last_records']) assert.ok(keys.includes(k), k);
  assert.ok(m.points.some((p) => /^dd\.runs_by_verdict\{verdict=.+\}$/.test(p.key)));
  assert.ok(keys.every((k) => !/content|prompt|record_text/.test(k)), 'metadata only');
});

test('config: scripts are config files (ids contain .py), validate mentions local execution, apply is healthy', async () => {
  const specs = await (await get('/systems/datadesigner/config')).json();
  assert.equal(specs.length, 3);
  assert.ok(specs.every((s) => s.editable && s.id.endsWith('.py') && s.format === 'text'));
  const f = await (await get('/systems/datadesigner/config/support_tickets.py')).json();
  assert.match(f.content, /def load_config_builder/); assert.equal(f.sha256.length, 64);
  const ok = await (await post('/systems/datadesigner/config/support_tickets.py/validate', { content: f.content })).json();
  assert.equal(ok.ok, true); assert.equal(ok.authoritative, true); assert.match(ok.note, /executes this script locally/i);
  const bad = await (await post('/systems/datadesigner/config/support_tickets.py/validate', { content: '# no builder here\n' })).json();
  assert.equal(bad.ok, false); assert.equal(bad.diagnostics[0].severity, 'error'); assert.ok(bad.diagnostics[0].line >= 1);
  let r = await post('/systems/datadesigner/config/support_tickets.py/apply', { content: f.content + '\n# ok\n', base_sha256: 'deadbeef', restart: true, confirm: true });
  assert.equal(r.status, 409, 'stale base is still rejected');
  const applied = await (await post('/systems/datadesigner/config/support_tickets.py/apply', { content: f.content + '\n# ok\n', base_sha256: f.sha256, restart: true, confirm: true })).json();
  assert.equal(applied.ok, true); assert.equal(applied.post_check, 'healthy'); assert.equal(applied.restarted, false, 'a script has nothing to restart');
  assert.ok(applied.revision_id);
});

test('records/files endpoints: server-driven pagination, unknown run 404, files list', async () => {
  const id = '20261023T091512-104-create-support_tickets';
  let r = await get(`/systems/datadesigner/runs/${id}/records`);
  let b = await r.json();
  assert.equal(b.run_id, id); assert.equal(b.total, 120); assert.equal(b.rows.length, 50); assert.equal(b.truncated, false);
  assert.deepEqual(b.columns, ['id', 'subject', 'priority', 'assignee', 'created_at', 'body']);
  assert.equal(b.rows[0].id, 1); assert.equal(typeof b.rows[0].body, 'string');
  b = await (await get(`/systems/datadesigner/runs/${id}/records?limit=50&offset=100`)).json();
  assert.equal(b.rows.length, 20); assert.equal(b.rows[0].id, 101);
  b = await (await get(`/systems/datadesigner/runs/${id}/records?limit=1000`)).json();
  assert.equal(b.rows.length, 120, 'limit is capped server-side (cap 200 not reached for 120 rows)');
  r = await get('/systems/datadesigner/runs/nope/records'); assert.equal(r.status, 404);
  r = await get('/systems/datadesigner/runs/nope/files'); assert.equal(r.status, 404);
  b = await (await get(`/systems/datadesigner/runs/${id}/files`)).json();
  assert.equal(b.run_id, id);
  assert.ok(b.files.length >= 3 && b.files.every((f) => typeof f.path === 'string' && typeof f.bytes === 'number'));
  assert.ok(b.files.some((f) => /metadata\.json$/.test(f.path)));
  // A running create run answers with an empty records page (not 404).
  await dev(`${base}/__dev/dd?do=start`);
  const live = (await (await dev(`${base}/__dev/dd`)).json()).live;
  assert.ok(live);
  b = await (await get(`/systems/datadesigner/runs/${encodeURIComponent(live)}/records`)).json();
  assert.equal(b.total, 0); assert.deepEqual(b.rows, []);
  await dev(`${base}/__dev/dd?do=finish`);
});

test('actions: create is disruptive (confirm required), refused while a run is live; safe actions pass through', async () => {
  const list = await (await get('/systems/datadesigner/actions')).json();
  assert.deepEqual(list.map((a) => a.id).sort(), ['check_models', 'create', 'preview', 'report', 'show_config']);
  const create = list.find((a) => a.id === 'create');
  assert.equal(create.danger, 'disruptive');
  assert.ok(create.params.some((p) => p.type === 'enum' && p.choices.some((c) => c.endsWith('.py'))));
  assert.equal((await post('/systems/datadesigner/actions/create', { params: {}, confirm: false })).status, 422);
  let r = await (await post('/systems/datadesigner/actions/create', { params: { script: 'support_tickets.py', num_records: 10, dataset_name: 'ds_x', destination_subdir: 'ds_x', format: 'csv' }, confirm: true })).json();
  assert.equal(r.ok, true); assert.match(r.output, /started in the background/);
  const live = (await (await dev(`${base}/__dev/dd`)).json()).live;
  r = await (await post('/systems/datadesigner/actions/create', { params: {}, confirm: true })).json();
  assert.equal(r.ok, false, 'one create at a time'); assert.ok(r.problems[0].message);
  await dev(`${base}/__dev/dd?do=finish`);
  const rep = await (await post('/systems/datadesigner/actions/report', { params: { run_id: live }, confirm: false })).json();
  assert.equal(rep.ok, true); assert.match(rep.output, /verdict: succeeded/);
  const repBad = await (await post('/systems/datadesigner/actions/report', { params: { run_id: 'ghost' }, confirm: false })).json();
  assert.equal(repBad.ok, false);
  const cm = await (await post('/systems/datadesigner/actions/check_models', { params: {}, confirm: false })).json();
  assert.equal(cm.ok, true); assert.match(cm.output, /lab-text/);
  const sc = await (await post('/systems/datadesigner/actions/show_config', { params: {}, confirm: false })).json();
  assert.match(sc.output, /model_providers\.yaml/); assert.match(sc.output, /LITELLM_DD_KEY/);
  const pv = await (await post('/systems/datadesigner/actions/preview', { params: { script: 'support_tickets.py', num_records: 3 }, confirm: false })).json();
  assert.equal(pv.ok, true); assert.match(pv.output, /sample_records_browser\.html/);
  // preview registered a run with its records
  const evs = await (await get('/systems/datadesigner/events?limit=50')).json();
  const last = evs.find((e) => e.kind === 'dd.run' && e.data.mode === 'preview');
  assert.ok(last); assert.ok(last.data.run_id && !('rows' in last.data) && !('body' in last.data), 'metadata only');
  const recs = await (await get(`/systems/datadesigner/runs/${encodeURIComponent(last.data.run_id)}/records`)).json();
  assert.equal(recs.total, 3);
});

test('SSE: a create run start/finish is delivered as dd.run metadata events', async () => {
  const ctl = new AbortController();
  const res = await fetch(`${base}/api/v1/systems/datadesigner/events/stream`, { signal: ctl.signal });
  assert.equal(res.headers.get('content-type'), 'text/event-stream');
  const reader = res.body.getReader(), dec = new TextDecoder(), parser = new SSEParser();
  const readEvents = async (want) => {
    const out = [];
    const t0 = Date.now();
    while (out.length < want && Date.now() - t0 < 15000) {
      const { value, done } = await reader.read();
      if (done) break;
      for (const e of parser.feed(dec.decode(value, { stream: true }))) { try { out.push(JSON.parse(e.data)); } catch { /* ping */ } }
    }
    return out;
  };
  await post('/systems/datadesigner/actions/create', { params: { script: 'release_notes.py', num_records: 5, dataset_name: 'mock_stream_ds', destination_subdir: 'mock_stream_ds', format: 'jsonl' }, confirm: true });
  const evs = await readEvents(1);
  assert.ok(evs.length >= 1, 'got the running event');
  const start = evs.find((e) => e.kind === 'dd.run' && e.data.verdict === 'running');
  assert.ok(start); assert.equal(start.system, 'datadesigner');
  assert.ok(start.data.run_id.includes('create-release_notes')); assert.ok(!('rows' in start.data) && !('body' in start.data));
  const done = await readEvents(1);
  const fin = done.find((e) => e.kind === 'dd.run' && e.data.verdict === 'succeeded');
  assert.ok(fin, 'got the finished event'); assert.equal(fin.data.num_records, 5);
  assert.equal(typeof fin.data.duration_ms, 'number');
  ctl.abort();
});

test('viewer role: the datadesigner mutations are 403, GETs stay open', async () => {
  await dev(`${base}/__dev/role?set=viewer`);
  assert.equal((await post('/systems/datadesigner/actions/preview', { params: {}, confirm: false })).status, 403);
  assert.equal((await post('/systems/datadesigner/config/support_tickets.py/validate', { content: 'x' })).status, 403);
  assert.equal((await get('/systems/datadesigner/status')).status, 200);
  assert.equal((await get('/systems/datadesigner/runs/20261023T091512-104-create-support_tickets/files')).status, 200);
  await dev(`${base}/__dev/role?set=admin`);
});
