// Pure shaping of the OpenShell cards (addendum 9.16): sandbox lifecycle swim lanes from the persisted `openshell.sandbox` events, the phase-count stacked area from
// `os.sandboxes_by_phase`, and the per-sandbox policy-decision timeline from `GET /systems/openshell/decisions`. No DOM, no i18n. A phase whose start was not recorded keeps its
// edge honest (`startUnknown`); a count the answer does not have is null, never 0. Read-only: nothing here changes a sandbox or a policy. Unit-tested in tests/openshell-model.test.mjs.
import { normalizeAnswer } from './series-model.js';
import { baseKey, finite, gridSize, alignToGrid, mergeFactor, rebin, rebinTimes } from './series-math.js';

const isNum = (n) => typeof n === 'number' && Number.isFinite(n);

/** The phase buckets of `os.sandboxes_by_phase`, in the order they are stacked (openshell.rs PHASES). */
export const PHASES = ['Provisioning', 'Ready', 'Error', 'Deleting', 'Other'];
export const EVENT_KIND = 'openshell.sandbox';
/** The most lanes drawn: more sandboxes than this and the newest are kept (the card says how many are left out). */
export const MAX_LANES = 14;
export const DECISION_LIMIT = 1000;
/** A log with fewer decisions than this is drawn as a list of them: a timeline of two marks says nothing (real logs often hold little more than the SSH open). */
export const SPARSE_MAX = 8;
export const MAX_DECISION_LANES = 8;
export const DECISION_BINS = 60;

/** openshell.rs `phase_bucket`: a case-insensitive match of the four known phases, else Other. */
export function phaseBucket(phase) {
  const p = String(phase ?? '')
    .trim()
    .toLowerCase();
  return PHASES.slice(0, 4).find((k) => k.toLowerCase() === p) ?? 'Other';
}

/** Lifecycle events of the list, oldest first (ties keep their order): only well-formed `openshell.sandbox` events with a sandbox name. */
export function lifecycleEvents(events) {
  const out = [];
  (Array.isArray(events) ? events : []).forEach((ev, i) => {
    const d = ev && ev.data && typeof ev.data === 'object' ? ev.data : null;
    if (!ev || ev.kind !== EVENT_KIND || !d || typeof d.name !== 'string' || !d.name || !isNum(ev.ts)) return;
    out.push({
      i,
      ts: ev.ts,
      name: d.name,
      id: typeof d.id === 'string' ? d.id : null,
      action: typeof d.action === 'string' ? d.action : null,
      phase: typeof d.phase === 'string' && d.phase ? d.phase : null,
      previous: typeof d.previous_phase === 'string' && d.previous_phase ? d.previous_phase : null,
    });
  });
  return out.sort((a, b) => a.ts - b.ts || b.i - a.i); // the API lists newest first: for equal times the later list position is the earlier event
}

/**
 * The sandboxes that exist now, by their events: a name whose last event is not a deletion. Newest first (by that last event).
 * (A decision log can only be read for a sandbox that is in the gateway's list.)
 */
export function presentSandboxes(events) {
  const last = new Map();
  for (const e of lifecycleEvents(events)) last.set(e.name, e);
  return [...last.values()]
    .filter((e) => e.action !== 'deleted')
    .sort((a, b) => b.ts - a.ts)
    .map((e) => ({ name: e.name, id: e.id, phase: e.phase, since: e.ts }));
}

/**
 * Swim lanes from the events: one lane per sandbox name, coloured phase segments from one event to the next (create -> ready -> delete), clipped to [from, to].
 * `now` closes the segment of a sandbox that has not been deleted. A first event that is a phase change or a deletion has no start for the phase before it: that segment
 * starts at the window's edge and says so (`startUnknown`). Returns {from, to, lanes: [{name, id, present, segments: [{phase, bucket, from, to, open, startUnknown, clippedStart}],
 * marks: [{ts, action, phase}], createdTs, readyTs, deletedTs, provisionS, lifetimeS, last}], hidden (lanes left out beyond maxLanes), total}.
 */
export function lifecycleLanes(events, { from, to, now = to, maxLanes = MAX_LANES } = {}) {
  const byName = new Map();
  for (const e of lifecycleEvents(events)) {
    if (!byName.has(e.name)) byName.set(e.name, []);
    byName.get(e.name).push(e);
  }
  const lanes = [];
  for (const [name, evs] of byName) {
    const segs = [],
      marks = [];
    let cur = null; // {phase, since, startUnknown}
    let id = null,
      createdTs = null,
      deletedTs = null,
      readyTs = null;
    const close = (end, open = false) => {
      if (!cur) return;
      segs.push({ phase: cur.phase, bucket: phaseBucket(cur.phase), from: cur.since, to: Math.max(cur.since, end), open, startUnknown: cur.startUnknown });
      cur = null;
    };
    for (const e of evs) {
      id = e.id ?? id;
      if (e.action === 'created' || e.action === 'present') {
        close(e.ts);
        cur = { phase: e.phase ?? 'Other', since: e.ts, startUnknown: false };
        createdTs = e.ts;
        deletedTs = null;
        marks.push({ ts: e.ts, action: e.action, phase: e.phase });
      } else if (e.action === 'deleted') {
        if (!cur) cur = { phase: e.phase ?? e.previous ?? 'Other', since: from ?? e.ts, startUnknown: true };
        close(e.ts);
        deletedTs = e.ts;
        marks.push({ ts: e.ts, action: 'deleted', phase: e.phase });
      } else {
        // a phase change: the phase before it ran until now; with no earlier event its start was not recorded
        if (!cur && e.previous) cur = { phase: e.previous, since: Math.min(from ?? e.ts, e.ts), startUnknown: true };
        close(e.ts);
        cur = { phase: e.phase ?? 'Other', since: e.ts, startUnknown: false };
        marks.push({ ts: e.ts, action: 'phase', phase: e.phase });
      }
      if (cur && phaseBucket(cur.phase) === 'Ready' && readyTs == null && !cur.startUnknown) readyTs = cur.since;
    }
    const present = !!cur;
    if (cur) close(Math.max(now, cur.since), true);
    const lo = finite(from) ? from : -Infinity,
      hi = finite(to) ? to : Infinity;
    // clip to the window
    const clipped = segs
      .filter((s) => s.to >= lo && s.from <= hi)
      .map((s) => ({ ...s, from: Math.max(s.from, lo), to: Math.min(s.to, hi), clippedStart: s.from < lo, clippedEnd: s.to > hi }))
      .filter((s) => s.to > s.from || s.open);
    const visibleMarks = marks.filter((m) => m.ts >= lo && m.ts <= hi);
    if (!clipped.length && !visibleMarks.length) continue;
    lanes.push({
      name,
      id,
      present,
      segments: clipped,
      marks: visibleMarks,
      createdTs,
      readyTs,
      deletedTs,
      provisionS: createdTs != null && readyTs != null && readyTs >= createdTs ? readyTs - createdTs : null,
      lifetimeS: createdTs != null ? (deletedTs ?? (present ? now : null)) - createdTs : null,
      last: evs.at(-1).ts,
    });
  }
  // oldest activity first, so a long-lived sandbox is on top and the newest at the bottom; the newest `maxLanes` stay
  lanes.sort((a, b) => (a.segments[0]?.from ?? a.marks[0]?.ts ?? 0) - (b.segments[0]?.from ?? b.marks[0]?.ts ?? 0) || (a.name < b.name ? -1 : 1));
  const total = lanes.length;
  const kept = lanes.length > maxLanes ? [...lanes].sort((a, b) => b.last - a.last).slice(0, maxLanes) : lanes;
  const keep = new Set(kept.map((l) => l.name));
  return { from: from ?? null, to: to ?? null, lanes: lanes.filter((l) => keep.has(l.name)), hidden: total - kept.length, total };
}

/**
 * `os.sandboxes_by_phase` (a series answer with split=1) as the stacked area: one part per phase bucket on one grid, merged to at most `maxCols` columns (the mean of the
 * counts in a column). A bucket with no sample is null in every part (a gap), a real "0 sandboxes" is 0. Returns {ts, bucketS, series: [{key, values}], from, to, any, peak, latest: {phase: n|null}}.
 */
export function phaseStack(answer, { maxCols = 120 } = {}) {
  const a = answer && typeof answer === 'object' ? answer : {};
  const lines = normalizeAnswer(a).filter((s) => baseKey(s.key) === 'os.sandboxes_by_phase');
  const stepS = a.step_s;
  const n = gridSize(a.from, a.to, stepS);
  const factor = mergeFactor(n, maxCols);
  const byPhase = new Map();
  for (const s of lines) byPhase.set(String(s.label?.phase ?? ''), s);
  const parts = PHASES.map((p) => {
    const s = byPhase.get(p);
    const raw = s && n ? alignToGrid(s.points, { from: a.from, stepS, n }) : null;
    return { key: p, raw, values: raw ? rebin(raw, factor) : null };
  }).filter((p) => p.values);
  const peak = parts.length && n ? Math.max(0, ...rebinTimes(a.from, stepS, n, factor).map((_, i) => parts.reduce((acc, p) => acc + (finite(p.values[i]) ? p.values[i] : 0), 0))) : null;
  const latest = {};
  for (const p of PHASES) {
    const part = parts.find((x) => x.key === p);
    let v = null;
    if (part)
      for (let i = part.raw.length - 1; i >= 0; i--)
        if (finite(part.raw[i])) {
          v = part.raw[i];
          break;
        }
    latest[p] = v;
  }
  return {
    ts: n ? rebinTimes(a.from, stepS, n, factor) : [],
    bucketS: finite(stepS) ? stepS * factor : 0,
    series: parts.map((p) => ({ key: p.key, values: p.values })),
    from: a.from ?? null,
    to: a.to ?? null,
    any: parts.some((p) => p.values.some(finite)),
    peak,
    latest,
  };
}

// ---------------------------------------------------------------- decisions

/**
 * The decision rows of `GET /systems/openshell/decisions` as the timeline: lanes by category (the busiest MAX_DECISION_LANES, the rest in `other`), each cut into `bins` time slots
 * holding the allowed and denied counts, plus the denied targets with their counts. Rows whose verdict is neither allowed nor denied, or that have no time, are dropped and counted in
 * `skipped`. A target is `null` when the answer has none (never invented). `problem` is the answer's own {code, message} (the CLI call failed: 200 with no rows). Returns {sandbox, total, allowed, denied, skipped,
 * truncated, problem, sparse (fewer than SPARSE_MAX decisions), rows (the decisions, oldest first), from, to, binS, bins, maxBin, lanes: [{category, allowed, denied, bins: [{a, d}]}], deniedTargets: [{category, kind, target, count, last}], empty}.
 */
export function decisionModel(answer, { bins = DECISION_BINS, maxLanes = MAX_DECISION_LANES } = {}) {
  const a = answer && typeof answer === 'object' ? answer : {};
  const rows = [];
  let skipped = 0;
  for (const r of Array.isArray(a.rows) ? a.rows : []) {
    const ok = r && isNum(r.ts) && (r.verdict === 'allowed' || r.verdict === 'denied');
    if (!ok) {
      skipped++;
      continue;
    }
    rows.push({
      ts: r.ts,
      category: typeof r.category === 'string' && r.category ? r.category : 'other',
      verdict: r.verdict,
      kind: typeof r.kind === 'string' && r.kind ? r.kind : null,
      target: typeof r.target === 'string' && r.target ? r.target : null,
    });
  }
  const problem =
    a.problem && typeof a.problem === 'object' && typeof a.problem.code === 'string' ? { code: a.problem.code, message: typeof a.problem.message === 'string' ? a.problem.message : '' } : null;
  const empty = {
    sandbox: typeof a.sandbox === 'string' ? a.sandbox : null,
    total: 0,
    allowed: 0,
    denied: 0,
    skipped,
    truncated: a.truncated === true,
    problem,
    from: null,
    to: null,
    binS: 0,
    bins: 0,
    maxBin: 0,
    lanes: [],
    rows: [],
    sparse: false,
    deniedTargets: [],
    empty: true,
  };
  if (!rows.length) return empty;
  const from = Math.min(...rows.map((r) => r.ts)),
    last = Math.max(...rows.map((r) => r.ts));
  const to = last > from ? last : from + 60;
  const nb = Math.max(1, Math.min(bins, rows.length > 1 ? bins : 1));
  const binS = (to - from) / nb;
  const binOf = (ts) => Math.min(nb - 1, Math.max(0, Math.floor((ts - from) / binS)));
  const count = new Map();
  for (const r of rows) count.set(r.category, (count.get(r.category) || 0) + 1);
  const order = [...count.entries()].sort((x, y) => y[1] - x[1] || (x[0] < y[0] ? -1 : 1)).map(([c]) => c);
  const named = new Set(order.slice(0, order.length > maxLanes ? maxLanes - 1 : maxLanes));
  const laneOf = (c) => (named.has(c) ? c : 'other');
  const lanes = new Map();
  for (const r of rows) {
    const c = laneOf(r.category);
    if (!lanes.has(c)) lanes.set(c, { category: c, allowed: 0, denied: 0, bins: Array.from({ length: nb }, () => ({ a: 0, d: 0 })) });
    const lane = lanes.get(c);
    const b = lane.bins[binOf(r.ts)];
    if (r.verdict === 'allowed') {
      lane.allowed++;
      b.a++;
    } else {
      lane.denied++;
      b.d++;
    }
  }
  // busiest first, with the catch-all `other` lane last whatever its count
  const laneList = [...lanes.values()].sort((x, y) => (x.category === 'other') - (y.category === 'other') || y.allowed + y.denied - (x.allowed + x.denied) || (x.category < y.category ? -1 : 1));
  const targets = new Map();
  for (const r of rows) {
    if (r.verdict !== 'denied') continue;
    const k = `${r.category}\u0000${r.kind ?? ''}\u0000${r.target ?? ''}`;
    const cur = targets.get(k) ?? { category: r.category, kind: r.kind, target: r.target, count: 0, last: r.ts };
    cur.count++;
    cur.last = Math.max(cur.last, r.ts);
    targets.set(k, cur);
  }
  const allowed = rows.filter((r) => r.verdict === 'allowed').length;
  return {
    sandbox: typeof a.sandbox === 'string' ? a.sandbox : null,
    total: rows.length,
    allowed,
    denied: rows.length - allowed,
    skipped,
    truncated: a.truncated === true,
    problem,
    sparse: rows.length < SPARSE_MAX,
    rows: [...rows].sort((x, y) => x.ts - y.ts),
    from,
    to,
    binS,
    bins: nb,
    maxBin: Math.max(1, ...laneList.flatMap((l) => l.bins.map((b) => b.a + b.d))),
    lanes: laneList,
    deniedTargets: [...targets.values()].sort((x, y) => y.count - x.count || y.last - x.last).slice(0, 12),
    empty: false,
  };
}
