// Pure helpers behind the Data Designer runs list and records table (js/dd-records.js).
import test from 'node:test';
import assert from 'node:assert/strict';
import { RECORDS_LIMIT, CELL_MAX, runsFromEvents, liveRuns, verdictKind, pageMeta, visibleColumns, toggleHidden, cellText, truncateCell, rowJson } from '../js/dd-records.js';

const ev = (ts, run_id, verdict, extra = {}) => ({ ts, kind: 'dd.run', summary: `${run_id} ${verdict}`, data: { run_id, verdict, ...extra } });

test('runsFromEvents: one entry per run, the latest event wins, newest first, cap applied', () => {
  const runs = runsFromEvents([
    ev(100, 'b', 'running', { mode: 'create', num_records: 10 }),
    ev(300, 'b', 'succeeded', { mode: 'create', num_records: 10, duration_ms: 5000 }),
    ev(200, 'a', 'failed', { mode: 'preview' }),
  ]);
  assert.deepEqual(runs.map((r) => r.id), ['b', 'a']);
  assert.equal(runs[0].verdict, 'succeeded');
  assert.equal(runs[0].duration_ms, 5000);
  assert.equal(runs[1].verdict, 'failed');
  // A run whose finish event omits fields keeps the start event's values.
  const merged = runsFromEvents([ev(1, 'x', 'running', { mode: 'create', dataset_name: 'ds' }), ev(2, 'x', 'interrupted')])[0];
  assert.equal(merged.mode, 'create');
  assert.equal(merged.dataset_name, 'ds');
  assert.equal(merged.verdict, 'interrupted');
  // Non-dd.run events and events without a run_id are ignored; cap limits the list.
  assert.equal(runsFromEvents([{ ts: 1, kind: 'routing.decision', data: { run_id: 'z' } }, { ts: 2, kind: 'dd.run', data: {} }]).length, 0);
  const many = runsFromEvents(Array.from({ length: 300 }, (_, i) => ev(i, `r${i}`, 'succeeded')), { cap: 5 });
  assert.equal(many.length, 5);
  assert.equal(many[0].id, 'r299');
  // The UI's list is newest-first: an older event must never overwrite a newer verdict.
  const reversed = runsFromEvents([ev(300, 'b', 'succeeded', { mode: 'create' }), ev(100, 'b', 'running')]);
  assert.equal(reversed[0].verdict, 'succeeded');
  assert.equal(reversed[0].ts, 300);
});

test('liveRuns/verdictKind: only "running" is live; unknown verdicts get the muted badge', () => {
  const runs = runsFromEvents([ev(1, 'a', 'running'), ev(2, 'b', 'succeeded'), ev(3, 'c', 'weird')]);
  assert.deepEqual(liveRuns(runs).map((r) => r.id), ['a']);
  assert.equal(verdictKind('succeeded'), 'ok');
  assert.equal(verdictKind('failed'), 'bad');
  assert.equal(verdictKind('interrupted'), 'warn');
  assert.equal(verdictKind('running'), 'info');
  assert.equal(verdictKind('weird'), 'muted');
});

test('pageMeta: clamps the offset and derives the pager flags', () => {
  const pm = pageMeta(120, 50, 0);
  assert.equal(pm.offset, 0); assert.equal(pm.from, 1); assert.equal(pm.to, 50);
  assert.equal(pm.hasPrev, false); assert.equal(pm.hasNext, true); assert.equal(pm.pages, 3);
  assert.deepEqual(pageMeta(120, 50, 50), { offset: 50, from: 51, to: 100, total: 120, hasPrev: true, hasNext: true, pages: 3 });
  const last = pageMeta(120, 50, 100);
  assert.equal(last.to, 120); assert.equal(last.hasPrev, true); assert.equal(last.hasNext, false);
  // Offsets past the end and negatives clamp instead of producing an empty page silently.
  assert.equal(pageMeta(120, 50, 999).offset, 119);
  assert.equal(pageMeta(120, 50, -5).offset, 0);
  // Empty run: no rows, no flags.
  const empty = pageMeta(0, 50, 0);
  assert.equal(empty.from, 0); assert.equal(empty.to, 0); assert.equal(empty.hasPrev, false); assert.equal(empty.hasNext, false);
  // A short last page and non-integer/garbage inputs.
  assert.equal(pageMeta(7, 50, 0).to, 7);
  assert.equal(pageMeta('x', 'y', null).offset, 0);
  assert.equal(pageMeta(3, 0, 0).to, 1); // a bad limit is coerced to 1; nothing NaN leaks
  assert.equal(RECORDS_LIMIT, 50);
});

test('visibleColumns/toggleHidden: show/hide is pure', () => {
  const cols = ['id', 'subject', 'body'];
  let hidden = new Set();
  assert.deepEqual(visibleColumns(cols, hidden), cols);
  hidden = toggleHidden(hidden, 'body');
  assert.deepEqual(visibleColumns(cols, hidden), ['id', 'subject']);
  hidden = toggleHidden(hidden, 'subject');
  assert.deepEqual(visibleColumns(cols, hidden), ['id']);
  hidden = toggleHidden(hidden, 'subject'); // toggle back on
  assert.deepEqual(visibleColumns(cols, hidden), ['id', 'subject']);
  assert.deepEqual(visibleColumns(null, hidden), []);
});

test('cellText/truncateCell/rowJson: null stays "null", objects become JSON, truncation keeps the full text', () => {
  assert.equal(cellText(null), 'null');
  assert.equal(cellText(undefined), '');
  assert.equal(cellText(42), '42');
  assert.equal(cellText(true), 'true');
  assert.equal(cellText('hi'), 'hi');
  assert.equal(cellText({ a: 1 }), '{"a":1}');
  assert.equal(cellText([1, 2]), '[1,2]');
  const long = 'x'.repeat(CELL_MAX + 50);
  const t = truncateCell(long);
  assert.equal(t.text.length, CELL_MAX);
  assert.ok(t.text.endsWith('…'));
  assert.equal(t.full, long);
  assert.deepEqual(truncateCell('short'), { text: 'short', full: 'short' });
  assert.deepEqual(truncateCell(null), { text: '', full: '' });
  assert.equal(rowJson({ id: 1, body: 'a"b' }), '{\n  "id": 1,\n  "body": "a\\"b"\n}');
  const circular = {}; circular.self = circular;
  assert.equal(typeof rowJson(circular), 'string'); // unserializable rows fall back to String()
});
