// The OpenShell models (addendum 9.16): lifecycle lanes from the persisted events, the phase stack, the decision timeline.
import test from 'node:test';
import assert from 'node:assert/strict';
import { phaseBucket, lifecycleEvents, presentSandboxes, lifecycleLanes, phaseStack, decisionModel, PHASES, MAX_LANES } from '../js/openshell-model.js';

const NOW = 2_000_000;
const E = (name, ago, action, phase, previous = null, id = `id-${name}`) => ({
  ts: NOW - ago,
  system: 'openshell',
  kind: 'openshell.sandbox',
  summary: '',
  data: { name, id, phase, action, previous_phase: previous },
});
// a Data Designer run: created, Ready 20 s later, deleted at the end
const run = (name, ago, life = 300) => [
  E(name, ago, 'created', 'Provisioning'),
  E(name, ago - 20, 'phase', 'Ready', 'Provisioning'),
  E(name, ago - life, 'phase', 'Deleting', 'Ready'),
  E(name, ago - life - 5, 'deleted', 'Deleting', 'Deleting'),
];
const newestFirst = (evs) => [...evs].sort((a, b) => b.ts - a.ts);

test("phase buckets are the adapter's: four known phases case-insensitively, everything else Other", () => {
  assert.deepEqual(PHASES, ['Provisioning', 'Ready', 'Error', 'Deleting', 'Other']);
  assert.equal(phaseBucket('ready'), 'Ready');
  assert.equal(phaseBucket(' PROVISIONING '), 'Provisioning');
  assert.equal(phaseBucket('Terminating'), 'Other');
  assert.equal(phaseBucket(null), 'Other');
});

test('lifecycleEvents keeps only well-formed sandbox events, oldest first (the API lists newest first)', () => {
  const evs = [...newestFirst(run('a', 600)), { kind: 'dd.run', ts: 1, data: { name: 'x' } }, { kind: 'openshell.sandbox', ts: 2, data: {} }, { kind: 'openshell.sandbox', data: { name: 'n' } }, null];
  const l = lifecycleEvents(evs);
  assert.equal(l.length, 4);
  assert.deepEqual(
    l.map((e) => e.action),
    ['created', 'phase', 'phase', 'deleted'],
  );
  assert.ok(l.every((e, i) => i === 0 || e.ts >= l[i - 1].ts));
});

test('a finished run is one lane: Provisioning -> Ready -> Deleting, closed by the deletion; it is not present', () => {
  const m = lifecycleLanes(newestFirst(run('dd-a', 900)), { from: NOW - 3600, to: NOW, now: NOW });
  assert.equal(m.lanes.length, 1);
  const l = m.lanes[0];
  assert.deepEqual(
    l.segments.map((s) => s.bucket),
    ['Provisioning', 'Ready', 'Deleting'],
  );
  assert.equal(l.present, false);
  assert.equal(l.createdTs, NOW - 900);
  assert.equal(l.provisionS, 20, 'ready after the creation');
  assert.equal(l.deletedTs, NOW - 595, 'five seconds after the Deleting phase began');
  assert.equal(l.lifetimeS, 305);
  assert.ok(l.segments.every((s) => !s.open));
  assert.deepEqual(
    l.marks.map((x) => x.action),
    ['created', 'phase', 'phase', 'deleted'],
  );
});

test('a sandbox that still exists has an open segment that runs to now; segments tile without gaps', () => {
  const evs = newestFirst([E('live', 700, 'created', 'Provisioning'), E('live', 650, 'phase', 'Ready', 'Provisioning')]);
  const l = lifecycleLanes(evs, { from: NOW - 3600, to: NOW, now: NOW }).lanes[0];
  assert.equal(l.present, true);
  assert.equal(l.segments.at(-1).open, true);
  assert.equal(l.segments.at(-1).to, NOW);
  for (let i = 1; i < l.segments.length; i++) assert.equal(l.segments[i].from, l.segments[i - 1].to);
  assert.equal(l.lifetimeS, 700);
});

test('a sandbox seen at baseline (`present`, created long ago) is clipped to the window and says so; a phase change with no earlier event has an unknown start', () => {
  const m = lifecycleLanes(newestFirst([E('old', 20 * 3600, 'present', 'Ready'), E('mid', 1800, 'phase', 'Ready', 'Provisioning')]), { from: NOW - 3600, to: NOW, now: NOW });
  const old = m.lanes.find((l) => l.name === 'old');
  assert.equal(old.segments[0].clippedStart, true);
  assert.equal(old.segments[0].from, NOW - 3600);
  assert.equal(old.createdTs, NOW - 20 * 3600);
  const mid = m.lanes.find((l) => l.name === 'mid');
  assert.deepEqual(
    mid.segments.map((s) => [s.bucket, s.startUnknown]),
    [
      ['Provisioning', true],
      ['Ready', false],
    ],
  );
  assert.equal(mid.createdTs, null);
  assert.equal(mid.provisionS, null, 'no creation event: no "ready after" figure');
});

test('a deletion with no earlier event is a lane of the phase it was in, start unknown', () => {
  const l = lifecycleLanes([E('gone', 100, 'deleted', 'Deleting', 'Deleting')], { from: NOW - 3600, to: NOW, now: NOW }).lanes[0];
  assert.equal(l.segments[0].startUnknown, true);
  assert.equal(l.present, false);
  assert.equal(l.deletedTs, NOW - 100);
});

test('a sandbox that ended in Error has an Error segment; a re-created name is one lane with a gap', () => {
  const evs = newestFirst([
    E('e', 900, 'created', 'Provisioning'),
    E('e', 850, 'phase', 'Error', 'Provisioning'),
    E('e', 800, 'deleted', 'Error', 'Error'),
    E('e', 400, 'created', 'Provisioning', null, 'id-2'),
    E('e', 380, 'phase', 'Ready', 'Provisioning', 'id-2'),
  ]);
  const m = lifecycleLanes(evs, { from: NOW - 3600, to: NOW, now: NOW });
  assert.equal(m.lanes.length, 1);
  assert.deepEqual(
    m.lanes[0].segments.map((s) => s.bucket),
    ['Provisioning', 'Error', 'Provisioning', 'Ready'],
  );
  assert.ok(m.lanes[0].segments[2].from > m.lanes[0].segments[1].to, 'the gap between the two lives');
  assert.equal(m.lanes[0].present, true);
});

test('lanes outside the window are dropped; more than maxLanes keeps the newest and counts the rest', () => {
  const evs = [];
  for (let i = 0; i < MAX_LANES + 3; i++) evs.push(...run(`s${i}`, 3000 - i * 100, 60));
  evs.push(...run('ancient', 5 * 86400, 60));
  const m = lifecycleLanes(newestFirst(evs), { from: NOW - 3600, to: NOW, now: NOW });
  assert.equal(m.total, MAX_LANES + 3);
  assert.equal(m.lanes.length, MAX_LANES);
  assert.equal(m.hidden, 3);
  assert.ok(!m.lanes.some((l) => l.name === 'ancient'));
  assert.ok(
    m.lanes.some((l) => l.name === `s${MAX_LANES + 2}`),
    'the newest is kept',
  );
  assert.ok(!m.lanes.some((l) => l.name === 's0'), 'the oldest of those in the window is dropped');
  assert.equal(lifecycleLanes([], { from: 0, to: 1 }).lanes.length, 0);
  assert.equal(lifecycleLanes(null, { from: 0, to: 1 }).lanes.length, 0);
});

test('presentSandboxes: the names whose last event is not a deletion, newest first', () => {
  const evs = newestFirst([
    ...run('done', 5000),
    E('a', 900, 'created', 'Provisioning'),
    E('b', 100, 'created', 'Provisioning'),
    E('b', 80, 'phase', 'Ready', 'Provisioning'),
    E('c', 3000, 'present', 'Ready'),
  ]);
  assert.deepEqual(
    presentSandboxes(evs).map((s) => s.name),
    ['b', 'a', 'c'],
  );
  assert.equal(presentSandboxes(evs)[0].phase, 'Ready');
  assert.deepEqual(presentSandboxes(null), []);
});

// ---- the phase stack

const ans = (series, over = {}) => ({ window: '1h', step_s: 60, from: 1000, to: 1000 + 59 * 60, series, truncated: false, ...over });
const line = (phase, vals, from = 1000) => ({ key: 'os.sandboxes_by_phase', label: { phase }, points: vals.map((v, i) => [from + i * 60, v, v]).filter((p) => p[1] != null) });

test('phaseStack puts the phases on one grid in the stacking order; a missing sample is null in every part (a gap), a real 0 stays 0', () => {
  const n = 60;
  const ready = Array.from({ length: n }, (_, i) => (i < 30 ? 1 : 2));
  const prov = Array.from({ length: n }, () => 0);
  for (const i of [10, 11]) ((ready[i] = null), (prov[i] = null));
  const m = phaseStack(ans([line('Ready', ready), line('Provisioning', prov)]));
  assert.deepEqual(
    m.series.map((s) => s.key),
    ['Provisioning', 'Ready'],
  );
  assert.equal(m.ts.length, 60);
  assert.equal(m.bucketS, 60);
  assert.equal(m.series[1].values[10], null);
  assert.equal(m.series[0].values[10], null);
  assert.equal(m.series[0].values[5], 0, 'a real zero is kept');
  assert.equal(m.latest.Ready, 2);
  assert.equal(m.latest.Error, null, 'a phase the answer does not have is null, not 0');
  assert.equal(m.peak, 2);
  assert.equal(m.any, true);
});

test('phaseStack merges to maxCols by the mean, and an answer with no series is empty', () => {
  const m = phaseStack(
    ans([
      line(
        'Ready',
        Array.from({ length: 60 }, (_, i) => i % 2),
      ),
    ]),
    { maxCols: 30 },
  );
  assert.equal(m.ts.length, 30);
  assert.equal(m.bucketS, 120);
  assert.equal(m.series[0].values[0], 0.5);
  for (const bad of [null, {}, ans([])]) assert.equal(phaseStack(bad).any, false);
});

// ---- decisions

const T = 5000;
const D = (i, category, verdict, kind = null, target = null) => ({ ts: T + i, category, verdict, kind, target });

test('decisionModel counts allowed and denied per category lane and per time slot', () => {
  const rows = [
    D(0, 'NET', 'allowed', 'CONNECT', 'a.example.test:443'),
    D(10, 'NET', 'denied', 'CONNECT', 'b.example.test:443'),
    D(20, 'NET', 'denied', 'CONNECT', 'b.example.test:443'),
    D(30, 'SSH', 'allowed', 'OPEN'),
    D(59, 'FS', 'allowed'),
  ];
  const m = decisionModel({ sandbox: 'sb', rows, truncated: false }, { bins: 6 });
  assert.equal(m.sandbox, 'sb');
  assert.equal(m.total, 5);
  assert.equal(m.allowed, 3);
  assert.equal(m.denied, 2);
  assert.deepEqual(
    m.lanes.map((l) => l.category),
    ['NET', 'FS', 'SSH'].sort((a, b) => (a === 'NET' ? -1 : b === 'NET' ? 1 : a < b ? -1 : 1)),
  );
  const net = m.lanes.find((l) => l.category === 'NET');
  assert.deepEqual([net.allowed, net.denied], [1, 2]);
  assert.equal(
    net.bins.reduce((a, b) => a + b.a + b.d, 0),
    3,
  );
  assert.equal(m.from, T);
  assert.equal(m.to, T + 59);
  assert.equal(m.bins, 6);
  assert.equal(m.bins * m.binS, 59);
  assert.equal(
    m.lanes.every((l) => l.bins.length === 6),
    true,
  );
  assert.equal(m.lanes.find((l) => l.category === 'FS').bins[5].a, 1, 'the last row lands in the last slot');
});

test('the denied targets are counted per category, kind and target, most denied first; a missing target stays null', () => {
  const rows = [
    D(0, 'NET', 'denied', 'CONNECT', 'x.example.test:443'),
    D(1, 'NET', 'denied', 'CONNECT', 'x.example.test:443'),
    D(2, 'NET', 'denied', 'CONNECT', 'y.example.test:80'),
    D(3, 'FS', 'denied', 'ACCESS'),
    D(4, 'NET', 'allowed', 'CONNECT', 'x.example.test:443'),
  ];
  const m = decisionModel({ rows });
  assert.deepEqual(
    m.deniedTargets.map((t) => [t.category, t.target, t.count]),
    [
      ['NET', 'x.example.test:443', 2],
      ['FS', null, 1],
      ['NET', 'y.example.test:80', 1],
    ].sort((a, b) => b[2] - a[2]),
  );
  assert.equal(m.deniedTargets[0].last, T + 1);
  assert.equal(m.deniedTargets.find((t) => t.category === 'FS').target, null);
});

test('rows that are not allowed / denied or have no time are skipped and counted, never drawn', () => {
  const m = decisionModel({ rows: [D(0, 'NET', 'allowed'), { ts: T, category: 'NET', verdict: 'maybe' }, { category: 'NET', verdict: 'allowed' }, null, { ts: 'x', verdict: 'denied' }] });
  assert.equal(m.total, 1);
  assert.equal(m.skipped, 4);
});

test('more categories than lanes: the busiest are named and the rest are summed into other', () => {
  const rows = [];
  ['A', 'B', 'C', 'D', 'E'].forEach((c, k) => {
    for (let i = 0; i <= k; i++) rows.push(D(i + k * 7, c, 'allowed'));
  });
  const m = decisionModel({ rows }, { maxLanes: 3 });
  assert.equal(m.lanes.length, 3);
  assert.deepEqual(
    m.lanes.map((l) => l.category),
    ['E', 'D', 'other'],
    'the catch-all lane is last, even when it is the busiest',
  );
  assert.equal(m.lanes.find((l) => l.category === 'other').allowed, 1 + 2 + 3);
});

test('empty, truncated and single-row logs', () => {
  const none = decisionModel({ sandbox: 's', rows: [], truncated: false });
  assert.equal(none.empty, true);
  assert.equal(none.total, 0);
  assert.equal(decisionModel(null).empty, true);
  const one = decisionModel({ rows: [D(0, 'NET', 'denied')], truncated: true });
  assert.equal(one.truncated, true);
  assert.equal(one.bins, 1);
  assert.ok(one.to > one.from, 'a log of one row still has a time span to draw');
  assert.equal(one.lanes[0].bins[0].d, 1);
});

test('a sparse log (often only the SSH open) is flagged and keeps its rows oldest first; a failed log call is a problem with no rows', () => {
  const m = decisionModel({ sandbox: 's', rows: [D(5, 'ssh', 'allowed', 'open'), D(1, 'config', 'allowed', 'applying')], truncated: false });
  assert.equal(m.sparse, true);
  assert.deepEqual(
    m.rows.map((r) => [r.category, r.kind]),
    [
      ['config', 'applying'],
      ['ssh', 'open'],
    ],
  );
  const many = decisionModel({ rows: Array.from({ length: 20 }, (_, i) => D(i, 'network', 'allowed')) });
  assert.equal(many.sparse, false);
  const failed = decisionModel({ sandbox: 's', rows: [], truncated: false, problem: { code: 'openshell.logs.failed', message: 'the CLI call failed' } });
  assert.equal(failed.empty, true);
  assert.deepEqual(failed.problem, { code: 'openshell.logs.failed', message: 'the CLI call failed' });
  assert.equal(decisionModel({ rows: [], problem: { message: 'no code' } }).problem, null);
});
