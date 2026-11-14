// The mock's wave-2 data follows addendum 9.14: the fleet route, the new keys of DCGM, Switchyard and Dynamo (and they go through the series route),
// XID events, Dynamo log events. The UI is built against this mock, so it must behave like the contract.
import test, { before, after } from 'node:test';
import assert from 'node:assert/strict';
import { server } from '../dev/mock-server.mjs';
import { FLEET_HOSTS, fleetAnswer, FLEET_MAX_ROWS } from '../dev/mock-wave2.mjs';
import { shapeFleet } from '../js/dcgm-model.js';
import { normalizeAnswer } from '../js/series-model.js';

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
const metrics = async (id) => (await json(`/systems/${id}/metrics`)).points;
const keysOf = (points) => new Set(points.map((p) => p.key));
const REAL_NAMES = /spark\d|ai300|aitop|node\d/i;

test('fleet route: the documented shape, three or more hosts with one down, several GPUs, generic host names', async () => {
  for (const [window, step] of [
    ['1h', 60],
    ['24h', 300],
    ['7d', 1800],
  ]) {
    const r = await get(`/systems/dcgm/fleet?metric=util&window=${window}`);
    assert.equal(r.status, 200);
    const a = await r.json();
    assert.deepEqual(Object.keys(a).sort(), ['from', 'metric', 'rows', 'step_s', 'to', 'truncated', 'unit', 'window']);
    assert.equal(a.step_s, step);
    assert.equal(a.unit, 'ratio');
    assert.equal(a.truncated, false);
    assert.equal((a.to - a.from) % step, 0, 'the grid sits on whole steps');
    assert.ok(a.rows.length >= 6, 'several GPUs');
    assert.ok(new Set(a.rows.map((r) => r.host)).size >= 3, 'three or more hosts');
    for (const row of a.rows) {
      assert.deepEqual(Object.keys(row).sort(), ['gpu', 'host', 'job', 'model', 'points']);
      assert.equal(typeof row.gpu, 'string');
      assert.match(row.host, /^host-[a-z]$/);
      assert.ok(row.points.every((p) => p.length === 2 && p.every(Number.isFinite)));
      assert.ok(
        row.points.every((p, i) => i === 0 || p[0] - row.points[i - 1][0] >= step),
        'in time order',
      );
      assert.ok(
        row.points.every((p) => p[1] >= 0 && p[1] <= 1),
        'a ratio',
      );
    }
    const order = a.rows.map((r) => `${r.host}/${String(r.gpu).padStart(3, '0')}`);
    assert.deepEqual(order, [...order].sort(), 'ordered by host, then GPU');
    assert.ok(a.rows.length <= FLEET_MAX_ROWS);
  }
});

test('fleet route: units follow the per-GPU keys (ratio, C, W, B) and the metrics differ', async () => {
  const want = { util: 'ratio', temp: 'C', power: 'W', fb_used: 'B' };
  const last = {};
  for (const [metric, unit] of Object.entries(want)) {
    const a = await json(`/systems/dcgm/fleet?metric=${metric}&window=1h`);
    assert.equal(a.unit, unit);
    assert.equal(a.metric, metric);
    last[metric] = a.rows[0].points.at(-1)[1];
  }
  assert.ok(last.fb_used > 1e9, 'bytes');
  assert.ok(last.temp > 20 && last.temp < 100);
  assert.ok(last.power > 10 && last.power < 700);
});

test('fleet route: a host that is down has history that ends; in the 1h window it has no rows at all; one host has a hole', async () => {
  const h24 = await json('/systems/dcgm/fleet?metric=power&window=24h');
  const d = h24.rows.filter((r) => r.host === 'host-d');
  assert.ok(d.length >= 1, 'its earlier history is still there');
  assert.ok(d[0].points.at(-1)[0] < h24.to - 3600, 'it ends well before now');
  const full = h24.rows.find((r) => r.host === 'host-a').points.length;
  assert.ok(d[0].points.length < full);
  const b = h24.rows.find((r) => r.host === 'host-b').points;
  assert.ok(b.length < full, 'host-b had an outage');
  assert.ok(
    b.some((p, i) => i && p[0] - b[i - 1][0] > 300),
    'a hole in the series',
  );
  const h1 = await json('/systems/dcgm/fleet?metric=power&window=1h');
  assert.ok(!h1.rows.some((r) => r.host === 'host-d'), 'a host down for the whole window has no series');
  const up = (await metrics('dcgm')).filter((p) => p.key === 'dcgm.host_up');
  assert.deepEqual(Object.fromEntries(up.map((p) => [p.labels.host, p.value])), { 'host-a': 1, 'host-b': 1, 'host-c': 1, 'host-d': 0 });
});

test('fleet route: the same request answers the same values (a heatmap does not jump on refresh)', async () => {
  const a = await json('/systems/dcgm/fleet?metric=temp&window=1h');
  const b = await json('/systems/dcgm/fleet?metric=temp&window=1h');
  assert.deepEqual(a.rows[0].points, b.rows[0].points);
});

test('fleet route: 422 for an unknown or missing metric, an unknown window; other systems 404; a bare window defaults to 24h', async () => {
  for (const q of ['metric=nope&window=1h', 'window=1h', 'metric=util&window=15m', 'metric=util&window=30d', 'metric=Util&window=1h']) {
    const r = await get(`/systems/dcgm/fleet?${q}`);
    assert.equal(r.status, 422, q);
    assert.equal((await r.json()).error.code, 'invalid_request', q);
  }
  assert.equal((await get('/systems/switchyard/fleet?metric=util&window=1h')).status, 404);
  assert.equal((await get('/systems/dynamo/fleet?metric=util')).status, 404);
  assert.equal((await json('/systems/dcgm/fleet?metric=util')).window, '24h');
});

test('fleet route dev modes: Prometheus down is 200 with rows [] and a problem; empty is 200 with rows []; unsupported is a 404', async () => {
  try {
    await dev('/__dev/wave2?fleet=prom_down');
    const a = await json('/systems/dcgm/fleet?metric=util&window=1h');
    assert.deepEqual(a.rows, []);
    assert.equal(a.problem.code, 'dcgm.prometheus.unreachable');
    assert.ok(a.problem.message);
    await dev('/__dev/wave2?fleet=empty');
    const e = await json('/systems/dcgm/fleet?metric=util&window=1h');
    assert.deepEqual(e.rows, []);
    assert.equal('problem' in e, false, 'no problem field on a success');
    await dev('/__dev/wave2?fleet=unsupported');
    assert.equal((await get('/systems/dcgm/fleet?metric=util&window=1h')).status, 404);
  } finally {
    await dev('/__dev/wave2?fleet=ok');
  }
  assert.equal((await get('/systems/dcgm/fleet?metric=util&window=1h')).status, 200);
  assert.equal((await fetch(`${base}/__dev/wave2?fleet=ok`)).status, 403, 'the dev helper needs the custom header like the others');
});

test('the fleet answer shapes into a heatmap model with a gap for the outage and a row of gaps for the down host', async () => {
  const a = await json('/systems/dcgm/fleet?metric=util&window=24h');
  const hosts = (await metrics('dcgm')).filter((p) => p.key === 'dcgm.host_up').map((p) => ({ host: p.labels.host, job: p.labels.job, up: p.value > 0 }));
  const m = shapeFleet(a, { maxCols: 160, hostsUp: hosts });
  assert.ok(m.cols <= 160);
  const b = m.rows.find((r) => r.host === 'host-b');
  assert.ok(
    b.values.some((v) => v == null),
    'the hole is a gap',
  );
  assert.ok(b.values.some((v) => v != null));
  const d = m.rows.find((r) => r.host === 'host-d');
  assert.equal(d.down, true);
  assert.ok(d.values.at(-1) == null, 'the down host has nothing at the right edge');
  const m1 = shapeFleet(await json('/systems/dcgm/fleet?metric=util&window=1h'), { hostsUp: hosts });
  const dd = m1.rows.find((r) => r.host === 'host-d');
  assert.equal(dd.missing, true);
  assert.ok(dd.values.every((v) => v == null));
});

test('the DCGM snapshot has the 9.14 keys with their label sets (gpu, model; reason; kind), and no key the card would draw as 0 when absent', async () => {
  const pts = await metrics('dcgm');
  const keys = keysOf(pts);
  for (const k of [
    'dcgm.fb_reserved_b',
    'dcgm.mem_temp_c',
    'dcgm.mem_clock_mhz',
    'dcgm.enc_util',
    'dcgm.dec_util',
    'dcgm.power_limit_w',
    'dcgm.energy_mj_total',
    'dcgm.violation_us_total',
    'dcgm.remapped_rows',
    'dcgm.row_remap_failure',
  ])
    assert.ok(keys.has(k), k);
  const per = (k) => pts.filter((p) => p.key === k);
  assert.ok(per('dcgm.power_limit_w').every((p) => p.labels.gpu != null && p.labels.model));
  assert.deepEqual([...new Set(per('dcgm.violation_us_total').map((p) => p.labels.reason))].sort(), ['board', 'low_util', 'power', 'reliability', 'sync_boost', 'thermal']);
  assert.deepEqual([...new Set(per('dcgm.remapped_rows').map((p) => p.labels.kind))].sort(), ['correctable', 'uncorrectable']);
  assert.ok(
    pts.every((p) => !REAL_NAMES.test(JSON.stringify(p.labels))),
    'no real lab host names',
  );
  // the memory-temperature key is absent on a board with no memory sensor (the GB10 class), not 0
  await dev('/__dev/seven?dcgm_gpus=5');
  try {
    const many = await metrics('dcgm');
    const memT = many.filter((p) => p.key === 'dcgm.mem_temp_c');
    assert.ok(memT.length > 0 && memT.length < new Set(many.filter((p) => p.key === 'dcgm.gpu_temp_c').map((p) => p.labels.gpu)).size, 'some GPUs report no memory temperature');
    assert.ok(memT.every((p) => p.value > 0));
  } finally {
    await dev('/__dev/seven?dcgm_gpus=2');
  }
});

test('counters of the new DCGM keys are rates through the series route: energy in mJ/s, violations in us/s; the energy counter never goes down except at a restart', async () => {
  const a = await json('/systems/dcgm/metrics/series?keys=dcgm.energy_mj_total&window=24h&split=1');
  assert.equal(a.series.length, 2);
  assert.deepEqual(
    a.series.map((s) => s.label.gpu),
    ['0', '1'],
  );
  for (const s of a.series) {
    const drops = s.points.filter((p, i) => i && p[1] < s.points[i - 1][1]).length;
    assert.ok(drops <= 1, 'one restart at most in 24 hours');
    assert.ok(s.points.length > 1000);
  }
  const v = await json('/systems/dcgm/metrics/series?keys=dcgm.violation_us_total&window=1h&split=1');
  assert.equal(v.series.length, 12, 'six reasons for each of two GPUs');
  const power0 = v.series.find((s) => s.label.gpu === '0' && s.label.reason === 'power');
  const flat = v.series.find((s) => s.label.gpu === '1' && s.label.reason === 'power');
  // (the mock restarts its counters 40 minutes ago, so compare the newest steps, after the restart)
  assert.ok(power0.points.at(-1)[1] > power0.points.at(-2)[1], 'GPU 0 is throttled for power');
  assert.equal(flat.points.at(-1)[1] - flat.points.at(-2)[1], 0, 'GPU 1 is not');
});

test('Switchyard snapshot: route calls with the window label, per-model keys, decisions as source -> target, fallbacks, overhead, client outcomes', async () => {
  const pts = await metrics('switchyard');
  const keys = keysOf(pts);
  for (const k of [
    'sy.route.calls',
    'sy.model.calls',
    'sy.model.errors_total',
    'sy.model.latency_ms',
    'sy.model.cache_hit_ratio',
    'sy.model.max_context_tokens',
    'sy.routing_overhead_ms',
    'sy.fallbacks_total',
    'sy.decisions_total',
    'sy.client_responses_total',
  ])
    assert.ok(keys.has(k), k);
  const per = (k) => pts.filter((p) => p.key === k);
  assert.ok(per('sy.route.calls').length >= 3);
  assert.ok(per('sy.route.calls').every((p) => /^last_\d+_records$/.test(p.labels.window)));
  assert.deepEqual([...new Set(per('sy.model.latency_ms').map((p) => p.labels.q))].sort(), ['p50', 'p99']);
  assert.ok(per('sy.model.latency_ms').every((p) => p.labels.model));
  assert.deepEqual([...new Set(per('sy.routing_overhead_ms').map((p) => p.labels.q))].sort(), ['p50', 'p99']);
  assert.ok(
    per('sy.decisions_total').every((p) => p.labels.source && p.labels.target && !('route' in p.labels)),
    'the contract lists source and target only',
  );
  assert.ok(per('sy.fallbacks_total').some((p) => p.labels.reason === 'context_window'));
  assert.ok(per('sy.client_responses_total').some((p) => p.labels.outcome === 'ok'));
});

test('Switchyard series: counters are cumulative and the key comes back as requested; model calls differ by model; split=1 gives a line per label', async () => {
  const a = await json('/systems/switchyard/metrics/series?keys=sy.model.calls,sy.decisions_total&window=24h&split=1');
  const calls = a.series.filter((s) => s.key === 'sy.model.calls');
  assert.equal(calls.length, 4);
  for (const s of calls) assert.ok(s.points.filter((p, i) => i && p[1] < s.points[i - 1][1]).length <= 1);
  const rate = (s) => (s.points.at(-1)[1] - s.points[s.points.length - 2][1]) / 60;
  const by = Object.fromEntries(calls.map((s) => [s.label.model, rate(s)]));
  assert.ok(by['nemotron-lightning'] > by['qwen3.8-27b-1m']);
  const dec = a.series.filter((s) => s.key === 'sy.decisions_total');
  assert.equal(dec.length, 6);
  assert.ok(dec.every((s) => s.label.source && s.label.target));
  const labelled = await json('/systems/dynamo/metrics/series?keys=dyn.tokens%7Bkind%3Dcompletion%7D&window=1h&split=1');
  assert.equal(labelled.series[0].key, 'dyn.tokens{kind=completion}', 'the answer carries the key as requested');
  assert.deepEqual(labelled.series[0].label, { kind: 'completion' });
});

test('Dynamo snapshot: windowed latency (p50 / p95 / p99), phases, running / waiting, preemptions, capacity, spec-decode by position, output tokens as kind=completion', async () => {
  const pts = await metrics('dynamo');
  const per = (k) => pts.filter((p) => p.key === k);
  assert.deepEqual(
    per('dyn.ttft_window_ms')
      .map((p) => p.labels.q)
      .sort(),
    ['p50', 'p95', 'p99'],
  );
  assert.deepEqual(
    per('dyn.itl_window_ms')
      .map((p) => p.labels.q)
      .sort(),
    ['p50', 'p95', 'p99'],
  );
  assert.ok(per('dyn.ttft_ms').length > 0, 'the since-start keys stay');
  assert.equal(per('dyn.phase_ms').length, 8);
  assert.deepEqual([...new Set(per('dyn.phase_ms').map((p) => p.labels.phase))].sort(), ['decode', 'inference', 'prefill', 'queue']);
  assert.ok(per('dyn.requests_running').length === 1 && per('dyn.requests_waiting').length === 1 && per('dyn.preemptions_total').length === 1);
  assert.deepEqual(
    per('dyn.capacity')
      .map((p) => p.labels.kind)
      .sort(),
    ['context_length', 'kv_blocks', 'max_batched_tokens', 'max_num_seqs'],
  );
  assert.deepEqual(
    per('dyn.spec_accept_pos').map((p) => p.labels.pos),
    ['0', '1', '2', '3', '4'],
  );
  assert.deepEqual(
    per('dyn.tokens').map((p) => p.labels.kind),
    ['completion'],
  );
  assert.ok(per('dyn.prefix_cache_hit_ratio').length === 1);
});

test('Dynamo series: capacity is steady; windowed latency is a gauge; tokens are drawn as a rate (the key has no _total)', async () => {
  const a = await json('/systems/dynamo/metrics/series?keys=dyn.capacity,dyn.tokens%7Bkind%3Dcompletion%7D&window=1h&split=1');
  const cap = a.series.find((s) => s.label?.kind === 'context_length');
  assert.ok(
    cap.points.every((p) => p[1] === cap.points[0][1]),
    'a capacity does not wobble',
  );
  const tok = a.series.find((s) => s.key.startsWith('dyn.tokens'));
  const recent = tok.points.slice(-30); // after the mock's counter restart 40 minutes ago
  assert.ok(
    recent.every((p, i) => !i || p[1] >= recent[i - 1][1]),
    'cumulative',
  );
  const rate = (recent.at(-1)[1] - recent[0][1]) / (recent.at(-1)[0] - recent[0][0]);
  assert.ok(rate > 20 && rate < 60, `about 38 tokens per second, got ${rate}`);
  assert.ok(normalizeAnswer(a).length === a.series.length);
});

test('Dynamo log events: levels ERROR / WARN / INFO / DEBUG / TRACE, a component each, newest first, one line with markup and one redacted; down gives no stream data', async () => {
  const evs = await json('/systems/dynamo/events?limit=500');
  assert.ok(evs.length >= 100);
  assert.ok(evs.every((e) => e.kind === 'dynamo.log' && e.data.component && e.data.level));
  assert.deepEqual([...new Set(evs.map((e) => e.data.level))].sort(), ['DEBUG', 'ERROR', 'INFO', 'TRACE', 'WARN']);
  assert.deepEqual([...new Set(evs.map((e) => e.data.component))].sort(), ['frontend', 'worker']);
  assert.ok(evs.every((e, i) => !i || e.ts <= evs[i - 1].ts));
  assert.ok(
    evs.some((e) => /<img src=x/.test(e.summary)),
    'a line with markup, to prove it is shown as text',
  );
  assert.ok(evs.some((e) => /\[REDACTED\]/.test(e.summary)));
  assert.ok(evs.every((e) => !REAL_NAMES.test(e.summary)));
});

test('DCGM events: XID events carry a string gpu, next to the diagnostic runs; a few of them in the last week', async () => {
  const evs = await json('/systems/dcgm/events?limit=500');
  const xid = evs.filter((e) => e.kind === 'dcgm.xid');
  assert.ok(xid.length >= 4);
  const live = xid.filter((e) => e.data.source === 'last_xid'); // the shape the console emits
  assert.ok(live.length >= 4);
  assert.ok(live.every((e) => typeof e.data.gpu === 'string' && Number.isInteger(e.data.xid)));
  assert.ok(evs.filter((e) => e.kind === 'dcgm.diag').length >= 10, 'the diagnostic runs stay');
  assert.ok(evs.every((e, i) => !i || e.ts <= evs[i - 1].ts));
});

test('no real lab host name appears in any wave-2 mock data', () => {
  const all = JSON.stringify([FLEET_HOSTS, fleetAnswer({ metric: 'util', window: '1h', now: 1_800_000_000 })]);
  assert.ok(!REAL_NAMES.test(all));
});

test('the existing mock behaviour stays: the old DCGM keys, hosts as host_up labels, the since-start Dynamo keys and the Switchyard totals', async () => {
  const d = keysOf(await metrics('dcgm'));
  for (const k of ['dcgm.gpu_util', 'dcgm.gpu_temp_c', 'dcgm.power_w', 'dcgm.fb_used_b', 'dcgm.fb_free_b', 'dcgm.host_up', 'dcgm.pcie_replay_total']) assert.ok(d.has(k), k);
  const s = keysOf(await metrics('switchyard'));
  for (const k of ['sy.requests_total', 'sy.errors_total', 'sy.in_flight', 'sy.tokens']) assert.ok(s.has(k), k);
  const y = keysOf(await metrics('dynamo'));
  for (const k of ['dyn.up', 'dyn.requests_total', 'dyn.ttft_ms', 'dyn.itl_ms', 'dyn.kv_cache_usage']) assert.ok(y.has(k), k);
});
