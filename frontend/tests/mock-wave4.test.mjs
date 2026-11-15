// The mock's wave-4 data follows addendum 9.16 (the pinned shapes): the rails and decisions routes and their 404 / 422 answers, the persisted OpenShell lifecycle events and what is derived
// from them, the Flare targets, the up / down histories, the audit history with its summary and filters. The UI is built against this mock, so it must behave like the contract.
import test, { before, after } from 'node:test';
import assert from 'node:assert/strict';
import { server } from '../dev/mock-server.mjs';
import { shapeRails } from '../js/guardrails-model.js';
import { lifecycleLanes, presentSandboxes, phaseStack, decisionModel } from '../js/openshell-model.js';
import { targetGrid, targetsWithData, tokenMix } from '../js/flare-model.js';
import { shapeSummary } from '../js/audit-model.js';
import { stripFromAnswer } from '../js/strip-model.js';
import { SPARKS, sparkRequest, sparkModel } from '../js/overview-model.js';
import { SYSTEM_IDS } from '../js/systems.js';

let base;
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
const get = (p) => fetch(`${base}/api/v1${p}`);
const json = async (p) => (await get(p)).json();
const dev = (u) => fetch(`${base}${u}`, { headers: { 'X-Console': '1' } });
const REAL_NAMES = /spark\d|ai300|aitop|node\d/i;
const CATEGORIES = ['ssh', 'config', 'network', 'http', 'dns', 'process', 'filesystem', 'policy', 'auth', 'other'];
const KINDS = ['open', 'close', 'connect', 'request', 'applying', 'built', 'exec', 'read', 'write', 'resolve', 'load', 'other'];

// ---- guardrails rails

test('rails: the documented shape; every flow a plain name; an unreadable config is still listed; other systems are 404', async () => {
  const a = await json('/systems/guardrails/rails');
  assert.deepEqual(Object.keys(a).sort(), ['configs', 'problems']);
  for (const c of a.configs) {
    assert.deepEqual(Object.keys(c).sort(), ['id', 'input', 'main_model', 'output', 'retrieval', 'streaming']);
    assert.ok(c.main_model === null || typeof c.main_model === 'string');
    for (const f of [...c.input, ...c.output, ...c.retrieval]) assert.match(f, /^[A-Za-z0-9 _.-]{1,80}$/);
    if (c.streaming) assert.deepEqual(Object.keys(c.streaming).sort(), ['chunk_size', 'context_size', 'enabled']);
  }
  const m = shapeRails(a);
  assert.equal(m.configs.length, a.configs.length);
  assert.ok(
    m.configs.some((c) => c.input.length && c.output.length && c.streaming?.enabled === true),
    'a full config',
  );
  assert.ok(
    m.configs.some((c) => !c.input.length && !c.output.length && c.streaming === null && c.mainModel === null),
    'a bare one',
  );
  assert.equal((await get('/systems/openshell/rails')).status, 404);
  assert.equal((await get('/systems/switchyard/rails')).status, 404);
});

test('rails dev modes: empty, problem ({config, code, count}), unsupported (404)', async () => {
  await dev('/__dev/wave4?rails=empty');
  assert.deepEqual(await json('/systems/guardrails/rails'), { configs: [], problems: [] });
  await dev('/__dev/wave4?rails=problem');
  const p = await json('/systems/guardrails/rails');
  assert.ok(p.problems.length >= 1);
  for (const x of p.problems) assert.deepEqual(Object.keys(x).sort(), ['code', 'config', 'count']);
  assert.ok(
    p.configs.some((c) => c.id === 'lab-broken' && c.input.length === 0 && c.main_model === null),
    'a broken config is still listed, empty',
  );
  await dev('/__dev/wave4?rails=unsupported');
  assert.equal((await get('/systems/guardrails/rails')).status, 404);
  await dev('/__dev/wave4?rails=ok');
  assert.equal((await get('/systems/guardrails/rails')).status, 200);
});

// ---- openshell: lifecycle events and what follows from them

test("openshell events: persisted lifecycle events in the adapter's data shape, newest first, and they tell a coherent story", async () => {
  const evs = await json('/systems/openshell/events?limit=500');
  assert.ok(evs.length > 40);
  assert.ok(
    evs.every((e, i) => i === 0 || e.ts <= evs[i - 1].ts),
    'newest first',
  );
  for (const e of evs) {
    assert.equal(e.kind, 'openshell.sandbox');
    assert.deepEqual(Object.keys(e.data).sort(), ['action', 'id', 'name', 'phase', 'previous_phase']);
    assert.ok(['present', 'created', 'phase', 'deleted'].includes(e.data.action));
    assert.ok(e.data.action === 'phase' ? typeof e.data.previous_phase === 'string' : true);
  }
  const lanes = lifecycleLanes(evs, { from: Date.now() / 1000 - 7 * 86400, to: Date.now() / 1000, now: Date.now() / 1000, maxLanes: 100 });
  assert.ok(lanes.lanes.length >= 20);
  assert.ok(
    lanes.lanes.some((l) => l.segments.some((s) => s.bucket === 'Error')),
    'a run that ended in Error',
  );
  assert.ok(lanes.lanes.filter((l) => l.present).length === 3, 'three exist now');
  assert.ok(lanes.lanes.some((l) => l.provisionS != null && l.deletedTs != null));
  const status = await json('/systems/openshell/status');
  assert.match(status.summary, /3 sandboxes/);
});

test('openshell: the phase history comes from the same story (the counts add up to the sandboxes that existed) and the snapshot matches the present list', async () => {
  const ans = await json('/systems/openshell/metrics/series?keys=os.sandboxes_by_phase&window=24h&split=1');
  assert.equal(ans.series.length, 5);
  assert.deepEqual(
    ans.series.map((s) => s.label.phase),
    ['Deleting', 'Error', 'Other', 'Provisioning', 'Ready'],
  );
  const m = phaseStack(ans);
  assert.equal(m.any, true);
  assert.ok(m.peak >= 2, 'merged columns are means: the last minutes with three are blended with the ones before');
  const snap = await json('/systems/openshell/metrics');
  const byPhase = Object.fromEntries(snap.points.filter((p) => p.key === 'os.sandboxes_by_phase').map((p) => [p.labels.phase, p.value]));
  const total = snap.points.find((p) => p.key === 'os.sandboxes').value;
  assert.equal(
    Object.values(byPhase).reduce((a, b) => a + b, 0),
    total,
  );
  assert.equal(total, presentSandboxes(await json('/systems/openshell/events?limit=500')).length);
  assert.equal(byPhase.Ready, 2);
  assert.equal(byPhase.Provisioning, 1);
  const last = m.latest;
  assert.equal(last.Ready, 2);
});

// ---- decisions

test('decisions: the documented shape and fixed vocabularies; oldest first; targets only host:port or null; deterministic', async () => {
  const q = '/systems/openshell/decisions?sandbox=dd-create-9c1e&limit=2000';
  const a = await json(q);
  assert.deepEqual(Object.keys(a).sort(), ['rows', 'sandbox', 'truncated']);
  assert.equal(a.sandbox, 'dd-create-9c1e');
  assert.equal(a.truncated, false);
  assert.ok(a.rows.length > 60);
  for (const r of a.rows) {
    assert.deepEqual(Object.keys(r).sort(), ['category', 'kind', 'target', 'ts', 'verdict']);
    assert.ok(CATEGORIES.includes(r.category), r.category);
    assert.ok(KINDS.includes(r.kind), r.kind);
    assert.ok(['allowed', 'denied'].includes(r.verdict));
    assert.ok(r.target === null || /^[A-Za-z0-9.-]+:\d{1,5}$/.test(r.target), String(r.target));
    assert.ok(typeof r.ts === 'number');
  }
  assert.ok(
    a.rows.every((r, i) => i === 0 || r.ts >= a.rows[i - 1].ts),
    'oldest first',
  );
  assert.deepEqual(
    (await json(q)).rows.map((r) => [r.category, r.verdict, r.kind, r.target]),
    a.rows.map((r) => [r.category, r.verdict, r.kind, r.target]),
    'the same answer twice',
  );
  assert.ok(a.rows.some((r) => r.verdict === 'denied') && a.rows.some((r) => r.verdict === 'allowed'));
  const m = decisionModel(a);
  assert.equal(m.total, a.rows.length);
  assert.equal(m.skipped, 0);
  assert.ok(m.lanes.length >= 4 && !m.sparse);
  assert.ok(!REAL_NAMES.test(JSON.stringify(a)));
});

test('decisions: the limit keeps the NEWEST rows (default 500, at most 2000) and says truncated; bad input is 422; an unknown sandbox and another system are 404', async () => {
  const all = await json('/systems/openshell/decisions?sandbox=dd-create-9c1e&limit=2000');
  const few = await json('/systems/openshell/decisions?sandbox=dd-create-9c1e&limit=10');
  assert.equal(few.rows.length, 10);
  assert.equal(few.truncated, true);
  // (the mock's rows are placed relative to the clock of each request, so the times drift by a few milliseconds between two requests)
  const key = (rows) => rows.map((r) => [r.category, r.verdict, r.kind, r.target].join('|'));
  assert.deepEqual(key(few.rows), key(all.rows.slice(-10)), 'the newest ten, still oldest first');
  assert.ok(few.rows.every((r, i) => Math.abs(r.ts - all.rows.at(-10 + i).ts) < 1));
  const dflt = await json('/systems/openshell/decisions?sandbox=dd-create-9c1e');
  assert.ok(dflt.rows.length <= 500);
  for (const bad of ['limit=0', 'limit=2001', 'limit=abc', 'limit=1.5']) {
    const r = await get(`/systems/openshell/decisions?sandbox=dd-create-9c1e&${bad}`);
    assert.equal(r.status, 422, bad);
    assert.equal((await r.json()).error.code, 'invalid_request');
  }
  for (const bad of ['', 'sandbox=', 'sandbox=a%20b', 'sandbox=../x', `sandbox=${'a'.repeat(200)}`]) assert.equal((await get(`/systems/openshell/decisions${bad ? `?${bad}` : ''}`)).status, 422, bad);
  const nope = await get('/systems/openshell/decisions?sandbox=gone-sandbox');
  assert.equal(nope.status, 404);
  assert.equal((await nope.json()).error.code, 'not_found');
  assert.equal((await get('/systems/guardrails/decisions?sandbox=dd-create-9c1e')).status, 404);
});

test('decisions dev modes: empty, sparse (the SSH open only), many (truncated), fail (200 with a problem and no rows)', async () => {
  const url = '/systems/openshell/decisions?sandbox=dd-create-9c1e&limit=500';
  await dev('/__dev/wave4?decisions=empty');
  assert.deepEqual((await json(url)).rows, []);
  await dev('/__dev/wave4?decisions=sparse');
  const sp = await json(url);
  assert.equal(sp.rows.length, 1);
  assert.deepEqual([sp.rows[0].category, sp.rows[0].kind, sp.rows[0].verdict], ['ssh', 'open', 'allowed']);
  assert.equal(decisionModel(sp).sparse, true);
  await dev('/__dev/wave4?decisions=many');
  const many = await json(url);
  assert.equal(many.rows.length, 500);
  assert.equal(many.truncated, true);
  await dev('/__dev/wave4?decisions=fail');
  const res = await get(url);
  assert.equal(res.status, 200);
  const f = await res.json();
  assert.deepEqual(f.rows, []);
  assert.equal(f.problem.code, 'openshell.logs.failed');
  assert.equal(typeof f.problem.message, 'string');
  assert.equal(decisionModel(f).problem.code, 'openshell.logs.failed');
  await dev('/__dev/wave4?decisions=ok');
});

// ---- flare

test("flare: one component per target in the adapter's text, all the states the grid draws, and engine counters only for the targets that serve", async () => {
  const st = await json('/systems/flare/status');
  const grid = targetGrid(st, (await json('/systems/flare/metrics')).points);
  assert.equal(grid.targets.length, 7);
  assert.deepEqual(
    grid.groups.map((g) => g.chip),
    ['sm120', 'sm121'],
  );
  assert.deepEqual([grid.counts.serving, grid.counts.loading, grid.counts.busy], [2, 1, 4]);
  assert.equal(grid.targets.find((t) => t.target === 'spark-all').hosts.length, 4);
  assert.equal(grid.targets.find((t) => t.target === 'spark-b').packages, 0);
  assert.equal(grid.targets.find((t) => t.target === 'local').model.length > 0, true);
  const ans = await json('/systems/flare/metrics/series?keys=flare.generated_total,flare.prefill_total,flare.cache_reused_total,flare.active_requests,flare.pending_requests&window=1h&split=1');
  assert.deepEqual(targetsWithData(ans), ['local', 'spark-all']);
  assert.ok(ans.series.every((s) => s.label && s.label.target));
  const mix = tokenMix(ans);
  assert.ok(mix.rows.every((r) => r.values.generated > 0 && r.values.prefill > 0));
  assert.ok(!REAL_NAMES.test(JSON.stringify(st)), 'generic names only');
  await dev('/__dev/wave4?flare=mixed');
  const mixed = targetGrid(await json('/systems/flare/status'));
  assert.deepEqual(
    Object.keys(mixed.counts)
      .filter((k) => mixed.counts[k] > 0)
      .sort(),
    ['absent', 'busy', 'failed', 'loading', 'serving', 'stopped', 'unknown'],
    'every state once',
  );
  assert.equal(targetsWithData(await json('/systems/flare/metrics/series?keys=flare.generated_total&window=1h&split=1')).join(), 'local');
  await dev('/__dev/wave4?flare=none');
  assert.equal(targetGrid(await json('/systems/flare/status')).counts.serving, 0);
  assert.deepEqual(targetsWithData(await json('/systems/flare/metrics/series?keys=flare.generated_total&window=1h&split=1')), []);
  await dev('/__dev/wave4?flare=default');
});

// ---- the up / down histories and the sparklines

test('up histories: gr.up has down periods and holes (no data), values in 0..1; the strip finds the down run; a system without an up key has none', async () => {
  const day = await json('/systems/guardrails/metrics/series?keys=gr.up&window=24h');
  const strip = stripFromAnswer(day, 'gr.up', { maxCells: 1440 });
  assert.ok(strip.downRuns.length >= 1, 'the 26 h-ago outage is outside 24 h, the 90 min one is inside');
  const coarse = stripFromAnswer(day, 'gr.up', { maxCells: 96 });
  assert.equal(coarse.downRuns.length, 0, 'seven minutes of outage in a 15-minute cell is a partial cell, not a down one');
  assert.ok(coarse.cells.some((c) => c.state === 'part'));
  assert.ok(
    strip.cells.some((c) => c.state === 'none'),
    'a hole where the console was not sampling',
  );
  assert.ok(day.series[0].points.every((p) => p[1] >= 0 && p[1] <= 1));
  const hour = stripFromAnswer(await json('/systems/guardrails/metrics/series?keys=gr.up&window=1h'), 'gr.up');
  assert.ok(hour.uptime > 0.85 && hour.uptime <= 1);
  const week = stripFromAnswer(await json('/systems/guardrails/metrics/series?keys=gr.up&window=7d'), 'gr.up');
  assert.ok(
    week.cells.some((c) => c.state === 'part' || c.state === 'down'),
    'the outage three days ago shows',
  );
  for (const k of ['os.gateway_up', 'relay.gateway_up', 'dyn.up']) {
    const sys = Object.entries(SPARKS).find(([, s]) => s.up === k)[0];
    assert.ok(stripFromAnswer(await json(`/systems/${sys}/metrics/series?keys=${k}&window=1h`), k), k);
  }
});

test('every system has samples for its Overview sparkline over the hour (one request each), and the sparkline model reads them', async () => {
  for (const id of SYSTEM_IDS) {
    const req = sparkRequest(id);
    const a = await json(`/systems/${id}/metrics/series?keys=${req.keys.join(',')}&window=${req.window}&split=${req.split ? 1 : 0}`);
    const m = sparkModel(a, id);
    assert.equal(m.any, true, `${id}: no sparkline samples`);
    assert.ok(m.values.length >= 30, id);
  }
});

// ---- audit

test('audit log: thirty days of history behind the live rows, five actors, error statuses, newest first; the filters since / until / actor / status / system / limit', async () => {
  const all = await json('/audit?limit=500');
  assert.ok(all.length > 300, `${all.length} rows of history`);
  assert.ok(all.every((r, i) => i === 0 || r.ts <= all[i - 1].ts));
  assert.ok(all.every((r) => r.id && r.path && r.role && !('action' in r) && typeof r.status === 'number'));
  const actors = new Set(all.map((r) => r.actor));
  assert.ok(actors.size >= 5);
  const now = Date.now() / 1000;
  const day = await json(`/audit?limit=500&since=${Math.floor(now - 86400)}`);
  assert.ok(day.length >= 10 && day.length < all.length && day.every((r) => r.ts >= now - 86400 - 1));
  const win = await json(`/audit?limit=500&since=${Math.floor(now - 3 * 86400)}&until=${Math.floor(now - 86400)}`);
  assert.ok(win.every((r) => r.ts >= now - 3 * 86400 - 1 && r.ts <= now - 86400 + 1) && win.length > 0);
  const ops = await json('/audit?limit=500&actor=ops-console');
  assert.ok(ops.length > 0 && ops.every((r) => r.actor === 'ops-console'));
  const errs = await json('/audit?limit=500&status=error');
  assert.ok(errs.length > 0 && errs.every((r) => r.status >= 400));
  const oks = await json('/audit?limit=500&status=ok');
  assert.ok(oks.every((r) => r.status < 400));
  assert.ok((await json('/audit?limit=500&system=dynamo')).every((r) => r.system === 'dynamo'));
  assert.equal((await json('/audit?limit=7')).length, 7);
  assert.deepEqual(await json('/audit?actor=nobody'), []);
  const both = await json('/audit?limit=500&actor=ops-console&status=error&system=switchyard');
  assert.ok(both.every((r) => r.actor === 'ops-console' && r.status >= 400 && r.system === 'switchyard'));
  for (const bad of ['since=abc', 'until=-1', 'status=maybe', 'since=1e9x']) {
    const r = await get(`/audit?${bad}`);
    assert.equal(r.status, 422, bad);
    assert.equal((await r.json()).error.code, 'invalid_request');
  }
  assert.equal((await get('/audit?actor=&status=&since=')).status, 200, 'an empty value is no filter');
});

test("audit summary: the pinned shape; counts agree with the log; a null system is the console's own calls; windows, bucket sizes and 422", async () => {
  for (const [w, bucket] of [
    ['24h', 3600],
    ['7d', 21600],
    ['30d', 86400],
  ]) {
    const s = await json(`/audit/summary?window=${w}`);
    assert.deepEqual(Object.keys(s).sort(), ['bucket_s', 'by_actor', 'by_day', 'denied_total', 'errors', 'from', 'to', 'truncated', 'window']);
    assert.equal(s.window, w);
    assert.equal(s.bucket_s, bucket);
    assert.ok(s.to > s.from && s.to - s.from === { '24h': 86400, '7d': 604800, '30d': 2592000 }[w]);
    for (const r of s.by_day) assert.match(r.day, /^\d{4}-\d{2}-\d{2}$/);
    assert.ok(
      s.errors.every((e) => e.count > 0 && e.ts_bucket % bucket === 0),
      'only non-empty buckets',
    );
    assert.equal(typeof s.denied_total, 'number');
    assert.equal(s.truncated, false);
    const dayTotal = s.by_day.reduce((a, r) => a + r.count, 0);
    assert.equal(
      s.by_actor.reduce((a, r) => a + r.count, 0),
      dayTotal,
      'both views count the same calls',
    );
    const log = await json(`/audit?limit=500&since=${s.from}&until=${s.to}`);
    if (log.length < 500) assert.equal(log.length, dayTotal, `${w}: the summary and the log agree`);
    assert.equal(
      s.errors.reduce((a, e) => a + e.count, 0),
      log.filter((r) => r.status >= 400).length,
      `${w}: errors`,
    );
  }
  const s30 = await json('/audit/summary?window=30d');
  assert.ok(
    s30.by_day.some((r) => r.system === null),
    'a call that belongs to no system has system null',
  );
  assert.ok(s30.by_actor.length <= 200);
  assert.ok(
    s30.by_actor.every((r, i) => i === 0 || r.count <= s30.by_actor[i - 1].count),
    'busiest first',
  );
  assert.equal((await json('/audit/summary')).window, '7d', 'the default window');
  const model = shapeSummary(s30);
  assert.ok(model.days.ts.length >= 28 && model.actors.length === 5 && model.errors.total > 0);
  // denied_total counts the audited rows answered 401/403 in the window (not a separate counter of refused requests)
  const log30 = await json(`/audit?limit=500&since=${s30.from}&until=${s30.to}`);
  if (log30.length < 500) assert.equal(model.deniedTotal, log30.filter((x) => x.status === 401 || x.status === 403).length);
  const r = await get('/audit/summary?window=1y');
  assert.equal(r.status, 422);
  assert.equal((await r.json()).error.code, 'invalid_request');
});

test('audit summary and filtered log are admin only; the mock holds no real host or user names', async () => {
  await dev('/__dev/role?set=viewer');
  assert.equal((await get('/audit/summary?window=7d')).status, 403);
  assert.equal((await get('/audit?status=error')).status, 403);
  assert.equal((await get('/systems/guardrails/rails')).status, 200, 'viewers read the rails');
  assert.equal((await get('/systems/openshell/decisions?sandbox=dd-create-9c1e')).status, 200, 'and the decisions');
  await dev('/__dev/role?set=admin');
  const text = JSON.stringify([
    await json('/audit/summary?window=30d'),
    await json('/audit?limit=500'),
    await json('/systems/openshell/events?limit=500'),
    await json('/systems/guardrails/rails'),
    await json('/systems/flare/status'),
  ]);
  assert.ok(!REAL_NAMES.test(text));
});
