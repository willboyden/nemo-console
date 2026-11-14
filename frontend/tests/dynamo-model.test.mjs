// The Dynamo shaping (addendum 9.14): the latency breakdown from phase keys, output tokens, the capacity panel, spec-decode by position and the
// log filters. A phase or figure the worker does not report is null, never 0.
import test from 'node:test';
import assert from 'node:assert/strict';
import {
  DYN_KEYS,
  phaseSummary,
  phaseSeries,
  outputTokens,
  capacityModel,
  specAcceptModel,
  logRows,
  filterLogs,
  logFacets,
  normLevel,
  encodeLevels,
  decodeLevels,
  LOG_LEVELS,
} from '../js/dynamo-model.js';

const ser = (key, label, points) => ({ key, label, points: points.map(([t, v]) => [t, v, v]) });
const ramp = (n, f, t0 = 0, step = 60) => Array.from({ length: n }, (_, i) => [t0 + i * step, f(i)]);
const phase = (p, q, v, n = 5) =>
  ser(
    'dyn.phase_ms',
    { phase: p, q },
    ramp(n, () => v),
  );

test('the status tab asks for at most 12 keys, all valid series-route keys', () => {
  assert.ok(DYN_KEYS.length <= 12);
  for (const k of DYN_KEYS) assert.match(k, /^[a-z][a-z0-9_.]{0,63}(?:\{[A-Za-z0-9_.:/\-=,]{1,200}\})?$/, k);
  assert.ok(DYN_KEYS.includes('dyn.tokens{kind=completion}'), 'the adapter reports output tokens as kind=completion');
});

test('phaseSummary: the window mean of each phase for p50 and p95, the stack total, partial when a phase is missing', () => {
  const s = phaseSummary([
    phase('queue', 'p50', 10),
    phase('prefill', 'p50', 100),
    phase('decode', 'p50', 2000),
    phase('inference', 'p50', 2150),
    phase('queue', 'p95', 90),
    phase('decode', 'p95', 6000),
  ]);
  assert.deepEqual([s.p50.queue, s.p50.prefill, s.p50.decode, s.p50.inference, s.p50.total], [10, 100, 2000, 2150, 2110]);
  assert.equal(s.p50.partial, false);
  assert.equal(s.p50.any, true);
  assert.equal(s.p95.prefill, null, 'a missing phase is null, not 0');
  assert.equal(s.p95.total, 6090);
  assert.equal(s.p95.partial, true, 'the stack is a lower bound');
});

test('phaseSummary: no phase series at all (no request finished) gives no figures, not zeros', () => {
  for (const input of [
    [],
    undefined,
    [ser('dyn.phase_ms', { phase: 'queue', q: 'p50' }, [])],
    [
      ser(
        'dyn.ttft_ms',
        { q: 'p50' },
        ramp(3, () => 5),
      ),
    ],
  ]) {
    const s = phaseSummary(input);
    assert.equal(s.p50.total, null);
    assert.equal(s.p50.any, false);
    assert.equal(s.p95.queue, null);
    assert.equal(s.p95.partial, false);
  }
});

test('phaseSummary takes the mean over a window with a varying phase, and ignores other quantiles', () => {
  const s = phaseSummary([
    ser('dyn.phase_ms', { phase: 'decode', q: 'p50' }, [
      [0, 1000],
      [60, 3000],
    ]),
    ser('dyn.phase_ms', { phase: 'decode', q: 'p99' }, [[0, 9e9]]),
  ]);
  assert.equal(s.p50.decode, 2000);
  assert.equal(s.p95.decode, null);
});

test('phaseSeries: one stacked series per phase present, on one grid, null where no request finished, merged to fit', () => {
  const series = [
    ser('dyn.phase_ms', { phase: 'queue', q: 'p50' }, [
      [0, 5],
      [120, 7],
    ]),
    ser('dyn.phase_ms', { phase: 'decode', q: 'p50' }, [
      [0, 100],
      [60, 200],
      [120, 300],
    ]),
    ser('dyn.phase_ms', { phase: 'decode', q: 'p95' }, [[0, 9]]),
  ];
  const p = phaseSeries(series, 'p50', { from: 0, to: 120, stepS: 60 });
  assert.deepEqual(
    p.series.map((s) => s.key),
    ['queue', 'decode'],
  );
  assert.deepEqual(p.series[0].values, [5, null, 7]);
  assert.deepEqual(p.ts, [0, 60, 120]);
  assert.equal(p.any, true);
  assert.equal(phaseSeries([], 'p50', { from: 0, to: 120, stepS: 60 }).any, false);
  const long = phaseSeries(
    [
      ser(
        'dyn.phase_ms',
        { phase: 'decode', q: 'p50' },
        ramp(1008, () => 1, 0, 600),
      ),
    ],
    'p50',
    { from: 0, to: 1007 * 600, stepS: 600 },
    120,
  );
  assert.ok(long.ts.length <= 120);
});

test('outputTokens: the adapter key kind=completion, a series of kind=output wins when an answer has one, none gives null', () => {
  const c = ser(
    'dyn.tokens{kind=completion}',
    { kind: 'completion' },
    ramp(3, (i) => i * 10),
  );
  assert.equal(outputTokens([c]).kind, 'completion');
  const bare = ser(
    'dyn.tokens{kind=completion}',
    null,
    ramp(3, (i) => i * 10),
  );
  assert.equal(outputTokens([bare]).kind, 'completion', 'the selector of the requested key is enough when the route gives no label');
  const o = ser(
    'dyn.tokens',
    { kind: 'output' },
    ramp(3, (i) => i),
  );
  assert.equal(outputTokens([c, o]).kind, 'output');
  assert.deepEqual(outputTokens([ser('dyn.tokens{kind=completion}', { kind: 'completion' }, [])]), { kind: null, series: [] });
  assert.deepEqual(
    outputTokens([
      ser(
        'dyn.tokens',
        { kind: 'prompt' },
        ramp(3, (i) => i),
      ),
    ]),
    { kind: null, series: [] },
  );
  assert.deepEqual(outputTokens(undefined), { kind: null, series: [] });
});

test('capacityModel: the contract order, what is in use now where the snapshot has the matching figure, nothing reported is empty', () => {
  const pts = [
    { key: 'dyn.capacity', value: 16384, labels: { kind: 'max_batched_tokens' } },
    { key: 'dyn.capacity', value: 262144, labels: { kind: 'context_length' } },
    { key: 'dyn.capacity', value: 256, labels: { kind: 'max_num_seqs' } },
    { key: 'dyn.capacity', value: 18000, labels: { kind: 'kv_blocks' } },
    { key: 'dyn.capacity', value: 5, labels: { kind: 'unknown_kind' } },
    { key: 'dyn.requests_running', value: 64, labels: {} },
    { key: 'dyn.kv_cache_usage', value: 0.25, labels: {} },
  ];
  const m = capacityModel(pts);
  assert.deepEqual(
    m.rows.map((r) => r.kind),
    ['context_length', 'kv_blocks', 'max_num_seqs', 'max_batched_tokens'],
  );
  assert.equal(m.rows[1].used, 4500);
  assert.equal(m.rows[1].usedRatio, 0.25);
  assert.equal(m.rows[2].used, 64);
  assert.equal(m.rows[2].usedRatio, 0.25);
  assert.equal(m.rows[0].used, null);
  assert.equal(m.rows[3].usedRatio, null);
  assert.equal(m.any, true);
  const none = capacityModel([{ key: 'dyn.kv_cache_usage', value: 0.5, labels: {} }]);
  assert.equal(none.any, false);
  assert.equal(capacityModel(undefined).any, false);
  const noRun = capacityModel([{ key: 'dyn.capacity', value: 256, labels: { kind: 'max_num_seqs' } }]);
  assert.equal(noRun.rows[0].used, null, 'no running-requests figure: unknown, not 0 of 256');
});

test('specAcceptModel: by position in order, absent when the worker does not report it', () => {
  const m = specAcceptModel([
    { key: 'dyn.spec_accept_pos', value: 0.5, labels: { pos: '2' } },
    { key: 'dyn.spec_accept_pos', value: 0.8, labels: { pos: '0' } },
    { key: 'dyn.spec_accept_pos', value: 0.6, labels: { pos: '1' } },
    { key: 'dyn.spec_accept_pos', value: 0.1, labels: { pos: 'x' } },
    { key: 'dyn.spec_accept_ratio', value: 0.55, labels: {} },
  ]);
  assert.deepEqual(m.rows, [
    { pos: 0, ratio: 0.8 },
    { pos: 1, ratio: 0.6 },
    { pos: 2, ratio: 0.5 },
  ]);
  assert.equal(m.overall, 0.55);
  assert.equal(m.any, true);
  const none = specAcceptModel([{ key: 'dyn.spec_accept_ratio', value: 0.5, labels: {} }]);
  assert.equal(none.any, false);
  assert.equal(none.overall, 0.5);
  assert.equal(specAcceptModel(undefined).any, false);
});

const ev = (ts, level, component, text, kind = 'dynamo.log') => ({ ts, kind, summary: text, data: { level, component } });
test('normLevel: case-insensitive, WARNING is WARN, anything else is INFO', () => {
  assert.equal(normLevel('warn'), 'WARN');
  assert.equal(normLevel('Warning'), 'WARN');
  assert.equal(normLevel('error'), 'ERROR');
  assert.equal(normLevel(' trace '), 'TRACE');
  assert.equal(normLevel('fatal'), 'INFO');
  assert.equal(normLevel(undefined), 'INFO');
});

test('logRows keeps only dynamo.log events, newest first, with an en dash for a missing component; the text is kept as is (markup stays text)', () => {
  const rows = logRows([
    ev(1, 'INFO', 'worker', 'a'),
    ev(3, 'warn', '', '<img src=x onerror=alert(1)>'),
    ev(2, 'ERROR', 'frontend', 'c'),
    ev(9, 'INFO', 'worker', 'other', 'relay.atof'),
    null,
    { kind: 'dynamo.log', ts: 'x' },
  ]);
  assert.deepEqual(
    rows.map((r) => r.ts),
    [3, 2, 1],
  );
  assert.equal(rows[0].component, '–');
  assert.equal(rows[0].level, 'WARN');
  assert.equal(rows[0].text, '<img src=x onerror=alert(1)>');
  assert.deepEqual(logRows(undefined), []);
});

test('filterLogs: levels, component and a plain-text search combine; the search is a substring, never a pattern', () => {
  const rows = logRows([
    ev(1, 'INFO', 'worker', 'kv cache 21%'),
    ev(2, 'ERROR', 'worker', 'request aborted'),
    ev(3, 'WARN', 'frontend', 'slow request (a.b)'),
    ev(4, 'DEBUG', 'frontend', 'router: worker 0'),
  ]);
  assert.equal(filterLogs(rows, {}).length, 4);
  assert.deepEqual(
    filterLogs(rows, { levels: new Set(['ERROR', 'WARN']) }).map((r) => r.level),
    ['WARN', 'ERROR'],
  );
  assert.deepEqual(
    filterLogs(rows, { component: 'frontend' }).map((r) => r.ts),
    [4, 3],
  );
  assert.deepEqual(
    filterLogs(rows, { text: 'REQUEST' }).map((r) => r.ts),
    [3, 2],
    'case-insensitive',
  );
  assert.deepEqual(
    filterLogs(rows, { text: 'a.b' }).map((r) => r.ts),
    [3],
  );
  assert.deepEqual(filterLogs(rows, { text: '.*' }), [], 'a regex is not interpreted');
  assert.deepEqual(filterLogs(rows, { levels: new Set(['INFO']), component: 'frontend' }), []);
  assert.deepEqual(filterLogs(rows, { text: '  ' }).length, 4, 'a blank search keeps everything');
  assert.deepEqual(filterLogs(undefined, {}), []);
});

test('logFacets: a count for every level (0 allowed) and the components, busiest first', () => {
  const f = logFacets(logRows([ev(1, 'INFO', 'worker', 'a'), ev(2, 'INFO', 'worker', 'b'), ev(3, 'ERROR', 'frontend', 'c')]));
  assert.deepEqual(f.levels, { ERROR: 1, WARN: 0, INFO: 2, DEBUG: 0, TRACE: 0 });
  assert.deepEqual(f.components, [
    { name: 'worker', count: 2 },
    { name: 'frontend', count: 1 },
  ]);
  assert.deepEqual(logFacets([]).components, []);
});

test('the level filter round-trips through the URL form; all levels and junk mean "all" (null)', () => {
  assert.equal(encodeLevels(new Set(['WARN', 'ERROR'])), 'ERROR|WARN', 'in level order');
  assert.equal(encodeLevels(new Set(LOG_LEVELS)), '');
  assert.equal(encodeLevels(new Set()), '');
  assert.equal(encodeLevels(null), '');
  assert.deepEqual([...decodeLevels('ERROR|warn')], ['ERROR', 'WARN']);
  assert.equal(decodeLevels(''), null);
  assert.equal(decodeLevels('nope|zzz'), null);
  assert.equal(decodeLevels(undefined), null);
});
