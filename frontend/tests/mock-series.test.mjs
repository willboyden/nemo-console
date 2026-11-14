// The mock's GET /systems/{id}/metrics/series and run events follow addendum 9.13 (the UI is built against this mock).
import test, { before, after } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { server } from '../dev/mock-server.mjs';
import { buildSeries, parseSeriesQuery, WINDOW_SPAN_S, MAX_SERIES_PER_KEY, KEY_RE } from '../dev/mock-series.mjs';
import { SYSTEM_IDS } from '../js/systems.js';
import { setMessages } from '../js/i18n.js';
import { normalizeAnswer, buildLines, rateSegments, splitSegments } from '../js/series-model.js';
import { shapeRuns, resolveNumbers, compareRuns, trendGroups, enumValues, valueOf } from '../js/runs.js';
import { RUN_DESCRIPTORS } from '../js/run-descriptors.js';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
setMessages(JSON.parse(fs.readFileSync(path.join(root, 'i18n/en.json'), 'utf8')));
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
const dev = (u) => fetch(`${base}${u}`, { headers: { 'X-Console': '1' } });
const series = async (id, q) => (await get(`/systems/${id}/metrics/series?${q}`)).json();

test('invalid queries are 422 invalid_request with a message: missing / empty / malformed key, more than 12 keys, bad window, bad split', async () => {
  const bad = [
    'window=1h',
    'keys=',
    'keys=Bad.Key',
    'keys=dcgm.gpu_util,',
    `keys=${Array.from({ length: 13 }, (_, i) => `k.m${i}`).join(',')}`,
    'keys=dcgm.gpu_util&window=2h',
    'keys=dcgm.gpu_util&window=30d',
    'keys=dcgm.gpu_util&split=2',
    'keys=dcgm.gpu_util&split=true',
    `keys=${'a'.repeat(70)}`,
  ];
  for (const q of bad) {
    const r = await get(`/systems/dcgm/metrics/series?${q}`);
    assert.equal(r.status, 422, q);
    const b = await r.json();
    assert.equal(b.error.code, 'invalid_request', q);
    assert.ok(b.error.message, q);
  }
  assert.equal((await get('/systems/nope/metrics/series?keys=a.b')).status, 404, 'an unknown system');
  const twelve = Array.from({ length: 12 }, (_, i) => `k.m${i}`).join(',');
  assert.equal((await get(`/systems/dcgm/metrics/series?keys=${twelve}`)).status, 200, 'exactly 12 keys is fine');
});

test('the answer has the documented shape for every window: step, from/to, time-ordered [ts, avg, max] points, max never below the average', async () => {
  const steps = { '15m': 2, '1h': 60, '24h': 60, '7d': 600 };
  for (const w of Object.keys(WINDOW_SPAN_S)) {
    const a = await series('dcgm', `keys=dcgm.gpu_util&window=${w}&split=1`);
    assert.deepEqual(Object.keys(a).sort(), ['from', 'series', 'step_s', 'to', 'truncated', 'window']);
    assert.equal(a.window, w);
    assert.equal(a.step_s, steps[w], w);
    assert.equal(a.to - a.from + a.step_s, WINDOW_SPAN_S[w], 'the range covers exactly the window');
    assert.equal(a.truncated, false);
    assert.equal(a.series.length, 2, 'two GPUs, one line each');
    for (const s of a.series) {
      assert.equal(s.key, 'dcgm.gpu_util');
      assert.ok(s.label && typeof s.label === 'object' && s.label.gpu && s.label.model);
      assert.ok(s.points.length > 10);
      for (let i = 0; i < s.points.length; i++) {
        const [ts, avg, max] = s.points[i];
        assert.ok(Number.isFinite(ts) && Number.isFinite(avg) && Number.isFinite(max));
        assert.ok(ts >= a.from && ts <= a.to);
        if (i) assert.ok(ts > s.points[i - 1][0], 'time order');
        assert.ok(max >= avg - 1e-9, 'max >= avg');
        if (w === '15m') assert.equal(max, avg, 'the raw points carry the value as their max');
      }
    }
  }
  const week = await series('dcgm', 'keys=dcgm.gpu_util&window=7d');
  assert.ok(week.series[0].points.length > 900 && week.series[0].points.length <= 1008, '7d is ten-minute buckets');
});

test('default window is 1h and split=0 sums the label variants into one unlabelled series per key', async () => {
  const a = await series('dcgm', 'keys=dcgm.gpu_util,dcgm.power_w');
  assert.equal(a.window, '1h');
  assert.deepEqual(
    a.series.map((s) => [s.key, s.label]),
    [
      ['dcgm.gpu_util', null],
      ['dcgm.power_w', null],
    ],
  );
  const split = await series('dcgm', 'keys=dcgm.power_w&split=1');
  const t0 = a.series[1].points[10][0];
  const sum = split.series.reduce((acc, s) => acc + s.points.find((p) => p[0] === t0)[1], 0);
  assert.ok(Math.abs(sum - a.series[1].points[10][1]) < 1e-3, 'the unsplit series is the sum of the split ones');
});

test('a label selector picks one variant; an unknown key is a series with no points, never an error', async () => {
  const one = await series('dcgm', `keys=${encodeURIComponent('dcgm.gpu_util{gpu=1}')}&split=1`);
  assert.equal(one.series.length, 1);
  assert.equal(one.series[0].label.gpu, '1');
  const none = await series('dcgm', 'keys=dcgm.no_such_key&window=24h&split=1');
  assert.deepEqual(none.series, [{ key: 'dcgm.no_such_key', label: null, points: [] }]);
});

test('the series picker has something to pick: /__dev/seven?dcgm_gpus=6 gives six lines for one key', async () => {
  await dev('/__dev/seven?dcgm_gpus=6');
  const a = await series('dcgm', 'keys=dcgm.gpu_util&split=1');
  assert.equal(a.series.length, 6);
  assert.deepEqual(
    a.series.map((s) => s.label.gpu),
    ['0', '1', '2', '3', '4', '5'],
    'ordered by label',
  );
  await dev('/__dev/seven?dcgm_gpus=2');
  assert.equal((await series('dcgm', 'keys=dcgm.gpu_util&split=1')).series.length, 2);
});

test('more than 32 variants of a key: the rest are dropped and truncated is true', () => {
  const points = Array.from({ length: 40 }, (_, i) => ({ key: 'x.m', value: 1 + i, unit: '', labels: { id: String(100 + i) } }));
  const a = buildSeries({ keys: ['x.m'], window: '1h', split: true, points, now: 1.8e9 });
  assert.equal(a.series.length, MAX_SERIES_PER_KEY);
  assert.equal(a.truncated, true);
  assert.equal(buildSeries({ keys: ['x.m'], window: '1h', split: false, points, now: 1.8e9 }).truncated, false, 'unsplit is one series');
});

test('every system answers for its own keys in every window, split or not', async () => {
  for (const id of SYSTEM_IDS) {
    const m = await (await get(`/systems/${id}/metrics`)).json();
    if (!m.available) continue; // dynamo may be down in the mock: its series are empty, tested below
    const keys = [...new Set(m.points.map((p) => p.key))].slice(0, 4);
    for (const split of [0, 1]) {
      const a = await series(id, `keys=${keys.join(',')}&window=24h&split=${split}`);
      assert.ok(a.series.length >= keys.length, id);
      assert.ok(
        a.series.every((s) => s.points.length > 0),
        `${id}: every own key has points`,
      );
    }
  }
  for (const id of SYSTEM_IDS) assert.equal((await get(`/systems/${id}/metrics/series?keys=a.b&window=7d`)).status, 200, id);
});

test('deterministic: two requests agree on every shared timestamp (a chart does not jump when it refreshes)', async () => {
  const a = await series('flare', 'keys=flare.generated_total,flare.active_requests&window=1h&split=1');
  const b = await series('flare', 'keys=flare.generated_total,flare.active_requests&window=1h&split=1');
  for (let i = 0; i < a.series.length; i++) {
    const m = new Map(b.series[i].points.map((p) => [p[0], p]));
    let shared = 0;
    for (const p of a.series[i].points) {
      const q = m.get(p[0]);
      if (!q) continue;
      shared++;
      assert.deepEqual(q, p);
    }
    assert.ok(shared > 50);
  }
});

test('counters: monotonic except at restarts (so the rate logic has resets to handle), and the 24h flare series has a gap', async () => {
  const a = await series('flare', 'keys=flare.generated_total&window=24h&split=1');
  const pts = a.series[0].points;
  const drops = pts.filter((p, i) => i && p[1] < pts[i - 1][1]);
  assert.ok(drops.length >= 1, 'a counter reset inside 24h');
  assert.ok(pts.every((p) => p[1] >= 0));
  const segs = rateSegments(normalizeAnswer(a)[0].points, a.step_s);
  assert.ok(segs.length >= 2, 'resets and the gap split the rate into segments');
  assert.ok(
    segs.flat().every((p) => p[1] >= 0),
    'no negative rate',
  );
  assert.ok(splitSegments(normalizeAnswer(a)[0].points, a.step_s).length >= 2, 'the hole is a gap, not interpolated');
  const slow = await series('flare', 'keys=flare.requests_completed_total&window=1h&split=1');
  const built = buildLines(normalizeAnswer(slow), { rate: true, stepS: slow.step_s });
  assert.equal(built.rate.per, 'min', 'a slow counter is shown per minute');
  const fast = await series('flare', 'keys=flare.generated_total&window=1h&split=1');
  assert.equal(buildLines(normalizeAnswer(fast), { rate: true, stepS: fast.step_s }).rate.per, 's');
});

test('a viewer may read series (it is a viewer route)', async () => {
  await dev('/__dev/role?set=viewer');
  assert.equal((await get('/systems/dcgm/metrics/series?keys=dcgm.gpu_util')).status, 200);
  await dev('/__dev/role?set=admin');
});

test('parseSeriesQuery and KEY_RE agree with the backend rules', () => {
  const q = parseSeriesQuery(new URLSearchParams('keys=a.b,c.d{x=1,y=2}&window=7d&split=1'));
  assert.deepEqual(q, { keys: ['a.b', 'c.d{x=1,y=2}'], window: '7d', split: true });
  assert.ok(KEY_RE.test('sy.model.calls{model=x}'));
  assert.ok(!KEY_RE.test('Sy.x'));
  assert.ok(!KEY_RE.test('a b'));
});

// ---- run events

const KINDS = {
  aiperf: 'aiperf.run',
  evaluator: 'evaluator.run',
  gym: 'gym.smoke',
  modelopt: 'modelopt.job',
  skillevaluator: 'skilleval.run',
  datadesigner: 'dd.run',
  dcgm: 'dcgm.diag',
  guardrails: 'guardrails.check',
};

test('run events: 10-30 runs of the real kind per run-based system, several models / presets, a few failures, metadata only', async () => {
  for (const [id, kind] of Object.entries(KINDS)) {
    const evs = await (await get(`/systems/${id}/events?limit=500`)).json();
    const runs = evs.filter((e) => e.kind === kind);
    assert.ok(runs.length >= 10 && runs.length <= 30, `${id}: ${runs.length} runs`);
    assert.ok(
      runs.every((e) => e.system === id && typeof e.ts === 'number' && e.data && typeof e.summary === 'string'),
      id,
    );
    const desc = RUN_DESCRIPTORS[id];
    const rows = shapeRuns(evs, desc);
    assert.equal(rows.length, runs.length, `${id}: every run event becomes a row`);
    assert.equal(new Set(rows.map((r) => r.id)).size, rows.length, `${id}: ids are unique`);
    assert.ok(
      rows.some((r) => r.vclass === 'bad'),
      `${id}: at least one failed run`,
    );
    assert.ok(
      rows.some((r) => r.vclass === 'ok'),
      `${id}: and good ones`,
    );
    const seen = enumValues(rows, desc.groupBy);
    assert.ok(
      Object.values(seen).some((v) => v.length >= 2),
      `${id}: more than one value to group by`,
    );
    const numbers = resolveNumbers(rows, desc);
    assert.ok(numbers.length >= 3, `${id}: ${numbers.length} numbers to chart`);
    // a failed run has no numbers, never zeros
    const failed = rows.find((r) => r.vclass === 'bad');
    const withVal = numbers.filter(
      (n) =>
        valueOf(failed, n) != null &&
        !['duration_ms', 'concurrency', 'requests', 'isl', 'osl', 'limit', 'num_records_requested', 'tests_total', 'probes_total'].includes(n.field) &&
        !n.field.startsWith('probe') &&
        !n.field.startsWith('test:'),
    );
    assert.ok(withVal.length <= 3, `${id}: a failed run carries few numbers (${withVal.map((n) => n.field)})`);
    // one A/B compare with a real difference
    const good = rows.filter((r) => r.vclass === 'ok');
    const cmp = compareRuns(good[0], good.at(-1), numbers);
    assert.ok(
      cmp.some((c) => c.diff != null && c.mark !== 'na'),
      `${id}: two good runs can be compared`,
    );
    assert.ok(
      trendGroups(rows, numbers[0], desc.groupBy[0]).some((g) => g.points.length >= 2),
      `${id}: a trend with a line`,
    );
    for (const e of runs) assert.ok(!/prompt|completion|password|secret|api[_-]?key/i.test(JSON.stringify(e.data)), `${id}: metadata only`);
  }
});

test('run events come newest first or in time order the UI can sort; the limit is honoured', async () => {
  const some = await (await get('/systems/aiperf/events?limit=5')).json();
  assert.equal(some.length, 5);
  const all = await (await get('/systems/aiperf/events?limit=500')).json();
  assert.ok(all.length > 5);
  const ts = all.map((e) => e.ts);
  assert.deepEqual(
    ts,
    [...ts].sort((a, b) => b - a),
    'newest first like the backend',
  );
});

test('data designer history and the Runs tab agree: every dd.run event has a run behind it', async () => {
  const evs = (await (await get('/systems/datadesigner/events?limit=500')).json()).filter((e) => e.kind === 'dd.run');
  const create = evs.find((e) => e.data.mode === 'create' && e.data.verdict === 'succeeded');
  assert.ok(create);
  for (const k of ['run_id', 'script', 'mode', 'verdict', 'num_records_requested', 'dataset_name', 'destination', 'format', 'batches', 'duration_ms', 'num_records']) assert.ok(k in create.data, k);
  assert.equal((await get(`/systems/datadesigner/runs/${encodeURIComponent(create.data.run_id)}/files`)).status, 200);
});

test('guardrails live events use the adapter shape, so the history can read them', async () => {
  const evs = await (await get('/systems/guardrails/events?limit=500')).json();
  for (const e of evs.filter((x) => x.kind === 'guardrails.check')) {
    assert.equal(typeof e.data.config, 'string');
    assert.equal(typeof e.data.ok, 'boolean');
    assert.equal(e.data.results.length >= 1 && e.data.results.length <= 3, true);
    for (const r of e.data.results) assert.deepEqual(Object.keys(r).sort(), ['ms', 'probe', 'rail', 'status']);
  }
});
