// Data Designer column profile (addendum 9.15), from `GET /systems/datadesigner/runs/{id}/profile`. ROLE: the most frequent VALUES of a column are model-written
// content, so they are for an admin only: the route sends top null and values_hidden true to a viewer, and the page must not show them even if a (wrong) answer
// carried them. Aggregates and column names are shown to everyone.
import test from 'node:test';
import assert from 'node:assert/strict';
import { shapeProfile, typeMixRows, topColumns, topCoverage, TYPE_KEYS, MAX_TOP, MAX_COLUMNS, latestPerRun, ddTimelineRows } from '../js/dd-profile-model.js';
import { ddProfile } from '../dev/mock-wave3.mjs';
import { ddRows } from '../dev/mock-data.mjs';

const col = (name, over = {}) => ({
  name,
  types: { string: 10, number: 0, bool: 0, null: 0, object: 0, array: 0 },
  non_null: 10,
  distinct: 3,
  distinct_capped: false,
  length: { min: 2, p50: 5, p95: 9, max: 12 },
  numeric: null,
  top: [
    { value: 'secret answer', count: 6 },
    { value: 'other', count: 4 },
  ],
  ...over,
});
const answer = (over = {}) => ({ run_id: 'r1', rows: 10, columns: [col('subject')], values_hidden: false, truncated: false, ...over });

test('a viewer never gets the top values, even from an answer that carries them', () => {
  const viewer = shapeProfile(answer({ values_hidden: true }), { admin: false });
  assert.equal(viewer.valuesHidden, true);
  assert.deepEqual(viewer.columns[0].top, []);
  assert.equal(viewer.topShown, false);
  const lying = shapeProfile(answer({ values_hidden: false }), { admin: false });
  assert.deepEqual(lying.columns[0].top, [], 'the role decides, not only the answer');
  assert.equal(lying.valuesHidden, true);
  assert.deepEqual(topColumns(lying), []);
  assert.ok(!JSON.stringify(viewer).includes('secret answer'), 'the withheld value is nowhere in what the page keeps');
  assert.ok(!JSON.stringify(lying).includes('secret answer'));
});

test('an admin gets them (as text, at most ten per column) unless the answer itself says they are hidden', () => {
  const admin = shapeProfile(answer(), { admin: true });
  assert.equal(admin.valuesHidden, false);
  assert.deepEqual(admin.columns[0].top, [
    { value: 'secret answer', count: 6 },
    { value: 'other', count: 4 },
  ]);
  assert.equal(admin.topShown, true);
  assert.equal(topColumns(admin).length, 1);
  const hidden = shapeProfile(answer({ values_hidden: true }), { admin: true });
  assert.equal(hidden.valuesHidden, true, 'an answer that says hidden is believed');
  assert.deepEqual(hidden.columns[0].top, []);
  const many = shapeProfile(answer({ columns: [col('x', { top: Array.from({ length: 25 }, (_, i) => ({ value: `v${i}`, count: 1 })) })] }), { admin: true });
  assert.equal(many.columns[0].top.length, MAX_TOP);
  const mixed = shapeProfile(answer({ columns: [col('x', { top: [{ value: 5, count: 2 }, { value: true, count: 1 }, { value: { a: 1 }, count: 9 }, { value: 'ok', count: 'x' }, null] })] }), {
    admin: true,
  });
  assert.deepEqual(
    mixed.columns[0].top,
    [
      { value: '5', count: 2 },
      { value: 'true', count: 1 },
    ],
    'numbers and booleans become text; objects and junk are dropped',
  );
  const none = shapeProfile(answer({ columns: [col('x', { top: null })] }), { admin: true });
  assert.deepEqual(none.columns[0].top, []);
  assert.equal(none.topShown, false);
});

test('the numbers: null rate from non_null and rows, distinct (capped), length and numeric ranges, a missing figure is null not 0', () => {
  const p = shapeProfile(
    answer({
      rows: 120,
      columns: [
        col('csat', { types: { string: 0, number: 102, bool: 0, null: 18, object: 0, array: 0 }, non_null: 102, distinct: 5, length: null, numeric: { min: 1, p50: 3, p95: 5, max: 5 } }),
        col('body', { distinct: 1000, distinct_capped: true }),
        { name: 'bare' },
      ],
    }),
    { admin: false },
  );
  const [csat, body, bare] = p.columns;
  assert.ok(Math.abs(csat.nullRate - 0.15) < 1e-9);
  assert.equal(csat.dominant, 'number');
  assert.deepEqual(csat.numeric, { min: 1, p50: 3, p95: 5, max: 5 });
  assert.equal(csat.length, null);
  assert.equal(body.distinctCapped, true);
  assert.equal(body.distinct, 1000);
  assert.equal(bare.nullRate, null, 'no figures: no rate');
  assert.equal(bare.distinct, null);
  assert.equal(bare.nonNull, null);
  assert.equal(bare.dominant, null);
  assert.equal(bare.typeTotal, 0);
  assert.equal(p.rows, 120);
  const cut = shapeProfile(answer({ columns: [col('x', { length: { min: 1, p50: null, p95: 'x', max: 9 } })] }), { admin: false }).columns[0].length;
  assert.deepEqual(cut, { min: 1, p50: null, p95: null, max: 9 });
  assert.equal(shapeProfile(answer({ columns: [col('x', { length: { min: null, p50: null, p95: null, max: null } })] }), { admin: false }).columns[0].length, null);
  assert.equal(shapeProfile(answer({ rows: 0, columns: [] }), { admin: false }).rows, 0);
  assert.equal(shapeProfile(null, { admin: false }).rows, null);
  assert.deepEqual(shapeProfile({}, {}).columns, []);
});

test("truncated is the route's flag; columns without a name are dropped; more than MAX_COLUMNS are cut", () => {
  assert.equal(shapeProfile(answer({ truncated: true }), { admin: false }).truncated, true);
  assert.equal(shapeProfile(answer(), { admin: false }).truncated, false);
  assert.equal(shapeProfile(answer({ columns: [col(''), { types: {} }, col('ok')] }), { admin: false }).columns.length, 1);
  assert.equal(shapeProfile(answer({ columns: Array.from({ length: MAX_COLUMNS + 20 }, (_, i) => col(`c${i}`)) }), { admin: false }).columns.length, MAX_COLUMNS);
});

test("typeMixRows: a row per column with a value, the six types in the answer's own counts", () => {
  const p = shapeProfile(
    answer({
      columns: [col('a', { types: { string: 3, number: 2, bool: 0, null: 5, object: 0, array: 0 } }), col('empty', { types: { string: 0, number: 0, bool: 0, null: 0, object: 0, array: 0 } })],
    }),
    { admin: false },
  );
  const rows = typeMixRows(p);
  assert.equal(rows.length, 1);
  assert.deepEqual(Object.keys(rows[0].values), TYPE_KEYS);
  assert.equal(rows[0].values.null, 5);
  assert.equal(rows[0].totalText, '10');
});

test('topCoverage: the share of the non-null values the top values cover; null when it cannot be told', () => {
  const p = shapeProfile(answer(), { admin: true });
  assert.equal(topCoverage(p.columns[0]), 1);
  assert.ok(Math.abs(topCoverage({ top: [{ value: 'a', count: 2 }], nonNull: 8 }) - 0.25) < 1e-9);
  assert.equal(topCoverage({ top: [], nonNull: 8 }), null);
  assert.equal(topCoverage({ top: [{ value: 'a', count: 2 }], nonNull: null }), null);
});

test('the mock profile: an admin gets top values, a viewer gets null and values_hidden; the counts add up to the rows; missing keys count as null', () => {
  const rows = ddRows(60);
  rows.push({ id: 61, subject: 'no priority key here' }); // a record that lacks keys the others have
  const run = { run_id: 'r', columns: ['id', 'priority', 'csat', 'tags'], rows };
  const admin = ddProfile(run, { admin: true });
  const viewer = ddProfile(run, { admin: false });
  assert.equal(admin.values_hidden, false);
  assert.equal(viewer.values_hidden, true);
  assert.equal(admin.truncated, false);
  assert.ok(admin.columns.find((c) => c.name === 'priority').top.length >= 1);
  assert.ok(
    viewer.columns.every((c) => c.top === null),
    'a viewer is never sent a value',
  );
  assert.ok(!JSON.stringify(viewer).includes('"high"'), 'not even by accident');
  for (const c of admin.columns)
    assert.equal(
      Object.values(c.types).reduce((a, b) => a + b, 0),
      admin.rows,
      `${c.name}: the type counts add up to the rows`,
    );
  const pr = admin.columns.find((c) => c.name === 'priority');
  assert.equal(pr.types.null, 1, 'the missing key');
  assert.equal(pr.non_null, 60);
  assert.deepEqual(admin.columns.find((c) => c.name === 'csat').numeric.min, 1);
  assert.equal(admin.columns.find((c) => c.name === 'tags').types.array, 60);
  assert.deepEqual(ddProfile({ run_id: 'e', rows: [], columns: [] }, { admin: true }), { run_id: 'e', rows: 0, columns: [], values_hidden: false, truncated: false });
});

test('latestPerRun keeps the newest event of each run id; ddTimelineRows: the duration, or the age of a running run, requested vs written, nothing as 0', () => {
  const e = (id, ts, over = {}) => ({ ts, kind: 'dd.run', data: { run_id: id, ...over } });
  const latest = latestPerRun([e('a', 10, { verdict: 'running' }), e('a', 20, { verdict: 'succeeded' }), e('b', 5), { ts: 1, data: {} }, null]);
  assert.deepEqual(
    latest.map((x) => [x.data.run_id, x.ts]),
    [
      ['a', 20],
      ['b', 5],
    ],
  );
  const rows = [
    { id: 'a', label: 'a', ts: 100, verdict: 'succeeded', vclass: 'ok', fields: { script: 's.py', mode: 'create', duration_ms: 5000, num_records: 10, num_records_requested: 10 } },
    { id: 'b', label: 'b', ts: 200, verdict: 'running', vclass: 'info', fields: { script: 's.py', mode: 'create' } },
    { id: 'c', label: 'c', ts: 300, verdict: 'failed', vclass: 'bad', fields: { duration_ms: null, num_records: 0 } },
  ];
  const t = ddTimelineRows(rows, 260 * 1000);
  assert.equal(t[0].durationMs, 5000);
  assert.equal(t[0].requested, 10);
  assert.equal(t[1].running, true);
  assert.equal(t[1].durationMs, 60 * 1000, 'the age of a running run');
  assert.equal(t[1].written, null);
  assert.equal(t[2].durationMs, null);
  assert.equal(t[2].written, 0, 'a failed run that wrote nothing wrote zero records: that one is a real zero');
  assert.equal(t[2].requested, null);
  assert.equal(ddTimelineRows(rows, 0, { max: 1 }).length, 1);
});
