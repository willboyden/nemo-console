// Pure shaping of the NVIDIA Flare cards (addendum 9.16): the target grid (target x host x chip x state, with the package counts) from the status the adapter already serves, and the
// per-target charts from ONE series answer split by target: tokens/s and requests/s (rates of the counters), prefill vs generated vs cache-reused tokens, and active vs pending requests.
// No DOM, no i18n. A figure a target does not report is null, never 0 (a target that is not serving has no engine counters at all). Unit-tested in tests/flare-model.test.mjs.
import { normalizeAnswer, rateSegments, pickRateScale, scaleSegments } from './series-model.js';
import { baseKey, finite, gridSize, alignToGrid, mergeFactor, rebin, rebinTimes, counterIncrease, lastOf, peakOf, meanOf } from './series-math.js';

/** The keys of the Flare page's one series request (at most 12; with `split=1` each comes back once per target). */
export const FLARE_KEYS = [
  'flare.generated_total',
  'flare.prefill_total',
  'flare.cache_reused_total',
  'flare.requests_completed_total',
  'flare.requests_failed_total',
  'flare.active_requests',
  'flare.pending_requests',
];

export const TARGET_STATES = ['serving', 'loading', 'busy', 'stopped', 'absent', 'failed', 'unknown'];

const COMPONENT_RE = /^flare @ (.+)$/;
const START_RE = /^start on (.+)$/;

/**
 * What the adapter's one-line description of a target says (flare.rs `describe`): `<chip> <family> (<hosts>), modes <modes> · serving <model>, context <n> | container <state> · image lab/flare:<build> loaded | image not loaded · <n> package(s) · GPU free | GPU busy: <why>`.
 * Every part is optional: a part that is not there is null. `error` is the text after `check failed:`.
 */
export function parseTargetDetail(detail) {
  const s = typeof detail === 'string' ? detail : '';
  const head = /^(\S+) (\S+) \(([^)]*)\), modes ([^·]*?)\s*(?:·|$)/.exec(s);
  const serving = /(?:^|· )serving ([^,·]+?)(?:, context (\d+(?:\.\d+)?))?(?= ·|$)/.exec(s);
  const container = /· container (\w+)/.exec(s);
  const pkgs = /· (\d+) package\(s\)/.exec(s);
  const busy = /· GPU busy(?:: ([^·]*))?$/.exec(s);
  const failed = /· check failed: ([^·]*)/.exec(s);
  return {
    chip: head?.[1] ?? null,
    family: head?.[2] ?? null,
    hosts: head ? head[3].split(/\s+/).filter(Boolean) : [],
    modes: head ? head[4].split(/\s+/).filter(Boolean) : [],
    serving: !!serving,
    model: serving?.[1]?.trim() || null,
    context: serving?.[2] != null ? Number(serving[2]) : null,
    container: container?.[1] ?? null,
    imageLoaded: /· image not loaded/.test(s) ? false : /· image \S+ loaded/.test(s) ? true : null,
    packages: pkgs ? Number(pkgs[1]) : null,
    gpuBusy: !!busy,
    busyWhy: busy?.[1]?.trim() || null,
    gpuFree: /· GPU free/.test(s),
    error: failed ? failed[1].trim() : null,
  };
}

/** The state of one target: serving (the engine answers), loading (a container runs but does not answer yet, or a start is under way), busy (not ours: another engine holds the GPU), failed (the container exited), stopped, absent, unknown. */
export function targetState(component, parsed, starting) {
  if (parsed.error || component.state === 'unknown') return 'unknown';
  if (parsed.serving) return 'serving';
  if (component.state === 'running' || starting) return 'loading';
  if (component.state === 'failed') return 'failed';
  if (parsed.gpuBusy) return 'busy';
  if (parsed.container === 'absent' || component.state === 'absent') return 'absent';
  return 'stopped';
}

/**
 * The grid from `GET /status` (components named `flare @ <target>`, one `start on <target>` while a start runs) and the metric points (`flare.packages{target}` is the package count,
 * and wins over the count in the text). Targets keep the launcher's order. Returns {targets: [{target, chip, family, hosts, modes, state, model, context, container, imageLoaded, packages,
 * busyWhy, detail}], groups: [{chip, targets}], counts: {state: n}, launcher: {state} | null, empty}.
 */
export function targetGrid(status, points = []) {
  const comps = Array.isArray(status?.components) ? status.components : [];
  const starting = new Set(comps.map((c) => START_RE.exec(String(c?.name ?? ''))?.[1]).filter(Boolean));
  const pk = new Map();
  for (const p of Array.isArray(points) ? points : []) if (p && p.key === 'flare.packages' && finite(p.value) && p.labels?.target) pk.set(String(p.labels.target), p.value);
  const targets = [];
  for (const c of comps) {
    const m = COMPONENT_RE.exec(String(c?.name ?? ''));
    if (!m) continue;
    const parsed = parseTargetDetail(c.detail);
    targets.push({
      target: m[1],
      chip: parsed.chip,
      family: parsed.family,
      hosts: parsed.hosts,
      modes: parsed.modes,
      state: targetState(c, parsed, starting.has(m[1])),
      model: parsed.model,
      context: parsed.context,
      container: parsed.container,
      imageLoaded: parsed.imageLoaded,
      packages: pk.has(m[1]) ? pk.get(m[1]) : parsed.packages,
      busyWhy: parsed.busyWhy,
      error: parsed.error,
      detail: typeof c.detail === 'string' ? c.detail : '',
    });
  }
  const groups = [];
  for (const t of targets) {
    const key = t.chip ?? '';
    let g = groups.find((x) => x.chip === key);
    if (!g) groups.push((g = { chip: key, targets: [] }));
    g.targets.push(t);
  }
  groups.sort((a, b) => (a.chip < b.chip ? -1 : a.chip > b.chip ? 1 : 0));
  const counts = Object.fromEntries(TARGET_STATES.map((s) => [s, 0]));
  for (const t of targets) counts[t.state]++;
  const launcher = comps.find((c) => c?.name === 'flare-local');
  return { targets, groups, counts, launcher: launcher ? { state: launcher.state } : null, empty: targets.length === 0 };
}

// ---------------------------------------------------------------- series, split by target

const targetOf = (s) => (s.label && s.label.target != null ? String(s.label.target) : null);
const linesOf = (answer, key) => normalizeAnswer(answer).filter((s) => baseKey(s.key) === baseKey(key) && targetOf(s) != null && s.points.length);

/** Targets that have at least one point of any Flare counter, in name order. */
export function targetsWithData(answer) {
  const t = new Set();
  for (const k of FLARE_KEYS) for (const s of linesOf(answer, k)) t.add(targetOf(s));
  return [...t].sort();
}

/**
 * A counter as one rate line per target for the lines card: {lines: [{name, idx, segments}], per: 's'|'min'} (the unit is chosen over the whole panel: per minute when the busiest rate is
 * under 0.1/s). `keys` may name several counters that are added up per target (completed + failed = all requests).
 */
export function rateLines(answer, key) {
  const series = linesOf(answer, key);
  const raw = series.map((s) => rateSegments(s.points, answer.step_s));
  const scale = pickRateScale(raw);
  const lines = series.map((s, i) => ({ name: targetOf(s), idx: i, segments: scaleSegments(raw[i], scale.factor) })).filter((l) => l.segments.length);
  return { lines, per: scale.per };
}

/** The grid helpers every per-target stack needs: n slots, the merge factor and the merged times. */
function gridOf(answer, maxCols) {
  const n = gridSize(answer.from, answer.to, answer.step_s);
  const factor = mergeFactor(n, maxCols);
  return { n, factor, ts: n ? rebinTimes(answer.from, answer.step_s, n, factor) : [], bucketS: finite(answer.step_s) ? answer.step_s * factor : 0 };
}

const MIX = [
  ['prefill', 'flare.prefill_total'],
  ['generated', 'flare.generated_total'],
  ['reused', 'flare.cache_reused_total'],
];
export const MIX_KEYS = MIX.map(([k]) => k);

/**
 * Prefill vs generated vs cache-reused tokens per target. `rows`: the increase of each counter over the window (a reset counts what it accumulated since: series-math.counterIncrease);
 * `time`: the rate (tokens per second) of each counter per bucket on one grid, so the stack at a time is the token flow then. A counter a target does not report is null, not 0.
 * Returns {rows: [{target, values: {prefill, generated, reused}, resets}], time: [{target, ts, bucketS, series: [{key, values}]}], targets}.
 */
export function tokenMix(answer, { maxCols = 120 } = {}) {
  const a = answer && typeof answer === 'object' ? answer : {};
  const targets = [...new Set(MIX.flatMap(([, k]) => linesOf(a, k).map(targetOf)))].sort();
  const g = gridOf(a, maxCols);
  const rows = [],
    time = [];
  for (const target of targets) {
    const values = {},
      series = [];
    let resets = 0;
    for (const [name, key] of MIX) {
      const s = linesOf(a, key).find((x) => targetOf(x) === target);
      const inc = s ? counterIncrease(s.points) : null;
      values[name] = inc ? inc.value : null;
      resets = Math.max(resets, inc ? inc.resets : 0); // the three counters restart together: one restart is one, not three
      if (s && g.n) {
        const pts = rateSegments(s.points, a.step_s).flat();
        series.push({ key: name, values: rebin(alignToGrid(pts, { from: a.from, stepS: a.step_s, n: g.n }), g.factor) });
      }
    }
    rows.push({ target, values, resets });
    time.push({ target, ts: g.ts, bucketS: g.bucketS, series });
  }
  return { rows, time, targets };
}

/**
 * Active vs pending requests per target: the newest figure of each and the two over time on one grid (a gauge: a missing sample is a gap, not 0).
 * Returns {rows: [{target, values: {active, pending}, peakActive, peakPending}], time: [{target, ts, bucketS, series}], targets}.
 */
export function loadByTarget(answer, { maxCols = 120 } = {}) {
  const a = answer && typeof answer === 'object' ? answer : {};
  const pairs = [
    ['active', 'flare.active_requests'],
    ['pending', 'flare.pending_requests'],
  ];
  const targets = [...new Set(pairs.flatMap(([, k]) => linesOf(a, k).map(targetOf)))].sort();
  const g = gridOf(a, maxCols);
  const rows = [],
    time = [];
  for (const target of targets) {
    const values = {},
      series = [],
      peaks = {};
    for (const [name, key] of pairs) {
      const s = linesOf(a, key).find((x) => targetOf(x) === target);
      values[name] = s ? lastOf(s.points) : null;
      peaks[name] = s ? peakOf(s.points) : null;
      if (s && g.n) series.push({ key: name, values: rebin(alignToGrid(s.points, { from: a.from, stepS: a.step_s, n: g.n }), g.factor, 'max') });
    }
    rows.push({ target, values, peakActive: peaks.active, peakPending: peaks.pending });
    time.push({ target, ts: g.ts, bucketS: g.bucketS, series });
  }
  return { rows, time, targets };
}

/** Mean of a line's points (the window average of a rate or gauge); null with no points. */
export const lineMean = (line) => meanOf(line.segments.flat());
