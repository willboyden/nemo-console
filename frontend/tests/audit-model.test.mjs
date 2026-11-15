// The Audit models (addendum 9.16): the summary as stacked days, an actor x system matrix and an error timeline; the log filters as the query GET /audit takes.
import test from 'node:test';
import assert from 'node:assert/strict';
import {
  shapeSummary,
  errorGrid,
  dayStart,
  logState,
  logQuery,
  logFiltered,
  parseUtcInput,
  toUtcInput,
  summaryWindow,
  CONSOLE_SYSTEM,
  NO_ACTOR,
  MAX_STACK_SYSTEMS,
  MAX_ACTORS,
  LOG_LIMIT,
} from '../js/audit-model.js';

const DAY = 86400;
const NOW = Date.UTC(2026, 10, 14, 12, 0, 0) / 1000;
const d = (s) => Date.UTC(...s.split('-').map((x, i) => (i === 1 ? Number(x) - 1 : Number(x)))) / 1000;

test('dayStart reads a UTC day, epoch seconds or milliseconds, and refuses anything else', () => {
  assert.equal(dayStart('2026-11-14'), d('2026-11-14'));
  assert.equal(dayStart('2026-11-14T09:30:00Z'), d('2026-11-14'));
  assert.equal(dayStart(d('2026-11-14') + 5000), d('2026-11-14'));
  assert.equal(dayStart((d('2026-11-14') + 5000) * 1000), d('2026-11-14'));
  for (const bad of [null, undefined, '', 'yesterday', 0, -3, NaN, {}, '14/11/2026']) assert.equal(dayStart(bad), null);
});

const S = {
  window: '7d',
  from: NOW - 7 * DAY,
  to: NOW,
  bucket_s: 21600,
  by_day: [
    { day: '2026-11-12', system: 'switchyard', count: 4 },
    { day: '2026-11-12', system: 'dynamo', count: 1 },
    { day: '2026-11-14', system: 'switchyard', count: 2 },
    { day: '2026-11-14', system: null, count: 3 },
  ],
  by_actor: [
    { actor: 'ops', system: 'switchyard', count: 5 },
    { actor: 'ops', system: 'dynamo', count: 1 },
    { actor: 'ci', system: 'switchyard', count: 1 },
    { actor: null, system: null, count: 3 },
  ],
  errors: [
    { ts_bucket: d('2026-11-13') + 6 * 3600, count: 2 },
    { ts_bucket: d('2026-11-14'), count: 1 },
  ],
  denied_total: 17,
  truncated: false,
};

test("actions per day: every day between the first and the last is a column (a quiet day is a real zero), stacked by system, the console's own calls under their own id", () => {
  const m = shapeSummary(S, { now: NOW });
  assert.equal(m.window, '7d');
  assert.deepEqual(m.days.ts, [d('2026-11-12'), d('2026-11-13'), d('2026-11-14')]);
  assert.equal(m.days.bucketS, DAY);
  assert.deepEqual(m.days.parts, ['switchyard', CONSOLE_SYSTEM, 'dynamo'], 'busiest first');
  const by = Object.fromEntries(m.days.series.map((s) => [s.key, s.values]));
  assert.deepEqual(by.switchyard, [4, 0, 2]);
  assert.deepEqual(by.dynamo, [1, 0, 0]);
  assert.deepEqual(by[CONSOLE_SYSTEM], [0, 0, 3]);
  assert.equal(m.total, 10);
  assert.deepEqual(
    m.systems.map((s) => [s.system, s.total]),
    [
      ['switchyard', 6],
      [CONSOLE_SYSTEM, 3],
      ['dynamo', 1],
    ],
  );
});

test('more systems than the stack has colours: the busiest are named, the rest are summed as "other"', () => {
  const rows = Array.from({ length: MAX_STACK_SYSTEMS + 3 }, (_, i) => ({ day: '2026-11-14', system: `s${i}`, count: 10 - i }));
  const m = shapeSummary({ window: '7d', by_day: rows }, { now: NOW });
  assert.equal(m.days.parts.length, MAX_STACK_SYSTEMS + 1);
  assert.equal(m.days.parts.at(-1), 'other');
  assert.deepEqual(m.days.parts.slice(0, 2), ['s0', 's1']);
  const other = m.days.series.find((s) => s.key === 'other');
  const expected = rows.slice(MAX_STACK_SYSTEMS).reduce((a, r) => a + r.count, 0);
  assert.equal(other.values[0], expected);
  assert.equal(
    m.days.series.reduce((a, s) => a + s.values[0], 0),
    m.total,
    'nothing is lost in the grouping',
  );
  // exactly one over: no needless "other" for a single system
  const six = shapeSummary({ window: '7d', by_day: rows.slice(0, MAX_STACK_SYSTEMS + 1) }, { now: NOW });
  assert.ok(!six.days.parts.includes('other'));
});

test('actor x system matrix: busiest actor first, a cell no call made is null (not 0), the key-less actor has its own id', () => {
  const m = shapeSummary(S, { now: NOW });
  assert.deepEqual(
    m.actors.map((a) => [a.actor, a.total]),
    [
      ['ops', 6],
      [NO_ACTOR, 3],
      ['ci', 1],
    ],
  );
  assert.equal(m.matrix('ops', 'switchyard'), 5);
  assert.equal(m.matrix('ops', 'dynamo'), 1);
  assert.equal(m.matrix('ci', 'dynamo'), null);
  assert.equal(m.matrix(NO_ACTOR, CONSOLE_SYSTEM), 3);
  assert.equal(m.matrix('nobody', 'switchyard'), null);
  assert.deepEqual(m.matrixCols, ['switchyard', CONSOLE_SYSTEM, 'dynamo'], 'systems by their total');
  assert.equal(m.matrixMax, 5);
  assert.equal(m.actorsHidden, 0);
  const many = shapeSummary({ window: '7d', by_actor: Array.from({ length: MAX_ACTORS + 4 }, (_, i) => ({ actor: `a${i}`, system: 'x', count: 100 - i })) }, { now: NOW });
  assert.equal(many.actors.length, MAX_ACTORS);
  assert.equal(many.actorsHidden, 4);
});

test("the error timeline is on the answer's own grid (from, to, bucket_s); a bucket it does not list had none, a real 0", () => {
  const m = shapeSummary(S, { now: NOW });
  assert.equal(m.errors.bucketS, 21600);
  assert.equal(m.errors.total, 3);
  assert.equal(m.errors.peak, 2);
  assert.equal(m.errors.ts.length, m.errors.values.length);
  assert.equal(m.errors.ts.length, 28, 'seven days of six-hour buckets');
  assert.ok(m.errors.values.filter((v) => v === 0).length >= 25);
  const i = m.errors.ts.indexOf(d('2026-11-13') + 6 * 3600);
  assert.equal(m.errors.values[i], 2);
  assert.ok(m.errors.ts.every((t, k) => k === 0 || t - m.errors.ts[k - 1] === 21600));
});

test('without from / to / bucket_s the grid ends at now and the bucket is a default by window (or the smallest gap)', () => {
  const g24 = errorGrid([{ ts: NOW - 7200, count: 1 }], '24h', NOW);
  assert.equal(g24.bucketS, 3600);
  assert.equal(g24.ts.length, 24, 'a day of hourly buckets');
  assert.equal(g24.total, 1);
  assert.equal(errorGrid([], '30d', NOW).bucketS, DAY);
  const gap = errorGrid(
    [
      { ts: NOW - 3 * 3600, count: 1 },
      { ts: NOW - 2 * 3600, count: 1 },
    ],
    '7d',
    NOW,
  );
  assert.equal(gap.bucketS, 3600, 'the smallest gap between the listed buckets');
  assert.equal(errorGrid([{ ts: 5, count: 4 }], '7d', NOW).total, 0, 'a bucket outside the window is not counted');
});

test("rows that do not parse are dropped and counted; the answer's own flags are carried; garbage is an empty summary", () => {
  const m = shapeSummary(
    {
      window: '30d',
      by_day: [{ day: 'x', system: 'a', count: 1 }, { day: '2026-11-14', system: 'a', count: -2 }, null, { day: '2026-11-14', system: 'a', count: 2 }],
      by_actor: [{ actor: 'a', system: 'b', count: 'many' }],
      truncated: true,
      denied_total: null,
    },
    { now: NOW },
  );
  assert.equal(m.dropped, 4);
  assert.equal(m.total, 2);
  assert.equal(m.truncated, true);
  assert.equal(m.deniedTotal, null, 'no figure is an en dash, not 0');
  assert.equal(m.window, '30d');
  for (const bad of [null, undefined, 'x', 3, [], {}]) {
    const e = shapeSummary(bad, { now: NOW });
    assert.equal(e.total, 0);
    assert.deepEqual(e.days.ts, []);
    assert.equal(e.window, '7d');
    assert.equal(e.errors.total, 0);
  }
  assert.equal(summaryWindow('1y'), '7d');
  assert.equal(summaryWindow('24h'), '24h');
});

// ---- the log filters

test('the filter state is read from the URL hash query and checked: an unknown range or status falls back, a since / until implies custom', () => {
  assert.deepEqual(logState({}), { range: 'any', since: null, until: null, actor: '', status: '', system: '' });
  const s = logState({ range: '7d', actor: 'ops', status: 'error', system: 'dynamo' });
  assert.deepEqual([s.range, s.actor, s.status, s.system], ['7d', 'ops', 'error', 'dynamo']);
  assert.equal(logState({ since: '1790000000' }).range, 'custom');
  assert.equal(logState({ range: 'forever' }).range, 'any');
  assert.equal(logState({ status: 'boom' }).status, '');
  assert.equal(logState({ since: 'abc', until: '-5' }).since, null);
  assert.equal(logState({ actor: 'x'.repeat(500) }).actor.length, 128);
});

test('the query GET /audit takes: empty fields are left out, a preset range is since = now - range, custom uses its own since and until', () => {
  const st = (o) => logState(o);
  assert.deepEqual(logQuery(st({}), NOW), { limit: LOG_LIMIT });
  assert.deepEqual(logQuery(st({ range: '24h', actor: 'ops', status: 'ok', system: 'dynamo' }), NOW), { limit: 200, system: 'dynamo', actor: 'ops', status: 'ok', since: NOW - DAY });
  assert.deepEqual(logQuery(st({ range: 'custom', since: '1790000000', until: '1790003600' }), NOW), { limit: 200, since: 1790000000, until: 1790003600 });
  assert.deepEqual(logQuery(st({ range: 'custom' }), NOW), { limit: 200 }, 'a custom range with no ends adds nothing');
  assert.equal('since' in logQuery(st({ range: 'any' }), NOW), false);
  assert.equal(logQuery(st({}), NOW, 50).limit, 50);
  assert.equal(logFiltered(st({})), false);
  assert.equal(logFiltered(st({ actor: 'a' })), true);
  assert.equal(logFiltered(st({ range: '1h' })), true);
});

test('a datetime-local value is read as UTC and written back the same', () => {
  const t = parseUtcInput('2026-11-14T08:30');
  assert.equal(t, Date.UTC(2026, 10, 14, 8, 30) / 1000);
  assert.equal(toUtcInput(t), '2026-11-14T08:30');
  assert.equal(parseUtcInput(''), null);
  assert.equal(parseUtcInput('tomorrow'), null);
  assert.equal(toUtcInput(null), '');
});
