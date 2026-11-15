// The GPU telemetry shaping (addendum 9.14): fleet matrix with gaps, energy from a counter with a reset, violation shares, the really-busy verdict,
// memory, remapped rows and the timeline. Missing values are null, never 0.
import test from 'node:test';
import assert from 'node:assert/strict';
import {
  shapeFleet,
  fleetDomain,
  fleetFormat,
  fleetMetric,
  fleetWindow,
  hostsFromSnapshot,
  groupByGpu,
  busyVerdict,
  busyModel,
  memoryModel,
  powerModel,
  energyModel,
  energyTotal,
  violationModel,
  remapModel,
  timelineModel,
  diagTests,
} from '../js/dcgm-model.js';
import { normalizeAnswer } from '../js/series-model.js';

const row = (host, gpu, points, extra = {}) => ({ host, job: `dcgm-${host}`, gpu, model: 'NVIDIA X', points, ...extra });
const answer = (rows, over = {}) => ({ metric: 'util', unit: 'ratio', window: '1h', step_s: 60, from: 1000, to: 1000 + 5 * 60, rows, truncated: false, ...over });
const GIB = 1073741824;

test('shapeFleet: points land on one grid, a missing slot is a gap (null), rows are ordered by host then numeric GPU', () => {
  const m = shapeFleet(
    answer([
      row('host-b', '0', [
        [1000, 0.5],
        [1060, 0.6],
        [1240, 0.9],
      ]),
      row('host-a', '10', [[1000, 0.1]]),
      row('host-a', '2', [
        [1000, 0.2],
        [1300, 0.3],
      ]),
    ]),
  );
  assert.deepEqual(
    m.rows.map((r) => `${r.host}/${r.gpu}`),
    ['host-a/2', 'host-a/10', 'host-b/0'],
  );
  assert.equal(m.cols, 6);
  assert.deepEqual(m.rows[2].values, [0.5, 0.6, null, null, 0.9, null]);
  assert.deepEqual(m.rows[0].values, [0.2, null, null, null, null, 0.3]);
  assert.equal(m.empty, false);
  assert.equal(m.problem, null);
  assert.equal(m.binS, 60);
  assert.deepEqual(m.rows[2].stats, { n: 3, latest: 0.9, min: 0.5, avg: (0.5 + 0.6 + 0.9) / 3, max: 0.9 });
});

test('shapeFleet merges columns to fit (the mean of what is there; a column with no sample stays a gap) but keeps honest stats', () => {
  const pts = Array.from({ length: 10 }, (_, i) => [1000 + i * 60, i % 2 ? 1 : 0]).filter((p) => p[0] !== 1120 && p[0] !== 1180);
  const m = shapeFleet(answer([row('h', '0', pts)], { from: 1000, to: 1000 + 9 * 60 }), { maxCols: 5 });
  assert.equal(m.cols, 5);
  assert.equal(m.binS, 120);
  assert.deepEqual(m.rows[0].values, [0.5, null, 0.5, 0.5, 0.5], 'the pair with both samples gone is a gap');
  assert.equal(m.rows[0].stats.n, 8, 'min / avg / max come from the unmerged samples');
  assert.equal(m.rows[0].raw.length, 10);
  assert.equal(m.ts[1], 1120);
});

test('shapeFleet: a host that is down and has no history gets a row of gaps; a down host with history keeps it and is flagged', () => {
  const m = shapeFleet(answer([row('host-a', '0', [[1000, 0.4]]), row('host-d', '0', [[1000, 0.2]])]), {
    hostsUp: [
      { host: 'host-a', job: 'dcgm', up: true },
      { host: 'host-d', job: 'dcgm-d', up: false },
      { host: 'host-e', job: 'dcgm-e', up: false },
      { host: 'host-f', job: 'dcgm-f', up: true },
    ],
  });
  const by = Object.fromEntries(m.rows.map((r) => [r.host, r]));
  assert.equal(by['host-a'].down, false);
  assert.equal(by['host-d'].down, true);
  assert.equal(by['host-d'].values[0], 0.2, 'its history is kept');
  assert.equal(by['host-e'].missing, true);
  assert.ok(
    by['host-e'].values.every((v) => v === null),
    'never 0',
  );
  assert.equal(by['host-e'].gpu, null);
  assert.ok(!by['host-f'], 'an up host with no rows is not invented');
  assert.equal(m.empty, false);
});

test('shapeFleet: empty rows, Prometheus down (a problem), a missing grid, junk input and string values do not throw', () => {
  const empty = shapeFleet(answer([]));
  assert.equal(empty.empty, true);
  assert.deepEqual(empty.rows, []);
  const down = shapeFleet(answer([], { problem: { code: 'dcgm.prometheus.unreachable', message: 'no route' } }));
  assert.deepEqual(down.problem, { code: 'dcgm.prometheus.unreachable', message: 'no route' });
  for (const junk of [null, undefined, 5, 'x', {}, { rows: 'no' }, { rows: [null, 3, { points: 'x' }] }]) {
    const m = shapeFleet(junk);
    assert.equal(m.empty, true);
    assert.ok(Array.isArray(m.rows));
  }
  const noGrid = shapeFleet({
    rows: [
      row('h', '0', [
        [100, 1],
        [160, 2],
      ]),
    ],
    step_s: 60,
  });
  assert.deepEqual(noGrid.rows[0].values, [1, 2], 'the grid is taken from the points when from / to are missing');
  const str = shapeFleet(
    answer([
      row('h', '0', [
        [1000, '0.5'],
        [1060, 'x'],
        [1120, null],
      ]),
    ]),
  );
  assert.deepEqual(str.rows[0].values.slice(0, 3), [0.5, null, null]);
  assert.equal(shapeFleet(answer([row('h', '0', [])], { truncated: true })).truncated, true);
});

test('fleetDomain / fleetFormat: util is a ratio or a percentage by the unit, temperature is fixed, power and memory round up above the data', () => {
  assert.deepEqual(fleetDomain('util', 'ratio', []), { min: 0, max: 1, curve: 'linear' });
  assert.equal(fleetDomain('util', '%', []).max, 100);
  assert.deepEqual(fleetDomain('temp', 'C', [{ values: [99] }]), { min: 30, max: 90, curve: 'linear' });
  assert.equal(fleetDomain('power', 'W', [{ raw: [null, 312, 280] }]).max, 400);
  assert.equal(fleetDomain('power', 'W', []).max, 100, 'a floor, so an idle fleet is not stretched');
  assert.equal(fleetDomain('fb_used', 'B', [{ raw: [60 * GIB] }]).max, 60 * GIB, 'the top is at or above the data, on a round number of GiB');
  assert.equal(fleetDomain('fb_used', 'MiB', [{ raw: [61440] }]).max, 60 * 1024);
  assert.equal(fleetDomain('fb_used', 'B', [{ raw: [86 * GIB, 12 * GIB] }]).max, 100 * GIB);
  assert.equal(fleetDomain('fb_used', 'B', []).max, 1);
  assert.equal(fleetFormat('util', 'ratio')(0.714), '71%');
  assert.equal(fleetFormat('util', '%')(71.4), '71%');
  assert.equal(fleetFormat('temp', 'C')(63.4), '63 °C');
  assert.equal(fleetFormat('power', 'W')(285.2), '285 W');
  assert.equal(fleetFormat('fb_used', 'B')(60 * GIB), '60.0 GiB');
  assert.equal(fleetFormat('fb_used', 'MiB')(2048), '2.0 GiB');
  assert.equal(fleetFormat('fb_used', 'GiB')(12.34), '12.3 GiB');
});

test('fleetMetric / fleetWindow accept only the contract values', () => {
  assert.equal(fleetMetric('temp'), 'temp');
  assert.equal(fleetMetric('bogus'), 'util');
  assert.equal(fleetMetric(''), 'util');
  assert.equal(fleetWindow('7d'), '7d');
  assert.equal(fleetWindow('15m'), '24h');
});

test('hostsFromSnapshot: one entry per host from dcgm.host_up; a host with no value is neither up nor down', () => {
  const hs = hostsFromSnapshot([
    { key: 'dcgm.host_up', value: 1, labels: { host: 'a', job: 'j1' } },
    { key: 'dcgm.host_up', value: 0, labels: { host: 'b', job: 'j2' } },
    { key: 'dcgm.host_up', value: 'x', labels: { host: 'c', job: 'j3' } },
    { key: 'dcgm.fleet_gpus', value: 2, labels: { host: 'a', job: 'j1' } },
    { key: 'dcgm.host_up', value: 1, labels: {} },
  ]);
  assert.deepEqual(hs, [
    { host: 'a', job: 'j1', up: true },
    { host: 'b', job: 'j2', up: false },
    { host: 'c', job: 'j3', up: null },
  ]);
});

// ---------------------------------------------------------------------------------------------- the Status cards
const L = (gpu, extra = {}) => ({ gpu, model: `NVIDIA ${gpu}`, ...extra });
const ser = (key, label, points) => ({ key, label, points });
const grid = (n, f, t0 = 0, step = 60) => Array.from({ length: n }, (_, i) => [t0 + i * step, f(i)]);
const ans = (series, over = {}) => ({ window: '1h', step_s: 60, from: 0, to: 600, truncated: false, series, ...over });

test('groupByGpu: per-GPU groups in numeric order; a series with no gpu label is not per-GPU', () => {
  const g = groupByGpu(
    ans([
      ser('dcgm.gpu_util', L('10'), []),
      ser('dcgm.gpu_util', L('2'), []),
      ser('dcgm.host_up', { host: 'a' }, []),
      ser('dcgm.violation_us_total', L('2', { reason: 'power' }), []),
      ser('dcgm.violation_us_total', L('2', { reason: 'thermal' }), []),
    ]),
  );
  assert.deepEqual(
    g.map((x) => x.gpu),
    ['2', '10'],
  );
  assert.equal(g[0].byKey.get('dcgm.violation_us_total').length, 2);
  assert.equal(g[0].model, 'NVIDIA 2');
});

test('busyVerdict: utilisation alone can mislead; the engines say how much of the chip worked', () => {
  assert.equal(busyVerdict({ util: null, gr: 0.9 }), 'unknown');
  assert.equal(busyVerdict({}), 'unknown');
  assert.equal(busyVerdict({ util: 0.05, gr: 0.9 }), 'idle');
  assert.equal(busyVerdict({ util: 0.95, gr: 0.1, tensor: 0.05, dram: 0.2 }), 'misleading');
  assert.equal(busyVerdict({ util: 0.95, gr: 0.1, tensor: 0.6, dram: 0.2 }), 'busy');
  assert.equal(busyVerdict({ util: 0.4, gr: 0.3, tensor: 0.1 }), 'light');
  assert.equal(busyVerdict({ util: 0.9 }), 'util_only');
  assert.equal(busyVerdict({ util: 0.9, gr: null, tensor: null, dram: null }), 'util_only', 'no profiling fields is not "no activity"');
  assert.equal(busyVerdict({ util: 0.5, dram: 0.24 }), 'misleading');
  assert.equal(busyVerdict({ util: 0.49, dram: 0.24 }), 'light');
});

test('busyModel: window means, the newest SM clock, a missing key is null (the encoder row is simply absent)', () => {
  const [g] = groupByGpu(
    ans([
      ser(
        'dcgm.gpu_util',
        L('0'),
        grid(4, (i) => [0.2, 0.4, 0.6, 0.8][i]),
      ),
      ser(
        'dcgm.gr_active',
        L('0'),
        grid(2, () => 0.1),
      ),
      ser(
        'dcgm.sm_clock_mhz',
        L('0'),
        grid(3, (i) => 1800 + i),
      ),
    ]),
  );
  const m = busyModel(g);
  assert.ok(Math.abs(m.util - 0.5) < 1e-9);
  assert.equal(m.gr, 0.1);
  assert.equal(m.tensor, null);
  assert.equal(m.smClock, 1802);
  assert.equal(m.encoder, null);
  assert.equal(m.verdict, 'misleading', 'util 50% with graphics 10% and no other engine above 25%');
});

test('memoryModel: used / reserved / free in GiB on one grid, the capacity only when all three are there, a missing key is null (not 0)', () => {
  const a = ans([
    ser(
      'dcgm.fb_used_b',
      L('0'),
      grid(11, () => 60 * GIB),
    ),
    ser(
      'dcgm.fb_free_b',
      L('0'),
      grid(11, () => 35 * GIB),
    ),
    ser(
      'dcgm.fb_reserved_b',
      L('0'),
      grid(11, () => 1 * GIB),
    ),
  ]);
  const [g] = groupByGpu(a);
  const m = memoryModel(g, { from: 0, to: 600, stepS: 60 });
  assert.deepEqual(m.latest, { used: 60, reserved: 1, free: 35 });
  assert.equal(m.total, 96);
  assert.equal(m.series.length, 3);
  assert.equal(m.ts.length, 11);
  const [g2] = groupByGpu(
    ans([
      ser(
        'dcgm.fb_used_b',
        L('1'),
        grid(11, () => 10 * GIB),
      ),
      ser(
        'dcgm.fb_free_b',
        L('1'),
        grid(11, () => 6 * GIB),
      ),
    ]),
  );
  const m2 = memoryModel(g2, { from: 0, to: 600, stepS: 60 });
  assert.equal(m2.latest.reserved, null);
  assert.equal(m2.total, null, 'the capacity is not guessed from two parts');
  assert.deepEqual(
    m2.series.map((s) => s.key),
    ['used', 'free'],
  );
  const [g3] = groupByGpu(ans([ser('dcgm.gpu_util', L('2'), [])]));
  assert.equal(memoryModel(g3, { from: 0, to: 600, stepS: 60 }).any, false);
});

test('memoryModel merges a long window into at most maxCols columns', () => {
  const [g] = groupByGpu(
    ans(
      [
        ser(
          'dcgm.fb_used_b',
          L('0'),
          grid(1008, () => 4 * GIB, 0, 600),
        ),
      ],
      { from: 0, to: 1007 * 600, step_s: 600 },
    ),
  );
  const m = memoryModel(g, { from: 0, to: 1007 * 600, stepS: 600 }, 120);
  assert.ok(m.ts.length <= 120);
  assert.equal(m.bucketS, 600 * 9);
});

test('powerModel: the limit and the headroom; within 5% of the limit is flagged; without a limit the field is null', () => {
  const [g] = groupByGpu(
    ans([
      ser(
        'dcgm.power_w',
        L('0'),
        grid(5, (i) => 250 + i * 10),
      ),
      ser(
        'dcgm.power_limit_w',
        L('0'),
        grid(5, () => 300),
      ),
    ]),
  );
  const m = powerModel(g, 60);
  assert.equal(m.limitW, 300);
  assert.equal(m.peak, 290);
  assert.equal(m.nearLimit, true);
  assert.equal(m.latest, 290);
  const [g2] = groupByGpu(
    ans([
      ser(
        'dcgm.power_w',
        L('1'),
        grid(5, () => 100),
      ),
    ]),
  );
  const m2 = powerModel(g2, 60);
  assert.equal(m2.limitW, null);
  assert.equal(m2.nearLimit, false);
  assert.equal(m2.has, true);
  assert.equal(powerModel(groupByGpu(ans([ser('dcgm.gpu_util', L('2'), [])]))[0], 60).has, false);
});

test('energyModel: the increase of the counter over the window in kWh and the average power it implies', () => {
  // 300 W for one hour: 300 * 3600 * 1000 mJ = 1.08e9 mJ = 0.3 kWh
  const pts = grid(61, (i) => 5e9 + i * 60 * 300_000);
  const [g] = groupByGpu(ans([ser('dcgm.energy_mj_total', L('0'), pts)]));
  const e = energyModel(g);
  assert.ok(Math.abs(e.kwh - 0.3) < 1e-9);
  assert.ok(Math.abs(e.wh - 300) < 1e-6);
  assert.ok(Math.abs(e.avgW - 300) < 1e-6);
  assert.equal(e.resets, 0);
  assert.equal(e.seconds, 3600);
});

test('energyModel: a counter reset counts the new value and says so; no counter or one sample is null, never 0 kWh', () => {
  const pts = [
    [0, 1000],
    [60, 3000],
    [120, 500],
    [180, 1500],
  ];
  const [g] = groupByGpu(ans([ser('dcgm.energy_mj_total', L('0'), pts)]));
  const e = energyModel(g);
  assert.equal(e.mj, 2000 + 500 + 1000);
  assert.equal(e.resets, 1);
  const [one] = groupByGpu(ans([ser('dcgm.energy_mj_total', L('1'), [[0, 5]])]));
  assert.equal(energyModel(one), null);
  const [none] = groupByGpu(ans([ser('dcgm.gpu_util', L('2'), [])]));
  assert.equal(energyModel(none), null);
});

test('energyTotal adds the GPUs that have a figure; none is null', () => {
  const mk = (mj, seconds, resets = 0) => ({ mj, seconds, resets });
  const t = energyTotal([mk(3.6e9, 3600), null, mk(1.8e9, 3600, 1)]);
  assert.ok(Math.abs(t.kwh - 1.5) < 1e-9);
  assert.equal(t.resets, 1);
  assert.equal(t.gpus, 2);
  assert.ok(Math.abs(t.avgW - 1500) < 1e-6);
  assert.equal(energyTotal([null]), null);
  assert.equal(energyTotal([]), null);
});

test('violationModel: microseconds of throttling become a share of time; reasons in the contract order; quiet when none throttled', () => {
  // power: 120 000 us per second = 12% of the time; thermal flat
  const power = grid(11, (i) => i * 60 * 120_000);
  const thermal = grid(11, () => 500);
  const [g] = groupByGpu(ans([ser('dcgm.violation_us_total', L('0', { reason: 'thermal' }), thermal), ser('dcgm.violation_us_total', L('0', { reason: 'power' }), power)]));
  const v = violationModel(g, 60);
  assert.deepEqual(
    v.rows.map((r) => r.reason),
    ['power', 'thermal'],
  );
  assert.ok(Math.abs(v.rows[0].pct - 12) < 1e-9);
  assert.ok(Math.abs(v.rows[0].seconds - 72) < 1e-9);
  assert.ok(Math.abs(v.rows[0].segments[0][0][1] - 12) < 1e-9, 'the line is in % of time');
  assert.equal(v.rows[1].pct, 0);
  assert.equal(v.quiet, false);
  const [q] = groupByGpu(ans([ser('dcgm.violation_us_total', L('1', { reason: 'power' }), thermal)]));
  assert.equal(violationModel(q, 60).quiet, true);
  const [x] = groupByGpu(ans([ser('dcgm.gpu_util', L('2'), [])]));
  const none = violationModel(x, 60);
  assert.equal(none.any, false);
  assert.equal(none.quiet, false, 'no counters is not "no throttling"');
});

test('violationModel: a counter reset starts a new segment (no spike) and the share counts the new value', () => {
  const pts = [
    [0, 0],
    [60, 6e6],
    [120, 1e6],
    [180, 7e6],
  ];
  const [g] = groupByGpu(ans([ser('dcgm.violation_us_total', L('0', { reason: 'power' }), pts)]));
  const r = violationModel(g, 60).rows[0];
  assert.equal(r.resets, 1);
  assert.equal(r.segments.length, 2, 'the reset splits the line');
  assert.ok(r.segments.flat().every((p) => p[1] <= 100 && p[1] >= 0));
  assert.ok(r.pct > 0 && r.pct <= 100);
});

test('remapModel: per GPU, null for a figure the exporter does not report; failure is a boolean', () => {
  const m = remapModel([
    { key: 'dcgm.remapped_rows', value: 2, labels: { gpu: '0', model: 'X', kind: 'correctable' } },
    { key: 'dcgm.remapped_rows', value: 0, labels: { gpu: '0', model: 'X', kind: 'uncorrectable' } },
    { key: 'dcgm.row_remap_failure', value: 1, labels: { gpu: '0', model: 'X' } },
    { key: 'dcgm.remapped_rows', value: 1, labels: { gpu: '1', model: 'Y', kind: 'correctable' } },
    { key: 'dcgm.remapped_rows', value: 1, labels: { gpu: '2', kind: 'other' } },
    { key: 'dcgm.gpu_util', value: 1, labels: { gpu: '3' } },
  ]);
  assert.deepEqual(m, [
    { gpu: '0', model: 'X', correctable: 2, uncorrectable: 0, failure: true },
    { gpu: '1', model: 'Y', correctable: 1, uncorrectable: null, failure: null },
  ]);
  assert.deepEqual(remapModel([]), []);
  assert.deepEqual(remapModel(undefined), []);
});

test('timelineModel: XID and diagnostic events inside the window, a lane per GPU in numeric order, severities, stable ids, "?" for no GPU', () => {
  const ev = [
    { ts: 900, kind: 'dcgm.xid', summary: 'GPU 1: XID 31', data: { gpu: '1', xid: 31 } },
    { ts: 800, kind: 'dcgm.diag', summary: 'diag', data: { gpu: 10, verdict: 'pass', level: 1 } },
    { ts: 700, kind: 'dcgm.diag', summary: 'diag', data: { gpu: 2, verdict: 'fail' } },
    { ts: 600, kind: 'dcgm.diag', summary: 'diag', data: { gpu: 2, verdict: 'warn' } },
    { ts: 500, kind: 'dcgm.diag', summary: 'diag', data: { verdict: 'error' } },
    { ts: 400, kind: 'dcgm.diag', summary: 'diag', data: { gpu: 2, verdict: 'mystery' } },
    { ts: 100, kind: 'dcgm.xid', summary: 'old', data: { gpu: '1' } },
    { ts: 950, kind: 'dcgm.other', summary: 'ignored', data: {} },
    { ts: 950, kind: 'dcgm.xid', summary: 'same instant', data: { gpu: '1' } },
    { ts: 950, kind: 'dcgm.xid', summary: 'same instant', data: { gpu: '1' } },
    null,
  ];
  const m = timelineModel(ev, { from: 300, to: 1000 });
  assert.deepEqual(m.lanes, ['1', '2', '10', '?']);
  assert.equal(m.outside, 1);
  assert.deepEqual(
    m.items.map((i) => i.ts),
    [400, 500, 600, 700, 800, 900, 950, 950],
  );
  assert.deepEqual(
    m.items.map((i) => i.severity),
    ['muted', 'bad', 'warn', 'bad', 'ok', 'bad', 'bad', 'bad'],
  );
  assert.equal(new Set(m.items.map((i) => i.id)).size, m.items.length, 'two events at the same instant keep distinct ids');
  const again = timelineModel([{ ts: 1, kind: 'dcgm.xid', summary: '', data: { gpu: '0' } }, ...ev], { from: 300, to: 1000 });
  assert.equal(again.items.find((i) => i.ts === 900).id, m.items.find((i) => i.ts === 900).id, 'a new event elsewhere does not rename this one');
  assert.deepEqual(timelineModel([], { from: 0, to: 1 }), { items: [], lanes: [], outside: 0 });
});

test('diagTests: failures first, then warnings, skips and passes', () => {
  const r = diagTests({ tests: { Memory: 'Pass', PCIe: 'Fail', Inforom: 'Warn', Graphics: 'Skip', Odd: 'what' } });
  assert.deepEqual(
    r.map((x) => x.name),
    ['PCIe', 'Inforom', 'Graphics', 'Memory', 'Odd'],
  );
  assert.deepEqual(diagTests({}), []);
  assert.deepEqual(diagTests(null), []);
});

test('the answers are plain data: normalizeAnswer + groupByGpu on an answer with gaps do not invent points', () => {
  const [g] = groupByGpu(
    normalizeAnswer(
      ans([
        ser('dcgm.gpu_util', L('0'), [
          [0, 0.1, 0.1],
          [60, NaN, NaN],
          [120, 0.3, 0.3],
        ]),
      ]),
    ),
  );
  assert.equal(busyModel(g).util, 0.2);
});
