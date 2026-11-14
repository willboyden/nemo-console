// The series data layer (addendum 9.13): rates and resets, gaps, the busiest-4 default, the picker choice and its hash form.
import test from 'node:test';
import assert from 'node:assert/strict';
import {
  isCounterKey,
  wantsRate,
  labelId,
  labelName,
  normalizeAnswer,
  splitSegments,
  rateSegments,
  valueSegments,
  pickRateScale,
  buildLines,
  activity,
  busiest,
  resolveSelection,
  encodeSelection,
  decodeSelection,
  lineStats,
  hasSpread,
  lineStyle,
  SERIES_WINDOWS,
  DEFAULT_SHOWN,
} from '../js/series-model.js';

const pts = (...v) => v.map(([t, a, m]) => [t, a, m ?? a]);

test('counters: a key ending in _total is a rate by default, a descriptor entry decides otherwise', () => {
  assert.equal(isCounterKey('flare.generated_total'), true);
  assert.equal(isCounterKey('dcgm.xid_total{gpu=0,model=x}'), true, 'labels are ignored');
  assert.equal(isCounterKey('ap.runs_by_verdict'), false);
  assert.equal(isCounterKey('x_total_y'), false);
  assert.equal(isCounterKey(undefined), false);
  assert.equal(wantsRate('flare.generated_total', {}), true);
  assert.equal(wantsRate('flare.generated_total', { rate: false }), false);
  assert.equal(wantsRate('mo.output_bytes', { rate: true }), true);
  assert.equal(wantsRate('mo.output_bytes'), false);
});

test('windows include 7d and the default shown count is 4', () => {
  assert.deepEqual(SERIES_WINDOWS, ['15m', '1h', '24h', '7d']);
  assert.equal(DEFAULT_SHOWN, 4);
});

test('label identity is canonical (sorted) and the legend name falls back to the metric name', () => {
  assert.equal(labelId({ model: 'x', gpu: '0' }), 'gpu=0,model=x');
  assert.equal(labelId(null), '');
  assert.equal(labelName({ gpu: '0', model: 'RTX' }, 'fb'), '0 / RTX');
  assert.equal(labelName(null, 'GPU memory used'), 'GPU memory used');
  assert.equal(labelName({}, 'x'), 'x');
});

test('normalizeAnswer keeps finite numbers only, sorts by time, defaults max to the average, and survives junk', () => {
  const s = normalizeAnswer({
    series: [
      { key: 'a', label: { gpu: '1' }, points: [[20, 2, 3], [10, 1], [30, null, 1], ['x', 1], [40, NaN, 2], 'junk'] },
      { key: 'b', label: null, points: [] },
      { key: 'c', label: ['not', 'an', 'object'], points: [[1, 5, 4]] },
    ],
  });
  assert.deepEqual(s[0].points, [
    [10, 1, 1],
    [20, 2, 3],
  ]);
  assert.equal(s[0].id, 'gpu=1');
  assert.equal(s[1].label, null);
  assert.deepEqual(s[1].points, []);
  assert.equal(s[2].label, null, 'an array is not a label object');
  assert.deepEqual(normalizeAnswer(null), []);
  assert.deepEqual(normalizeAnswer({}), []);
});

test('gaps: a hole longer than 2.5 steps ends the segment', () => {
  const segs = splitSegments(pts([0, 1], [10, 1], [20, 1], [100, 1], [110, 1]), 10);
  assert.equal(segs.length, 2);
  assert.deepEqual(
    segs.map((s) => s.length),
    [3, 2],
  );
  assert.equal(splitSegments(pts([0, 1], [10, 1], [24, 1]), 10).length, 1, '24 is 1.4 steps after 10: still one line');
  assert.equal(splitSegments(pts([0, 1], [10, 1], [36, 1]), 10).length, 2);
  assert.equal(splitSegments(pts([0, 1], [1000, 1]), undefined).length, 1, 'without a step nothing is cut');
});

test('rate: (value - previous) / seconds, per point, the first point has none', () => {
  const r = rateSegments(pts([0, 100], [10, 200], [20, 400]), 10);
  assert.deepEqual(r, [
    [
      [10, 10, null],
      [20, 20, null],
    ],
  ]);
});

test('rate: a flat counter is rate 0 (drawn), not dropped', () => {
  const r = rateSegments(pts([0, 5], [10, 5], [20, 5]), 10);
  assert.deepEqual(
    r[0].map((p) => p[1]),
    [0, 0],
  );
});

test('rate: a reset (negative delta) starts a new segment: no spike, no negative rate, the point after has no rate of its own', () => {
  const r = rateSegments(pts([0, 100], [10, 200], [20, 5], [30, 15], [40, 25]), 10);
  assert.equal(r.length, 2);
  assert.deepEqual(r[0], [[10, 10, null]]);
  assert.deepEqual(r[1], [
    [30, 1, null],
    [40, 1, null],
  ]);
  assert.ok(
    r.flat().every((p) => p[1] >= 0),
    'never negative',
  );
});

test('rate: two resets in a row, a reset at the very end, and a lone point', () => {
  assert.deepEqual(rateSegments(pts([0, 50], [10, 10], [20, 3]), 10), [], 'every delta negative: no rate at all');
  assert.deepEqual(rateSegments(pts([0, 1]), 10), []);
  assert.deepEqual(rateSegments([], 10), []);
  const end = rateSegments(pts([0, 10], [10, 20], [20, 0]), 10);
  assert.deepEqual(end, [[[10, 1, null]]]);
});

test('rate: a gap also ends the segment (no smear across missing data) and keeps each side', () => {
  const r = rateSegments(pts([0, 0], [10, 10], [20, 20], [500, 9999], [510, 10099]), 10);
  assert.equal(r.length, 2);
  assert.deepEqual(
    r[0].map((p) => p[1]),
    [1, 1],
  );
  assert.deepEqual(
    r[1].map((p) => p[1]),
    [10],
  );
});

test('rate: fractional seconds between points (the 15-minute raw points)', () => {
  const r = rateSegments(pts([0, 0], [2, 3], [4, 9]), 2);
  assert.deepEqual(
    r[0].map((p) => p[1]),
    [1.5, 3],
  );
});

test('the rate scale is per second unless the busiest rate is below 0.1 per second, then per minute', () => {
  const a = [
    [
      [
        [0, 0.02, null],
        [10, 0.05, null],
      ],
    ],
  ];
  assert.deepEqual(pickRateScale(a), { factor: 60, per: 'min' });
  assert.deepEqual(pickRateScale([[[[0, 4, null]]]]), { factor: 1, per: 's' });
  assert.deepEqual(pickRateScale([[[[0, 0, null]]]]), { factor: 1, per: 's' }, 'all zero stays per second');
  assert.deepEqual(pickRateScale([]), { factor: 1, per: 's' });
  const b = buildLines(
    normalizeAnswer({
      series: [
        {
          key: 'k_total',
          label: null,
          points: [
            [0, 0],
            [10, 0.5],
            [20, 1],
          ],
        },
      ],
    }),
    { rate: true, stepS: 10 },
  );
  assert.equal(b.rate.per, 'min');
  assert.deepEqual(
    b.lines[0].segments[0].map((p) => +p[1].toFixed(6)),
    [3, 3],
    '0.05 per second shown as 3 per minute',
  );
});

test('buildLines: a plain chart keeps the max, a rate chart carries none', () => {
  const ans = {
    series: [
      {
        key: 'k',
        label: null,
        points: [
          [0, 1, 2],
          [10, 2, 5],
        ],
      },
    ],
  };
  const plain = buildLines(normalizeAnswer(ans), { rate: false, stepS: 10 });
  assert.equal(plain.rate, null);
  assert.deepEqual(plain.lines[0].segments, [
    [
      [0, 1, 2],
      [10, 2, 5],
    ],
  ]);
  assert.equal(hasSpread(plain.lines), true);
  const rate = buildLines(normalizeAnswer(ans), { rate: true, stepS: 10 });
  assert.equal(rate.lines[0].segments[0][0][2], null);
  assert.equal(hasSpread(rate.lines), false);
  assert.equal(
    hasSpread(
      buildLines(
        normalizeAnswer({
          series: [
            {
              key: 'k',
              points: [
                [0, 1],
                [10, 2],
              ],
            },
          ],
        }),
        {},
      ).lines,
    ),
    false,
    'raw points: max equals the value',
  );
});

const seriesOf = (specs) => specs.map(([id, ...v]) => ({ id, segments: [v.map((x, i) => [i, x, x])] }));

test('the 4 busiest of 6 lines are the default; ties break by id; the order is busiest first', () => {
  const lines = seriesOf([
    ['a', 1, 1],
    ['b', 9, 9],
    ['c', 5, 5],
    ['d', 5, 5],
    ['e', 0.5],
    ['f', 7, 7],
  ]);
  assert.deepEqual(busiest(lines, 4), ['b', 'f', 'c', 'd']);
  assert.deepEqual(busiest(lines.slice(0, 3), 4), ['b', 'c', 'a'], 'fewer lines than n: all of them');
  assert.equal(
    activity([
      [
        [0, -3, -3],
        [1, 2, 2],
      ],
    ]),
    5,
    'absolute values',
  );
  assert.deepEqual(resolveSelection(lines, null), ['b', 'c', 'd', 'f'], 'the result keeps the order of the lines');
});

test('a saved choice: unknown ids are dropped, nothing left falls back to the busiest, an empty choice is kept', () => {
  const lines = seriesOf([
    ['a', 1],
    ['b', 9],
    ['c', 5],
    ['d', 5],
    ['e', 4],
    ['f', 7],
  ]);
  assert.deepEqual(resolveSelection(lines, ['e', 'a']), ['a', 'e']);
  assert.deepEqual(resolveSelection(lines, ['e', 'ghost']), ['e']);
  assert.deepEqual(resolveSelection(lines, ['ghost']), ['b', 'c', 'd', 'f'], 'a stale choice shows the default, not an empty chart');
  assert.deepEqual(resolveSelection(lines, []), [], 'deliberately none');
  assert.deepEqual(resolveSelection([], null), []);
});

test('the choice round-trips through the hash form, and none is a dash', () => {
  const ids = ['gpu=0,model=RTX PRO', 'gpu=1,model=RTX PRO'];
  assert.deepEqual(decodeSelection(encodeSelection(ids)), ids);
  assert.equal(encodeSelection([]), '-');
  assert.deepEqual(decodeSelection('-'), []);
  assert.equal(decodeSelection(''), null);
  assert.equal(decodeSelection(undefined), null);
  assert.deepEqual(decodeSelection('a||b|'), ['a', 'b'], 'empty parts are dropped');
});

test('line statistics and the colour / dash slot of the i-th line', () => {
  const st = lineStats([
    [
      [0, 1, 4],
      [10, 3, 3],
    ],
    [[20, 2, 2]],
  ]);
  assert.deepEqual(st, { n: 3, latest: 2, min: 1, avg: 2, max: 4 });
  assert.deepEqual(lineStats([]), { n: 0, latest: null, min: null, avg: null, max: null });
  assert.deepEqual(lineStyle(0), { color: 1, dash: 0 });
  assert.deepEqual(lineStyle(5), { color: 6, dash: 0 });
  assert.deepEqual(lineStyle(6), { color: 1, dash: 1 });
  assert.deepEqual(lineStyle(13), { color: 2, dash: 2 });
  assert.deepEqual(lineStyle(18), { color: 1, dash: 0 }, 'after 18 the styles repeat');
  assert.deepEqual(valueSegments(pts([0, 1], [10, 2]), 10), [
    [
      [0, 1, 1],
      [10, 2, 2],
    ],
  ]);
});

test('descriptor chart entries: a string is {key}; an object may carry rate / split; pickChartKeys reads both', async () => {
  const { chartEntries, pickChartKeys, groupPoints } = await import('../js/metric-groups.js');
  const desc = { charts: ['a.x', { key: 'a.y_total', rate: false }, { key: 'a.z', split: true }, 'a.missing'] };
  assert.deepEqual(chartEntries(desc), [{ key: 'a.x' }, { key: 'a.y_total', rate: false }, { key: 'a.z', split: true }, { key: 'a.missing' }]);
  const g = groupPoints({ points: ['a.x', 'a.y_total', 'a.z'].map((key) => ({ key, value: 1, labels: {} })) });
  assert.deepEqual(pickChartKeys(desc, g), ['a.x', 'a.y_total', 'a.z']);
  assert.equal(wantsRate('a.y_total', chartEntries(desc)[1]), false, 'the entry overrides the _total default');
  assert.deepEqual(chartEntries({}), []);
});
