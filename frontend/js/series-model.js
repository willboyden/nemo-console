// Pure data layer of the series charts (addendum 9.13), no DOM: reads the `GET /systems/{id}/metrics/series` answer,
// turns counters into rates (a negative delta starts a new segment), cuts lines at gaps, picks which lines of a split chart
// are shown (the 4 busiest by default) and encodes that choice for the URL hash. Unit-tested in tests/series-model.test.mjs.
const isNum = (n) => typeof n === 'number' && Number.isFinite(n);

export const SERIES_WINDOWS = ['15m', '1h', '24h', '7d'];
export const DEFAULT_SHOWN = 4; // a split chart shows this many lines until the user picks others
/** A gap longer than this many steps ends a segment: the chart draws a break, never a line across missing data. */
export const GAP_FACTOR = 2.5;
/** Below this many events per second a rate is shown per minute instead (the card says which). */
export const PER_SECOND_FLOOR = 0.1;

/** A counter by name: the metric key (labels ignored) ends in `_total`. */
export const isCounterKey = (key) => /_total$/.test(String(key ?? '').split('{')[0]);
/** The descriptor's `rate: true|false` decides; without it a `_total` key is a rate. */
export const wantsRate = (key, entry) => (typeof entry?.rate === 'boolean' ? entry.rate : isCounterKey(key));

/** Canonical identity of a label set: `k=v,k=v` sorted by name; no labels is ''. The hash stores these. */
export function labelId(label) {
  const e = Object.entries(label || {}).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
  return e.map(([k, v]) => `${k}=${v}`).join(',');
}
/** What the legend shows: the label values joined by ' / ', else `fallback` (the metric's own name). */
export const labelName = (label, fallback = '') => {
  const v = Object.values(label || {}).filter((x) => x != null && x !== '');
  return v.length ? v.join(' / ') : fallback;
};

/**
 * The route's answer as clean series: [{key, label, id, points: [[ts, avg, max]]}] with finite numbers only, in time
 * order, `max` falling back to `avg` when absent. Anything that is not an answer yields [].
 */
export function normalizeAnswer(answer) {
  if (!answer || !Array.isArray(answer.series)) return [];
  return answer.series.map((s) => {
    const pts = (Array.isArray(s.points) ? s.points : [])
      .filter((p) => Array.isArray(p) && isNum(p[0]) && isNum(p[1]))
      .map((p) => [p[0], p[1], isNum(p[2]) ? p[2] : p[1]])
      .sort((a, b) => a[0] - b[0]);
    const label = s.label && typeof s.label === 'object' && !Array.isArray(s.label) ? s.label : null;
    return { key: String(s.key ?? ''), label, id: labelId(label), points: pts };
  });
}

/** Cut a point list into runs where consecutive points are at most GAP_FACTOR steps apart. */
export function splitSegments(points, stepS, factor = GAP_FACTOR) {
  const limit = isNum(stepS) && stepS > 0 ? stepS * factor : Infinity;
  const out = [];
  let cur = [];
  for (const p of points) {
    if (cur.length && p[0] - cur[cur.length - 1][0] > limit) {
      out.push(cur);
      cur = [];
    }
    cur.push(p);
  }
  if (cur.length) out.push(cur);
  return out;
}

/**
 * A counter as a per-second rate: segments of [ts, rate, null]. The rate at a point is (value - previous value) /
 * seconds between them. A gap longer than GAP_FACTOR steps ends a segment (the rate across it would be a smear), and so
 * does a RESET (a negative delta: the counter restarted): the point after a reset starts a new segment and has no rate
 * of its own, so no spike and no negative rate is ever drawn. Segments with no rate (one point) are dropped.
 */
export function rateSegments(points, stepS) {
  const out = [];
  for (const seg of splitSegments(points, stepS)) {
    let cur = [];
    let prev = seg[0];
    for (let i = 1; i < seg.length; i++) {
      const p = seg[i],
        dt = p[0] - prev[0];
      if (dt <= 0) continue;
      const d = p[1] - prev[1];
      if (d < 0) {
        if (cur.length) out.push(cur);
        cur = [];
      } else cur.push([p[0], d / dt, null]);
      prev = p;
    }
    if (cur.length) out.push(cur);
  }
  return out;
}

/** Plain (not rate) series as segments, `max` kept. */
export const valueSegments = (points, stepS) => splitSegments(points, stepS).map((seg) => seg.map((p) => [p[0], p[1], p[2]]));

/** One scale for a whole chart: per second unless the busiest rate is below PER_SECOND_FLOOR, then per minute (x60). */
export function pickRateScale(segmentLists) {
  let peak = 0;
  for (const segs of segmentLists) for (const seg of segs) for (const p of seg) if (isNum(p[1]) && p[1] > peak) peak = p[1];
  return peak > 0 && peak < PER_SECOND_FLOOR ? { factor: 60, per: 'min' } : { factor: 1, per: 's' };
}
export const scaleSegments = (segs, factor) => (factor === 1 ? segs : segs.map((seg) => seg.map((p) => [p[0], p[1] * factor, p[2] == null ? null : p[2] * factor])));

/**
 * Ready-to-draw lines for one chart: `series` from normalizeAnswer, `rate` whether the chart is a rate.
 * Returns {lines: [{key, label, id, segments}], rate: {factor, per} | null}. For a rate the max is not drawn (the max
 * of a counter in a bucket is its last value, not a peak rate), so segments carry max = null.
 */
export function buildLines(series, { rate = false, stepS } = {}) {
  if (!rate) return { lines: series.map((s) => ({ key: s.key, label: s.label, id: s.id, segments: valueSegments(s.points, stepS) })), rate: null };
  const raw = series.map((s) => rateSegments(s.points, stepS));
  const scale = pickRateScale(raw);
  return { lines: series.map((s, i) => ({ key: s.key, label: s.label, id: s.id, segments: scaleSegments(raw[i], scale.factor) })), rate: scale };
}

/** How busy a line is: the sum of its absolute values (area under the curve for equal steps); ties break by id. */
export const activity = (segments) => segments.reduce((a, seg) => a + seg.reduce((b, p) => b + (isNum(p[1]) ? Math.abs(p[1]) : 0), 0), 0);

/** The ids of the `n` busiest lines (all of them when there are no more than n), busiest first. */
export function busiest(lines, n = DEFAULT_SHOWN) {
  return lines
    .map((l) => ({ id: l.id, score: activity(l.segments) }))
    .sort((a, b) => b.score - a.score || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0))
    .slice(0, n)
    .map((x) => x.id);
}

/**
 * Which lines to draw. `saved` is the user's choice (an array of ids, from the URL hash) or null. A line that no
 * longer exists is dropped; a choice that matches nothing falls back to the busiest n. An empty saved array is a
 * deliberate "none" and is kept. The result keeps the order of `lines`.
 */
export function resolveSelection(lines, saved, n = DEFAULT_SHOWN) {
  const have = new Set(lines.map((l) => l.id));
  let ids;
  if (Array.isArray(saved)) {
    const kept = saved.filter((id) => have.has(id));
    ids = kept.length || saved.length === 0 ? kept : busiest(lines, n);
  } else ids = busiest(lines, n);
  const pick = new Set(ids);
  return lines.filter((l) => pick.has(l.id)).map((l) => l.id);
}

const SEP = '|';
/** `a|b` for the URL hash; '-' means none. Ids never contain '|' (labels are limited to the metric-key alphabet). */
export const encodeSelection = (ids) => (ids.length ? ids.join(SEP) : '-');
/** The inverse; undefined/'' (no choice made) is null, '-' is the empty array. */
export function decodeSelection(s) {
  if (s == null || s === '') return null;
  if (s === '-') return [];
  return String(s).split(SEP).filter(Boolean);
}

/** {n, latest, min, avg, max} over a line's points (the text alternative of a chart); null values everywhere when it has none. */
export function lineStats(segments) {
  const pts = segments.flat().filter((p) => isNum(p[1]));
  if (!pts.length) return { n: 0, latest: null, min: null, avg: null, max: null };
  const v = pts.map((p) => p[1]);
  return {
    n: pts.length,
    latest: pts[pts.length - 1][1],
    min: Math.min(...v),
    avg: v.reduce((a, b) => a + b, 0) / v.length,
    max: Math.max(...pts.map((p) => (isNum(p[2]) ? Math.max(p[2], p[1]) : p[1]))),
  };
}

/** Whether any point of any line has a max above its average (false for the 15-minute raw points). */
export const hasSpread = (lines) => lines.some((l) => l.segments.some((seg) => seg.some((p) => isNum(p[2]) && p[2] > p[1] + 1e-9)));

/** Color slot (0..5) and dash pattern (0 solid, 1 dash, 2 dot) of the i-th line of a chart: six colours, then dashes. */
export const lineStyle = (i) => ({ color: (i % 6) + 1, dash: Math.floor(i / 6) % 3 });
