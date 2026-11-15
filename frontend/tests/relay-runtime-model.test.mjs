// The Relay runtime cards (docs/ARCHITECTURE.md addendum 9.17): the pure model (call planning, number formats, freshness, the
// diagnostics answer), the API client's new route, the message catalogue, the mock's contract and the rules the card files obey
// (admin only, text only, no inline style, no markup from data).
import test, { before, after } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { createClient } from '../js/api.js';
import { niceScale } from '../js/viz/scale.js';
import { setMessages, t } from '../js/i18n.js';
import {
  RES_CARDS,
  RES_TILES,
  RES_WINDOWS,
  RES_DEFAULT_WINDOW,
  RESOURCE_KEYS,
  SERIES_MAX_KEYS,
  DIAG_REASONS,
  planCalls,
  cardKeys,
  resWindow,
  fmtResource,
  axisFormatter,
  axisStep,
  decimalsForStep,
  eventsTruncated,
  freshness,
  sampleAge,
  latestValues,
  hasResourceFigures,
  tileModel,
  parseStamp,
  breakParts,
  normalizeDiagnostics,
  failureBars,
  outcomeOptions,
  filterEvents,
  lossRows,
  memoryGauges,
  outcomeKind,
  outcomeGlyph,
  severityKind,
} from '../js/relay-runtime-model.js';
import { describeLevel } from '../js/rt/pure.js';
import { server } from '../dev/mock-server.mjs';
import { diagnosticsAnswer, resourcesOf, resourcePoints, resourceLevel, resourceHint, RESOURCE_MODES, DIAG_MODES } from '../dev/mock-runtime.mjs';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const en = JSON.parse(fs.readFileSync(path.join(root, 'i18n/en.json'), 'utf8'));
setMessages(en);
const read = (f) => fs.readFileSync(path.join(root, f), 'utf8');
const res = (status, body) => ({ ok: status < 400, status, headers: { get: () => 'application/json' }, json: async () => body, text: async () => '' });

// ------------------------------------------------------------------------------------------------------------------ resources
test('the series calls hold whole cards, at most 12 keys each, and cover every charted key exactly once', () => {
  const plan = planCalls();
  assert.ok(plan.length >= 2, 'more than 12 keys are charted, so more than one call');
  for (const call of plan) assert.ok(call.keys.length <= SERIES_MAX_KEYS && call.keys.length > 0);
  assert.deepEqual(
    plan.flatMap((c) => c.keys),
    RES_CARDS.flatMap(cardKeys),
  );
  assert.deepEqual(
    plan.flatMap((c) => c.cards),
    RES_CARDS.map((c) => c.id),
    'cards keep page order and are never split',
  );
  for (const call of plan) for (const id of call.cards) for (const k of cardKeys(RES_CARDS.find((c) => c.id === id))) assert.ok(call.keys.includes(k), `${id} is whole in its call`);
  assert.equal(new Set(plan.flatMap((c) => c.keys)).size, plan.flatMap((c) => c.keys).length, 'no key is asked for twice');
});
test('planCalls packs by card and refuses a card that cannot fit one call', () => {
  const cards = [
    { id: 'a', panels: [{ id: 'a', unit: 'n', lines: ['k1', 'k2', 'k3'] }] },
    { id: 'b', panels: [{ id: 'b', unit: 'n', lines: ['k4', 'k5'] }] },
    { id: 'c', panels: [{ id: 'c', unit: 'n', lines: ['k6'] }] },
  ];
  assert.deepEqual(planCalls(cards, 4), [
    { keys: ['k1', 'k2', 'k3'], cards: ['a'] },
    { keys: ['k4', 'k5', 'k6'], cards: ['b', 'c'] },
  ]);
  assert.throws(() => planCalls(cards, 2), /at most 2/);
});
test('every key the contract names is charted or shown, with the unit the contract gives', () => {
  const want = [
    'relay.gw_cpu',
    'relay.gw_rss',
    'relay.gw_mem_accounted',
    'relay.gw_threads',
    'relay.gw_fds',
    'relay.gw_disk_read',
    'relay.gw_disk_write',
    'relay.atof_fs_free',
    'relay.atof_fs_used',
    'relay.route_rps',
    'relay.route_failures',
    'relay.route_overhead_p95',
    'relay.route_tokens',
    'relay.gw_cpu_pressure',
    'relay.gw_mem_pressure',
    'relay.gw_limit_events',
  ];
  assert.deepEqual([...RESOURCE_KEYS].sort(), [...want].sort());
  const unit = Object.fromEntries(RES_CARDS.flatMap((c) => c.panels.flatMap((p) => p.lines.map((k) => [k, p.unit]))));
  assert.equal(unit['relay.gw_cpu'], 'cores');
  assert.equal(unit['relay.gw_rss'], 'B');
  assert.equal(unit['relay.gw_disk_read'], 'B/s');
  assert.equal(unit['relay.atof_fs_used'], 'ratio');
  assert.equal(unit['relay.route_rps'], 'req/s');
  assert.equal(unit['relay.route_overhead_p95'], 'ms');
  assert.equal(unit['relay.route_tokens'], 'tok/s');
  assert.equal(unit['relay.gw_cpu_pressure'], 'ms/s');
  assert.ok(!RESOURCE_KEYS.some((k) => /age/.test(k)), 'the age is not a metric key: the capture answer gives the newest sample mark');
});
test('the window is 1h, 24h or 7d and defaults to 24h', () => {
  assert.deepEqual(RES_WINDOWS, ['1h', '24h', '7d']);
  assert.equal(RES_DEFAULT_WINDOW, '24h');
  assert.equal(resWindow('7d'), '7d');
  for (const bad of ['15m', '30d', '', null, undefined, 'all']) assert.equal(resWindow(bad), '24h');
});
test('values read by their unit, and a missing value is an en dash, never a zero', () => {
  assert.equal(fmtResource('cores', 0.0427), '43 millicores');
  assert.equal(fmtResource('cores', 0.5), '0.50 cores');
  assert.equal(fmtResource('cores', 0), '0 cores');
  assert.equal(fmtResource('B', 188e6), '179.3 MiB');
  assert.equal(fmtResource('B/s', 2.1e4), '20.5 KiB/s');
  assert.equal(fmtResource('ratio', 0.37), '37%');
  assert.equal(fmtResource('ratio', 0.032), '3.2%');
  assert.equal(fmtResource('ms/s', 1.4), '1.4 ms/s');
  assert.equal(fmtResource('ms/s', 120.4), '120.4 ms/s');
  assert.equal(fmtResource('req/s', 0.0045), '0.0045 req/s');
  assert.equal(fmtResource('req/s', 12.34), '12 req/s');
  assert.equal(fmtResource('req/s', 1234.5), '1235 req/s', 'normal rounding from 100 up, not two significant digits (1200)');
  assert.equal(fmtResource('req/s', 100.4), '100 req/s');
  assert.equal(fmtResource('req/s', 99.6), '100 req/s');
  assert.equal(fmtResource('req/s', 0.00041), '0.00041 req/s', 'small values keep their digits');
  assert.equal(fmtResource('tok/s', 84.26), '84.3 tok/s');
  assert.equal(fmtResource('tok/s', 1200), '1200 tok/s');
  assert.equal(fmtResource('ms', 0.31), '0.3 ms');
  assert.equal(fmtResource('n', 31), '31');
  assert.equal(fmtResource('n', 123456), '123k');
  assert.equal(fmtResource('n', 0), '0');
  for (const u of ['cores', 'B', 'B/s', 'ratio', 'ms/s', 'req/s', 'tok/s', 'ms', 'n']) for (const v of [null, undefined, NaN, Infinity, 'x']) assert.equal(fmtResource(u, v), '–', `${u} ${String(v)}`);
});
test('a zero is a zero in every unit: "0 req/s", never an en dash and never "0.0 tok/s"', () => {
  const zero = { cores: '0 cores', B: '0 B', 'B/s': '0 B/s', ratio: '0%', 'ms/s': '0 ms/s', 'req/s': '0 req/s', 'tok/s': '0 tok/s', ms: '0 ms', n: '0' };
  for (const [unit, want] of Object.entries(zero)) assert.equal(fmtResource(unit, 0), want, unit);
  for (const unit of Object.keys(zero)) assert.equal(axisFormatter(unit, 0.5)(0).startsWith('–'), false, `${unit} axis zero`);
});
test('small values keep two significant digits instead of rounding to zero (tiles, tooltips, data tables)', () => {
  assert.equal(fmtResource('cores', 0.0005), '0.5 millicores');
  assert.equal(fmtResource('cores', 0.00051), '0.51 millicores');
  assert.equal(fmtResource('cores', 0.0123), '12 millicores');
  assert.equal(fmtResource('ms/s', 0.0005), '0.0005 ms/s');
  assert.equal(fmtResource('ms/s', 0.05), '0.05 ms/s');
  assert.equal(fmtResource('ms/s', 0.5), '0.5 ms/s');
  assert.equal(fmtResource('req/s', 0.00041), '0.00041 req/s');
  assert.equal(fmtResource('tok/s', 0.043), '0.043 tok/s');
  assert.equal(fmtResource('tok/s', 0.9), '0.9 tok/s');
  assert.equal(fmtResource('tok/s', 5), '5.0 tok/s');
  assert.equal(fmtResource('B/s', 0.5), '0.5 B/s');
  assert.equal(fmtResource('B/s', 3), '3 B/s');
  assert.equal(fmtResource('B', 0.25), '0.25 B');
  assert.equal(fmtResource('ratio', 0.0004), '0.04%');
  assert.equal(fmtResource('ratio', 0.00001), '0.001%');
  assert.equal(fmtResource('ms', 0.02), '0.02 ms');
  assert.equal(fmtResource('ms', 0.31), '0.3 ms');
  for (const unit of ['cores', 'ms/s', 'req/s', 'tok/s', 'B/s', 'B', 'ratio', 'ms'])
    for (const v of [0.00004, 0.0005, 0.007, 0.08])
      assert.ok(!/^0(\.0+)?\s/.test(fmtResource(unit, v)) && !/^0\.0+%$/.test(fmtResource(unit, v)), `${unit} ${v} -> ${fmtResource(unit, v)} is not a zero`);
});
test('axis labels adapt to the tick step: a CPU axis of 0..0.0006 cores reads 0, 0.2 m, 0.4 m, 0.6 m (never 0, 0, 0, 0)', () => {
  const ticks = niceScale(0, 0.0006, 4).ticks;
  assert.deepEqual(ticks, [0, 0.0002, 0.0004, 0.0006]);
  const step = axisStep([
    {
      segments: [
        [
          [0, 0, null],
          [1, 0.0006, null],
        ],
      ],
    },
  ]);
  assert.ok(Math.abs(step - 0.0002) < 1e-12, `the step is 0.0002 (was ${step})`);
  assert.deepEqual(ticks.map(axisFormatter('cores', step)), ['0 m', '0.2 m', '0.4 m', '0.6 m']);
  // a CPU axis that is not tiny keeps cores
  assert.deepEqual([0, 0.02, 0.04, 0.06].map(axisFormatter('cores', 0.02)), ['0 m', '20 m', '40 m', '60 m']);
  assert.deepEqual([0, 0.5, 1, 1.5].map(axisFormatter('cores', 0.5)), ['0', '0.5', '1', '1.5']);
  assert.deepEqual([0, 0.1, 0.2].map(axisFormatter('cores', 0.1)), ['0', '0.1', '0.2']);
  // the other units that can be tiny
  assert.deepEqual([0, 0.0005, 0.001].map(axisFormatter('ms/s', 0.0005)), ['0', '0.0005', '0.001']);
  assert.deepEqual([0, 0.0002, 0.0004].map(axisFormatter('req/s', 0.0002)), ['0', '0.0002', '0.0004']);
  assert.deepEqual([0, 0.2, 0.4].map(axisFormatter('tok/s', 0.2)), ['0', '0.2', '0.4']);
  assert.deepEqual([0, 0.2, 0.4].map(axisFormatter('B/s', 0.2)), ['0 B/s', '0.2 B/s', '0.4 B/s']);
  assert.deepEqual([0, 0.0002, 0.0004].map(axisFormatter('ratio', 0.0002)), ['0%', '0.02%', '0.04%']);
  assert.deepEqual([0, 0.02, 0.04].map(axisFormatter('ms', 0.02)), ['0 ms', '0.02 ms', '0.04 ms']);
  // bytes read in one unit chosen by the step, with decimals when the step is a fraction of it
  assert.deepEqual([0, 512, 1024, 1536].map(axisFormatter('B', 512)), ['0 KiB', '0.5 KiB', '1 KiB', '1.5 KiB']);
  assert.deepEqual([0, 49152, 98304].map(axisFormatter('B/s', 49152)), ['0 KiB/s', '48 KiB/s', '96 KiB/s']);
  assert.deepEqual([0, 95e6, 191e6].map(axisFormatter('B', 95e6)), ['0 MiB', '91 MiB', '182 MiB']);
  assert.equal(axisFormatter('n', 1)(100), '100');
  assert.equal(axisFormatter('n', 1)(123456), '123k');
  for (const unit of ['cores', 'ms/s', 'req/s', 'tok/s', 'B/s', 'B', 'ratio', 'ms', 'n'])
    for (const v of [null, undefined, NaN, Infinity]) assert.equal(axisFormatter(unit, 0.5)(v), '–', `${unit} ${String(v)}`);
  for (const step of [0, -1, NaN, null, undefined, 'x']) assert.equal(typeof axisFormatter('cores', step)(0.5), 'string');
});
test('on every unit, at every magnitude from 0.0006 to 6e9, the labels of a nice axis are all different', () => {
  const maxes = [0.0006, 0.006, 0.06, 0.6, 1, 6, 60, 600, 6e3, 6e4, 6e6, 6e9];
  for (const unit of ['cores', 'ms/s', 'req/s', 'tok/s', 'B/s', 'B', 'ratio', 'ms', 'n']) {
    for (const max of maxes) {
      if (unit === 'ratio' && max > 1) continue;
      const axis = niceScale(0, max, 4);
      const labels = axis.ticks.map(axisFormatter(unit, axis.step));
      assert.equal(new Set(labels).size, labels.length, `${unit} 0..${max}: ${labels.join(' | ')}`);
    }
  }
});
test('the axis step is the one the chart draws with, from every value of every line', () => {
  const line = (...vs) => ({ segments: [vs.map((v, i) => [i, v, null])] });
  assert.equal(axisStep([line(0.0001, 0.0006)]), niceScale(0, 0.0006, 4).step);
  assert.equal(axisStep([line(1, 2), line(0, 98)]), niceScale(0, 98, 4).step, 'every line counts');
  assert.equal(axisStep([line(-5, 3)]), niceScale(-5, 3, 4).step, 'values below zero move the bottom of the axis');
  assert.equal(axisStep([]), 1);
  assert.equal(axisStep(null), 1);
  assert.equal(axisStep([{ segments: [] }]), 1);
  assert.equal(axisStep([line(0, 0)]), niceScale(0, 0, 4).step, "a flat zero line gets the chart's own default axis");
  assert.equal(decimalsForStep(1), 0);
  assert.equal(decimalsForStep(0.5), 1);
  assert.equal(decimalsForStep(0.1), 1);
  assert.equal(decimalsForStep(0.02), 2);
  assert.equal(decimalsForStep(0.0002), 4);
  assert.equal(decimalsForStep(1e-12), 6, 'capped');
  assert.equal(decimalsForStep(0), 0);
  assert.equal(decimalsForStep(NaN), 0);
});
test('freshness: fresh up to 3 intervals, stale beyond, no verdict without an age or an interval', () => {
  assert.deepEqual(freshness(40, 60), { state: 'fresh', ageS: 40, limitS: 180 });
  assert.equal(freshness(180, 60).state, 'fresh', 'exactly 3 intervals is still fresh');
  assert.equal(freshness(180.5, 60).state, 'stale');
  assert.equal(freshness(0, 60).state, 'fresh');
  assert.deepEqual(freshness(null, 60), { state: 'unknown', ageS: null, limitS: 180 });
  assert.deepEqual(freshness(undefined, null), { state: 'unknown', ageS: null, limitS: null });
  assert.deepEqual(freshness(42, null), { state: 'unknown', ageS: 42, limitS: null });
  assert.deepEqual(freshness(42, 0), { state: 'unknown', ageS: 42, limitS: null });
  assert.deepEqual(freshness(-5, 60), { state: 'unknown', ageS: null, limitS: 180 }, 'a negative age is not an age');
  assert.equal(freshness(NaN, 60).state, 'unknown');
});
test('the sample age is now minus the newest sample mark; no mark, or a nonsense time, is no age', () => {
  assert.equal(sampleAge(1000, 1041), 41);
  assert.equal(sampleAge(1000, 1000), 0);
  assert.equal(sampleAge(1005, 1000), 0, 'a mark a little in the future (clock skew) is age 0, never negative');
  for (const bad of [null, undefined, NaN, Infinity, 'x', 0, -5, 4e11]) assert.equal(sampleAge(bad, 1000), null, String(bad));
  assert.equal(sampleAge(1000, null), null);
  // the chip: fresh up to 3 x interval_s of the newest mark
  const now = 1_800_000_000;
  assert.equal(freshness(sampleAge(now - 41, now), 60).state, 'fresh');
  assert.equal(freshness(sampleAge(now - 180, now), 60).state, 'fresh');
  assert.equal(freshness(sampleAge(now - 181, now), 60).state, 'stale');
  assert.equal(freshness(sampleAge(null, now), 60).state, 'unknown', 'null last_ts: no sample yet');
});
test('latest values come from the snapshot: gateway keys only, finite and non-negative, labels ignored, the first point wins', () => {
  const snap = {
    points: [
      { key: 'relay.gateway_up', value: 1 },
      { key: 'relay.gw_cpu', value: 0.05, labels: { x: 'y' } },
      { key: 'relay.gw_cpu', value: 9 },
      { key: 'relay.gw_rss', value: -1 },
      { key: 'relay.gw_fds', value: NaN },
      { key: 'relay.gw_threads', value: 0 },
      { key: 'relay.route_rps', value: '3' },
    ],
  };
  assert.deepEqual(latestValues(snap), { 'relay.gw_cpu': 0.05, 'relay.gw_threads': 0 });
  assert.deepEqual(latestValues(null), {});
  assert.deepEqual(latestValues({ points: 'no' }), {});
  assert.equal(hasResourceFigures({ 'relay.gateway_up': 1 }), false, 'a key that is not a gateway-resource figure is not one');
  assert.equal(hasResourceFigures({ 'relay.gw_threads': 0 }), true, 'zero is a figure');
  assert.equal(hasResourceFigures({}), false);
});
test('tiles: a second figure under the first, and limit events above zero are marked', () => {
  const tile = (id) => RES_TILES.find((x) => x.id === id);
  const latest = { 'relay.gw_rss': 188e6, 'relay.gw_mem_accounted': 236e6, 'relay.gw_limit_events': 2 };
  assert.deepEqual(tileModel(tile('memory'), latest), { id: 'memory', value: '179.3 MiB', sub: '225.1 MiB', hot: false, known: true });
  assert.deepEqual(tileModel(tile('limits'), latest), { id: 'limits', value: '2', sub: null, hot: true, known: true });
  assert.equal(tileModel(tile('limits'), { 'relay.gw_limit_events': 0 }).hot, false);
  const none = tileModel(tile('cpu'), {});
  assert.deepEqual(none, { id: 'cpu', value: '–', sub: null, hot: false, known: false });
  assert.equal(tileModel(tile('disk'), { 'relay.gw_disk_read': 10 }).sub, '–', 'a missing second figure is an en dash');
  // a quiet gateway reports 0 failures (not nothing): it reads "0", never an en dash
  const quiet = tileModel(tile('routing'), { 'relay.route_rps': 0, 'relay.route_failures': 0 });
  assert.deepEqual(quiet, { id: 'routing', value: '0 req/s', sub: '0 req/s', hot: false, known: true });
  assert.equal(latestValues({ points: [{ key: 'relay.route_failures', value: 0 }] })['relay.route_failures'], 0, 'a zero point is kept');
  assert.equal(hasResourceFigures({ 'relay.route_failures': 0 }), true);
});

// ------------------------------------------------------------------------------------------------------------------ diagnostics
test('RFC 3339 stamps as Relay prints them: nanoseconds kept to the millisecond, an offset honoured, anything else null', () => {
  assert.equal(parseStamp('2026-11-15T08:27:37.295079873Z'), Date.parse('2026-11-15T08:27:37.295Z') / 1000);
  assert.equal(parseStamp('2026-11-15T08:27:37Z'), Date.parse('2026-11-15T08:27:37Z') / 1000);
  assert.equal(parseStamp('2026-11-15T10:27:37.5+02:00'), Date.parse('2026-11-15T08:27:37.5Z') / 1000);
  for (const bad of [null, undefined, '', 'yesterday', '2026-11-15', '2026-11-15 08:27:37Z', '2026-13-45T99:99:99Z', 12, {}]) assert.equal(parseStamp(bad), null, String(bad));
});
test('a reason, or an answer that says nothing, is an unavailable card with the reason and nothing else', () => {
  for (const reason of DIAG_REASONS) {
    const m = normalizeDiagnostics(diagnosticsAnswer(reason, 1e9));
    assert.equal(m.available, false);
    assert.equal(m.reason, reason);
    assert.deepEqual([m.components, m.events, m.loss], [[], [], []]);
    assert.equal(m.totals, null);
    assert.equal(m.memory, null);
  }
  assert.equal(normalizeDiagnostics({ available: false }).reason, 'error', 'no reason given is the generic error');
  assert.equal(normalizeDiagnostics({ available: false, reason: 'made-up' }).reason, 'error');
  assert.equal(normalizeDiagnostics(null).reason, 'error');
  assert.equal(normalizeDiagnostics('x').reason, 'error');
  assert.equal(normalizeDiagnostics({}).reason, 'error');
  assert.equal(normalizeDiagnostics({ available: true, reason: 'disabled' }).reason, 'disabled', 'a reason wins');
});
test('the mock answer is normalised: components sorted failures first, counts kept, nothing invented', () => {
  const a = diagnosticsAnswer('ok', 1.8e9);
  const m = normalizeDiagnostics(a);
  assert.equal(m.available, true);
  assert.equal(m.reason, null);
  assert.equal(m.version, '0.11.1');
  assert.equal(m.totals.failures, a.totals.failures);
  assert.equal(m.totals.recorded, a.totals.recorded);
  assert.equal(m.components.length, a.components.length);
  assert.deepEqual(
    m.components.map((c) => c.failures),
    [...m.components.map((c) => c.failures)].sort((x, y) => y - x),
    'failures never increase down the table',
  );
  assert.equal(m.components[0].key, 'plugin.worker_protocol');
  assert.equal(m.events.length, a.events.length);
  assert.equal(m.events[0].seq, a.events[0].seq);
  assert.ok(
    m.events.every((e) => e.at != null),
    'every stamp parses',
  );
  assert.equal(m.retention.retained, 150);
  assert.equal(m.retention.truncated, false);
  assert.deepEqual(m.memory, { reservedBytes: 2595089, events: 150, eventCapacity: 256, detailBytes: 38400, detailCapacityBytes: 262144 });
  assert.equal(normalizeDiagnostics(diagnosticsAnswer('clean', 1.8e9)).totals.failures, 0);
});
test('a hostile or damaged answer cannot break the model: odd values become null, other or are dropped', () => {
  const m = normalizeDiagnostics({
    available: true,
    reason: null,
    relay_version: 7,
    captured_at: 'soon',
    elapsed_s: -4,
    totals: { recorded: 'many', successes: -1, failures: 2 ** 60, unspecified: null },
    components: [null, 5, { key: 7 }, { key: 'ok.one', failures: 3, successes: '4' }, { key: 'ok.two' }],
    events: [{ component: '<script>', code: 'a b', severity: 'x'.repeat(200), outcome: 'failure', count: 2 }, null, 'x', { seq: 1.5 }],
    loss: { events_evicted: 1, 'bad name!': 5, odd: 'x', flag: true },
    retention: { events_retained: 'n' },
    memory: { reserved_bytes: 10 },
  });
  assert.equal(m.version, null);
  assert.equal(m.capturedAt, null);
  assert.equal(m.elapsedS, null);
  assert.deepEqual(m.totals, { recorded: null, successes: null, failures: 2 ** 53, unspecified: null, evictions: null });
  assert.deepEqual(
    m.components.map((c) => [c.key, c.failures, c.successes]),
    [
      ['ok.one', 3, null],
      ['ok.two', null, null],
    ],
  );
  assert.equal(m.events.length, 2, 'only objects are events');
  assert.deepEqual([m.events[0].component, m.events[0].code, m.events[0].severity, m.events[0].outcome], ['other', 'other', 'other', 'failure']);
  assert.deepEqual(m.loss, [
    { name: 'events_evicted', value: 1, nonZero: true },
    { name: 'flag', value: 1, nonZero: true },
  ]);
  assert.deepEqual(m.retention, { retained: null, oldestSeq: null, latestSeq: null, truncated: null });
  assert.deepEqual(m.memory, { reservedBytes: 10, events: null, eventCapacity: null, detailBytes: null, detailCapacityBytes: null });
});
test('components keep the order the server gave them (failures first, then by key): nothing here re-sorts them', () => {
  const c = (key, failures, occurrences) => ({ key, instances: 1, successes: 0, failures, occurrences });
  // the server's order: failures desc, then key. Occurrences must not reorder equal-failure rows (the old frontend sort did).
  const served = [c('a_new', 2, 1), c('z_busy', 2, 900), c('b', 0, 5), c('c', 0, 50), c('d', 0, 1)];
  const m = normalizeDiagnostics({ available: true, components: served });
  assert.deepEqual(
    m.components.map((x) => x.key),
    ['a_new', 'z_busy', 'b', 'c', 'd'],
  );
  // an order the server did not give is kept too: the page never decides it
  const odd = normalizeDiagnostics({ available: true, components: [c('b', 0, 5), c('a', 3, 1)] });
  assert.deepEqual(
    odd.components.map((x) => x.key),
    ['b', 'a'],
  );
  assert.ok(!/sortComponents/.test(read('js/relay-runtime-model.js')), 'no component sorter is left in the model');
  assert.ok(!/\.sort\(/.test(read('js/rt/diagnostics-card.js')), 'and none in the card');
});
test('long names wrap after dots, underscores and dashes, and the parts join back into the name', () => {
  assert.deepEqual(breakParts('pii_redaction.selector.profile_10'), ['pii_', 'redaction.', 'selector.', 'profile_', '10']);
  assert.equal(breakParts('plugin.worker_protocol').join(''), 'plugin.worker_protocol');
  assert.deepEqual(breakParts('plain'), ['plain']);
  assert.deepEqual(breakParts(''), ['']);
  assert.deepEqual(breakParts(null), ['']);
  assert.equal(breakParts('<img src=x onerror=1>').join(''), '<img src=x onerror=1>', 'text stays text');
});
test('failure bars keep the server order among equal counts', () => {
  const c = (key, failures) => ({ key, failures, occurrences: 1 });
  assert.deepEqual(
    failureBars([c('first', 2), c('second', 2), c('big', 5), c('third', 2)]).rows.map((r) => r.key),
    ['big', 'first', 'second', 'third'],
  );
});
test('the failure bars show only components with failures, the most first, scaled to the largest', () => {
  const c = (key, failures) => ({ key, failures, occurrences: 1 });
  const fb = failureBars([c('a', 0), c('b', 6), c('c', 3), c('d', null), c('e', 1)], 2);
  assert.deepEqual(fb.rows, [
    { key: 'b', failures: 6, share: 1 },
    { key: 'c', failures: 3, share: 0.5 },
  ]);
  assert.deepEqual([fb.top, fb.shown, fb.total], [6, 2, 3]);
  assert.deepEqual(failureBars([c('a', 0)]).rows, []);
  assert.deepEqual(failureBars([]).rows, []);
});
test('the outcome filter lists the outcomes present with their counts and keeps the answer order', () => {
  const e = (outcome) => ({ outcome });
  const events = [e('success'), e('failure'), e('success'), e('unspecified'), e('success')];
  assert.deepEqual(outcomeOptions(events), [
    { value: 'success', count: 3 },
    { value: 'failure', count: 1 },
    { value: 'unspecified', count: 1 },
  ]);
  assert.equal(filterEvents(events, 'all').length, 5);
  assert.equal(filterEvents(events, 'success').length, 3);
  assert.equal(filterEvents(events, 'failure').length, 1);
  assert.equal(filterEvents(events, 'nothing').length, 0);
  assert.equal(filterEvents(events, undefined).length, 5);
  assert.deepEqual(outcomeOptions(null), []);
  const evs = [
    { outcome: 'failure', seq: 3 },
    { outcome: 'success', seq: 2 },
    { outcome: 'failure', seq: 1 },
  ];
  assert.deepEqual(
    filterEvents(evs, 'failure').map((x) => x.seq),
    [3, 1],
  );
});
test('truncation: the backend says so, and an older backend that does not is read from the counts', () => {
  assert.equal(eventsTruncated({ truncated: true, retained: 10 }, 10), true, 'the flag wins');
  assert.equal(eventsTruncated({ truncated: false, retained: 400 }, 150), false, 'and so does a false one');
  assert.equal(eventsTruncated({ truncated: null, retained: 400 }, 150), true);
  assert.equal(eventsTruncated({ truncated: null, retained: 150 }, 150), false);
  assert.equal(eventsTruncated({ retained: 150 }, 150), false);
  assert.equal(eventsTruncated({ truncated: null, retained: null }, 150), false);
  assert.equal(eventsTruncated(null, 150), false);
  const m = normalizeDiagnostics(diagnosticsAnswer('truncated', 1.8e9));
  assert.equal(m.retention.truncated, true);
  assert.ok(m.retention.retained > m.events.length);
  assert.equal(eventsTruncated(m.retention, m.events.length), true);
  assert.equal(normalizeDiagnostics({ available: true, retention: { truncated: 'yes' } }).retention.truncated, null, 'only a boolean counts');
  assert.match(en['rt.diag.events_truncated'], /\{gathered\}/);
});
test('loss counters: the ones above zero first and marked, then the zeros by name', () => {
  const rows = lossRows({ b_zero: 0, a_zero: 0, big: 9, small: 2, 'bad name': 1 });
  assert.deepEqual(rows, [
    { name: 'big', value: 9, nonZero: true },
    { name: 'small', value: 2, nonZero: true },
    { name: 'a_zero', value: 0, nonZero: false },
    { name: 'b_zero', value: 0, nonZero: false },
  ]);
  assert.deepEqual(lossRows(null), []);
  assert.deepEqual(lossRows([1, 2]), [], 'a list is not a loss map');
});
test('memory gauges: used of capacity, clamped, and no ratio without a capacity', () => {
  const g = memoryGauges({ events: 150, eventCapacity: 256, detailBytes: 38400, detailCapacityBytes: 262144 });
  assert.deepEqual(
    g.map((x) => [x.id, x.used, x.capacity]),
    [
      ['events', 150, 256],
      ['detail', 38400, 262144],
    ],
  );
  assert.ok(Math.abs(g[0].ratio - 150 / 256) < 1e-12);
  assert.equal(memoryGauges({ events: 300, eventCapacity: 256 })[0].ratio, 1, 'clamped to 1');
  assert.equal(memoryGauges({ events: 3, eventCapacity: null })[0].ratio, null);
  assert.equal(memoryGauges({ events: 3, eventCapacity: 0 })[0].ratio, null);
  assert.deepEqual(memoryGauges({ events: null, detailBytes: null }), []);
  assert.deepEqual(memoryGauges(null), []);
});
test('outcome and severity map to a badge kind; unknown words are muted (the card prints the word too)', () => {
  assert.equal(outcomeKind('failure'), 'bad');
  assert.equal(outcomeKind('success'), 'ok');
  assert.equal(outcomeKind('unspecified'), 'muted');
  assert.equal(outcomeKind('other'), 'muted');
  assert.equal(severityKind('error'), 'bad');
  assert.equal(severityKind('warn'), 'warn');
  assert.equal(severityKind('info'), 'info');
  assert.equal(severityKind('whatever'), 'muted');
  assert.equal(outcomeKind('__proto__'), 'muted', 'a prototype name is not a kind');
  assert.equal(severityKind('constructor'), 'muted');
  assert.deepEqual(['failure', 'success', 'unspecified', 'other', '__proto__'].map(outcomeGlyph), ['✕', '✓', '–', '·', '·']);
});

// ------------------------------------------------------------------------------------------------------------------ api
test('api: diagnostics is one GET of /systems/relay/diagnostics with the signal and no CSRF header; metrics may carry a signal', async () => {
  const calls = [];
  const api = createClient({
    fetchImpl: async (u, o) => {
      calls.push({ u, o });
      return res(200, {});
    },
  });
  const sig = new AbortController().signal;
  await api.diagnostics({ signal: sig });
  await api.metrics('relay', { signal: sig });
  await api.metrics('dynamo');
  await api.series('relay', { keys: ['relay.gw_cpu', 'relay.gw_rss'], window: '24h', split: false }, { signal: sig });
  assert.deepEqual(
    calls.map((c) => c.u),
    [
      '/api/v1/systems/relay/diagnostics',
      '/api/v1/systems/relay/metrics',
      '/api/v1/systems/dynamo/metrics',
      '/api/v1/systems/relay/metrics/series?keys=relay.gw_cpu%2Crelay.gw_rss&window=24h&split=0',
    ],
  );
  assert.ok(calls.every((c) => c.o.method === 'GET' && c.o.headers['X-Console'] === undefined));
  assert.equal(calls[0].o.signal, sig);
  assert.equal(calls[1].o.signal, sig);
});
test('api: a viewer asking for diagnostics gets a 403 ApiError the page can read', async () => {
  const api = createClient({ fetchImpl: async () => res(403, { error: { code: 'forbidden', message: 'admin role required' } }) });
  await assert.rejects(api.diagnostics(), (e) => e.status === 403 && e.code === 'forbidden');
});

// ------------------------------------------------------------------------------------------------------------------ messages
test('every message the runtime cards use exists, for every chart, tile, panel, line and reason', () => {
  const need = new Set(['rt.res.title', 'rt.res.window', 'rt.res.explain', 'rt.res.off_title', 'rt.res.off_hint', 'rt.res.off_configured', 'rt.res.off_snippet', 'rt.res.off_lab', 'rt.diag.title']);
  for (const c of RES_CARDS) {
    need.add(`rt.res.card_${c.id}`);
    for (const p of c.panels) {
      if (c.panels.length > 1) need.add(`rt.res.panel_${p.id}`);
      for (const k of p.lines) need.add(`rt.res.l_${k.replace(/^relay\./, '')}`);
    }
  }
  for (const tile of RES_TILES) {
    need.add(`rt.res.t_${tile.id}`);
    if (tile.sub) need.add(`rt.res.s_${tile.id}`);
  }
  for (const s of ['fresh', 'stale', 'unknown', 'unverified']) need.add(`rt.res.fresh_${s}`);
  for (const s of ['fresh', 'stale', 'unknown']) need.add(`rt.res.fresh_${s}_tip`);
  for (const r of DIAG_REASONS) {
    need.add(`rt.diag.reason_${r}_title`);
    need.add(`rt.diag.reason_${r}_text`);
  }
  for (const w of RES_WINDOWS) need.add(`window.${w}`);
  for (const g of ['events', 'detail']) need.add(`rt.diag.g_${g}`);
  for (const o of ['failure', 'success', 'unspecified', 'other']) need.add(`rt.diag.outcome_${o}`);
  for (const k of ['loss_some', 'loss_zero', 'bar_alt', 'failed_below']) for (const f of ['one', 'other']) need.add(`rt.diag.${k}_${f}`);
  const missing = [...need].filter((k) => !(k in en));
  assert.deepEqual(missing, []);
  // every loss name the mock (and so the fixture of the contract) carries has its own wording
  for (const name of Object.keys(diagnosticsAnswer('ok', 1e9).loss)) assert.ok(`rt.diag.ln_${name}` in en, name);
});
test('the card files use only messages that exist, and every placeholder they fill is in the message', () => {
  const src = ['js/rt/resources-card.js', 'js/rt/diagnostics-card.js', 'js/rt/capture-tab.js'].map(read).join('\n');
  for (const m of src.matchAll(/\bt\(\s*'((?:rt|window|series|common)\.[a-z_0-9.]+)'/g)) assert.ok(m[1] in en || `${m[1]}_one` in en, `${m[1]} is missing`);
  // the messages that take values name them, and a placeholder never goes unfilled by the code that uses it
  const fills = {
    'rt.res.scope': ['scope'],
    'rt.res.interval': ['every'],
    'rt.res.age': ['age'],
    'rt.res.none_hint': ['every'],
    'rt.diag.updated': ['time'],
    'rt.diag.events_caption': ['shown', 'total', 'oldest', 'latest'],
    'rt.diag.of': ['used', 'capacity'],
  };
  for (const [key, names] of Object.entries(fills)) {
    const found = [...en[key].matchAll(/\{(\w+)\}/g)].map((x) => x[1]).sort();
    assert.deepEqual(found, [...names].sort(), key);
  }
  assert.equal(t('rt.res.age', { age: '42s' }), '42s ago');
  // plurals: one and other, each with the count placeholder
  for (const key of ['rt.diag.loss_some', 'rt.diag.loss_zero', 'rt.diag.bar_alt', 'rt.diag.failed_below'])
    for (const form of ['one', 'other']) assert.match(en[`${key}_${form}`], /\{count\}/, `${key}_${form}`);
  assert.equal(t('rt.diag.bar_alt', { name: 'x', count: 1 }), 'x: 1 failure');
  assert.equal(t('rt.diag.bar_alt', { name: 'x', count: 6 }), 'x: 6 failures');
});
test('the remedies the operator is shown are exact: a generic plugins.toml snippet first, the lab command second, both environment lines for diagnostics', () => {
  const card = read('js/rt/resources-card.js');
  assert.match(card, /RESOURCES_ON_COMMAND = 'python3 -I ops\/nemo-relay-levels\.py resources on'/);
  const snippet = card.match(/RESOURCES_PLUGIN_SNIPPET = `([^`]*)`/)[1];
  assert.equal(
    snippet,
    '[[components]]\nkind = "resource_metrics"\nenabled = true\n\n[components.config]\nmeasurement_scope = "application_process"\n\n[components.config.polling]\nenabled = true\ninterval_millis = 60000\n',
  );
  // the snippet comes first in the off state (the lab command is the secondary line, labelled "In the lab")
  assert.ok(card.indexOf('snippetBlock(RESOURCES_PLUGIN_SNIPPET') < card.indexOf('fixBlock(RESOURCES_ON_COMMAND'), 'the generic remedy is first');
  assert.equal(en['rt.res.off_lab'], 'In the lab');
  assert.match(en['rt.res.off_hint'], /plugins\.toml/);
  assert.ok(!/ops\/nemo-relay-levels/.test(en['rt.res.off_hint'] + en['rt.res.off_configured']), 'no lab script in the explanation itself');
  const diag = read('js/rt/diagnostics-card.js');
  assert.match(diag, /DIAGNOSTICS_ON_LINES = 'NEMO_RELAY_DIAGNOSTICS=1\\nNEMO_RELAY_DIAGNOSTICS_HTTP=1\\n'/);
  assert.match(diag, /snippetBlock\(DIAGNOSTICS_ON_LINES/);
  const text = en['rt.diag.reason_disabled_text'];
  assert.match(text, /NEMO_RELAY_DIAGNOSTICS=1/);
  assert.match(text, /NEMO_RELAY_DIAGNOSTICS_HTTP=1/);
  assert.match(text, /restart/i);
  assert.match(text, /HOME/, 'a credential or tunnel that cannot be used (another HOME) is named as the other cause');
  assert.match(en['rt.diag.reason_disabled_title'], /cannot be read/);
  const help = read('i18n/help.en.json');
  assert.match(help, /NEMO_RELAY_DIAGNOSTICS_HTTP=1/);
  assert.match(help, /resource_metrics/);
  assert.match(help, /measurement_scope/);
});

test('the capture matrix types the resource_metrics level: the pinned keys, and the keys a newer backend may add', () => {
  const lvl = (detail, observed = {}) => ({ id: 'resource_metrics', label: 'x', enabled: true, state: 'active', detail, observed });
  const d = describeLevel(lvl({ polling: true, interval_ms: 60000 }, { last_ts: 1700000000 }));
  assert.deepEqual(d.configured, [
    { type: 'bool', key: 'polling', value: true },
    { type: 'ms', key: 'interval_ms', value: 60000 },
  ]);
  assert.deepEqual(d.observed, [{ type: 'ts', key: 'last_ts', value: 1700000000 }]);
  const n = describeLevel(lvl({ scope: 'process', polling: true, interval_s: 60, gpu: false, network: false }));
  assert.deepEqual(
    n.configured.map((f) => [f.type, f.key, f.value]),
    [
      ['text', 'scope', 'process'],
      ['bool', 'polling', true],
      ['ms', 'interval_s', 60000],
      ['bool', 'gpu', false],
      ['bool', 'network', false],
    ],
  );
  assert.deepEqual(describeLevel(lvl({ polling: null, interval_ms: null })).configured, [], 'an unconfigured sampler lists nothing');
  assert.ok('rt.cap.lvl_resource_metrics' in en && 'rt.cap.k_scope' in en && 'rt.cap.k_interval_s' in en && 'rt.cap.k_gpu' in en && 'rt.cap.k_network' in en);
  assert.match(read('js/rt/capture-tab.js'), /const levelLabel = /, 'the row is named by the catalogue where it has the level');
  // the row the backend files under "Other" (`group: "other"`) is shown there, not under Enrichment by its id
  const tab = read('js/rt/capture-tab.js');
  assert.match(tab, /const filedUnderOther = \(l\) => l\?\.group === 'other'/);
  assert.ok(!/\['rt\.cap\.g_enrich', \[[^\]]*resource_metrics/.test(tab), 'resource_metrics is not in the Enrichment list');
});

// ------------------------------------------------------------------------------------------------------------------ the card files
test('the runtime cards are built for admins only, never merely disabled for a viewer', () => {
  const tab = read('js/rt/capture-tab.js');
  assert.match(tab, /import \{ diagnosticsCard \} from '\.\/diagnostics-card\.js'/);
  assert.match(tab, /if \(isAdmin\(\)\) \{[\s\S]*diagnosticsCard\(diagScope\)/, 'the card is constructed inside an isAdmin() branch');
  assert.match(tab, /store\.subscribe\(syncDiagnostics, \(st\) => st\.role\)/, 'a role change builds or removes it');
  assert.equal((tab.match(/diagnosticsCard\(/g) || []).length, 1, 'one construction site');
  for (const f of walk(path.join(root, 'js'))) {
    if (f.endsWith('capture-tab.js') || f.endsWith('diagnostics-card.js')) continue;
    assert.ok(!/diagnosticsCard|scope\.api\.diagnostics|api\.diagnostics\(/.test(fs.readFileSync(f, 'utf8')), `${path.relative(root, f)} reaches the diagnostics`);
  }
  assert.ok(!/adminBtn|disabled: !isAdmin/.test(read('js/rt/diagnostics-card.js')), 'no disabled-for-viewer control inside the card');
});
test('the card files are text only: no markup from data, no inline style, no storage, no direct fetch', () => {
  for (const f of ['js/relay-runtime-model.js', 'js/rt/resources-card.js', 'js/rt/diagnostics-card.js']) {
    const src = read(f);
    assert.ok(!/\.innerHTML|insertAdjacentHTML|outerHTML|setTrustedHtml|document\.write|\beval\s*\(|new Function|createContextualFragment|DOMParser/.test(src), `${f}: markup from a string`);
    assert.ok(!/setAttribute\(\s*['"]style['"]|\bstyle\s*:/.test(src), `${f}: inline style`);
    assert.ok(!/localStorage|sessionStorage|document\.cookie/.test(src), `${f}: browser storage`);
    assert.ok(!/\bfetch\s*\(/.test(src), `${f}: the network without api.js`);
    assert.ok(!/https?:\/\//.test(src), `${f}: an origin`);
  }
});
test('the auto-refresh is the 30 s liveLoad that pauses a hidden tab, and a window change goes through the URL state', () => {
  for (const f of ['js/rt/resources-card.js', 'js/rt/diagnostics-card.js']) {
    const src = read(f);
    assert.match(src, /REFRESH_MS = 30000/, f);
    assert.match(src, /liveLoad\(scope, \{/, f);
    assert.ok(!/setInterval|setTimeout/.test(src), `${f}: its own timer instead of the scope's`);
  }
  assert.match(read('js/rt/live.js'), /!document\.hidden/);
  assert.match(read('js/rt/resources-card.js'), /nav\.set\(\{ res_window:/);
});
function walk(d) {
  return fs.readdirSync(d, { withFileTypes: true }).flatMap((e) => (e.isDirectory() ? walk(path.join(d, e.name)) : e.name.endsWith('.js') ? [path.join(d, e.name)] : []));
}

// ------------------------------------------------------------------------------------------------------------------ the mock
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
const dev = (qs) => fetch(`${base}/__dev/${qs}`, { headers: { 'X-Console': '1' } });
const get = (p) => fetch(`${base}/api/v1${p}`);

test('mock: the diagnostics route is admin only (403 forbidden for a viewer), one fixture per reason, and never carries a detail field', async () => {
  await dev('role?set=admin');
  for (const mode of DIAG_MODES) {
    await dev(`traces?diag=${mode}`);
    const r = await get('/systems/relay/diagnostics');
    assert.equal(r.status, 200, mode);
    const a = await r.json();
    if (DIAG_REASONS.includes(mode)) {
      assert.equal(a.available, false);
      assert.equal(a.reason, mode);
    } else {
      assert.equal(a.available, true);
      assert.equal(a.reason, null);
      assert.deepEqual(Object.keys(a).sort(), ['available', 'captured_at', 'components', 'elapsed_s', 'events', 'last_failure_at', 'loss', 'memory', 'reason', 'relay_version', 'retention', 'totals']);
      for (const ev of a.events) assert.deepEqual(Object.keys(ev).sort(), ['at', 'code', 'component', 'count', 'failure_class', 'outcome', 'seq', 'severity']);
      assert.deepEqual(Object.keys(a.retention).sort(), ['events_retained', 'latest_seq', 'oldest_seq', 'truncated']);
      assert.equal(a.retention.truncated, mode === 'truncated', `${mode}: truncated`);
      assert.equal(a.retention.events_retained > a.events.length, mode === 'truncated', `${mode}: more retained than listed only when truncated`);
      assert.ok(
        a.events.every((e, i) => i === 0 || e.seq < a.events[i - 1].seq),
        `${mode}: events newest first`,
      );
      assert.ok(!/"detail"|"correlation"|"origin"|"instance_id"|"process_id"|"context"/.test(JSON.stringify(a)));
    }
  }
  await dev('traces?diag=ok');
  await dev('role?set=viewer');
  const denied = await get('/systems/relay/diagnostics');
  assert.equal(denied.status, 403);
  const b = await denied.json();
  assert.equal(b.error.code, 'forbidden');
  assert.ok(b.error.message);
  await dev('role?set=public');
  assert.equal((await get('/systems/relay/diagnostics')).status, 403);
  await dev('role?set=admin');
  assert.equal((await get('/systems/dynamo/diagnostics')).status, 404, 'only Relay has it');
});
test('mock: the capture answer carries resources, the resource_metrics level and the relay.resources.off hint per mode, as the real backend answers them', async () => {
  for (const mode of RESOURCE_MODES) {
    await dev(`traces?resources=${mode}`);
    const c = await (await get('/systems/relay/capture')).json();
    assert.deepEqual(c.resources, resourcesOf(mode));
    assert.deepEqual(Object.keys(c.resources).sort(), ['configured', 'enabled', 'gpu', 'interval_s', 'network', 'scope']);
    const lv = c.levels.find((l) => l.id === 'resource_metrics');
    assert.deepEqual(Object.keys(lv).sort(), ['detail', 'enabled', 'group', 'id', 'label', 'observed', 'state'], 'the row alone carries `group`');
    assert.equal(lv.group, 'other');
    assert.deepEqual(Object.keys(lv.detail).sort(), ['interval_ms', 'polling'], `${mode} detail`);
    assert.deepEqual(Object.keys(lv.observed), ['last_ts'], `${mode} observed`);
    assert.equal(lv.enabled, c.resources.enabled === true && c.resources.interval_s != null);
    assert.equal(lv.state, { on: 'active', quiet: 'active', stale: 'stale', idle: 'idle', off: 'off', polling_off: 'off', unreadable: 'unknown' }[mode]);
    assert.equal(lv.observed.last_ts === null, mode !== 'on' && mode !== 'quiet' && mode !== 'stale', `${mode}: last_ts is the newest mark's time, null when none was seen`);
    assert.equal(c.levels.filter((l) => l.group !== undefined).length, 1, 'no other level carries a group');
    const hint = c.hints.find((x) => x.code === 'relay.resources.off');
    assert.equal(!!hint, lv.state === 'off', 'the hint fires whenever the row is off');
    if (hint) assert.match(hint.message, /resource_metrics. component with polling enabled to plugins\.toml.*python3 -I ops\/nemo-relay-levels\.py resources on/);
    assert.equal(!!c.hints.find((x) => x.code === 'capture.config.unreadable'), mode === 'unreadable');
    assert.ok(!c.hints.some((x) => x.code === 'capture.metrics.resource_off'), 'the old hint is gone');
    assert.equal(c.levels.filter((x) => x.id === 'resource_metrics').length, 1, 'one such level, not two');
    if (mode === 'unreadable')
      assert.ok(
        Object.values(c.resources).every((v) => v === null),
        'all six null when plugins.toml cannot be read',
      );
    if (mode === 'off') assert.equal(c.resources.configured, false);
    if (c.resources.scope) assert.equal(c.resources.scope, 'application_process');
  }
  await dev('traces?resources=on&diag=ok');
});
test('mock: the Relay metrics carry the gateway keys only while the sampler runs, and the series route serves them in two calls of at most 12 keys', async () => {
  await dev('traces?resources=on');
  const m = await (await get('/systems/relay/metrics')).json();
  const keys = new Set(m.points.map((p) => p.key));
  for (const k of RESOURCE_KEYS) assert.ok(keys.has(k), k);
  const plan = planCalls();
  for (const win of RES_WINDOWS) {
    for (const call of plan) {
      assert.ok(call.keys.length <= 12);
      const r = await get(`/systems/relay/metrics/series?keys=${encodeURIComponent(call.keys.join(','))}&window=${win}&split=0`);
      assert.equal(r.status, 200);
      const a = await r.json();
      assert.deepEqual(
        a.series.map((s) => s.key),
        call.keys,
      );
      for (const s of a.series) assert.ok(s.points.length > 10, `${s.key} ${win}`);
    }
  }
  const tooMany = await get(`/systems/relay/metrics/series?keys=${RESOURCE_KEYS.slice(0, 13).join(',')}&window=24h`);
  assert.equal(tooMany.status, 422, 'the route refuses a 13th key, which is why the keys are split');
  await dev('traces?resources=off');
  const off = await (await get('/systems/relay/metrics')).json();
  assert.ok(!off.points.some((p) => RESOURCE_KEYS.includes(p.key)), 'no resource key while it is off');
  assert.deepEqual(
    resourcePoints('off', (b) => b),
    [],
  );
  assert.deepEqual(
    resourcePoints('polling_off', (b) => b),
    [],
  );
  // how fresh the samples are comes from the capture answer's newest sample mark, not from a metric
  assert.ok(!resourcePoints('on', (b) => b).some((p) => /age/.test(p.key)), 'no age key in the mock either');
  const now = 1.8e9;
  assert.equal(freshness(sampleAge(resourceLevel('on', now).observed.last_ts, now), 60).state, 'fresh');
  assert.equal(freshness(sampleAge(resourceLevel('stale', now).observed.last_ts, now), 60).state, 'stale', 'the stale fixture lands beyond the 3-interval line');
  assert.equal(freshness(sampleAge(resourceLevel('idle', now).observed.last_ts, now), 60).state, 'unknown', 'none seen yet');
  await dev('traces?resources=on');
  assert.equal(resourceLevel('on', 1e9).state, 'active');
  assert.equal(resourceHint('on'), null);
});
