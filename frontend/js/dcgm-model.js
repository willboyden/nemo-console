// Pure shaping of the GPU telemetry page (addendum 9.14): the fleet heatmap matrix from `GET /systems/dcgm/fleet`, and the per-GPU
// cards of the Status tab (really busy?, memory, power against its limit, energy over the window, throttle violations, remapped
// rows, the XID and diagnostics timeline) from series answers, the metrics snapshot and the events list. No DOM, no i18n: words are
// the views' job. A value that is not there is null, never 0 (a GPU with no energy counter has no energy figure, a host that is
// down has a row of gaps). Unit-tested in tests/dcgm-model.test.mjs.
import { normalizeAnswer, rateSegments, valueSegments } from './series-model.js';
import { counterIncrease, meanOf, lastOf, peakOf, finite, gridSize, alignToGrid, mergeFactor, rebin, rebinTimes, arrayStats } from './series-math.js';
import { niceScale } from './viz/scale.js';
import { fmtBytes } from './format.js';

export const FLEET_METRICS = ['util', 'temp', 'power', 'fb_used'];
export const FLEET_WINDOWS = ['1h', '24h', '7d'];
export const DEFAULT_FLEET = { metric: 'util', window: '24h' };
export const MAX_FLEET_ROWS = 64;
const GIB = 1073741824;

const num = (v) => {
  if (finite(v)) return v;
  if (typeof v === 'string' && v.trim() !== '') {
    const n = Number(v);
    return Number.isFinite(n) ? n : null;
  }
  return null;
};
const cmpGpu = (a, b) => {
  const x = Number(a),
    y = Number(b);
  return Number.isFinite(x) && Number.isFinite(y) ? x - y : String(a).localeCompare(String(b));
};

/** The picker values the page accepts from the URL (anything else falls back to the default). */
export const fleetMetric = (v) => (FLEET_METRICS.includes(v) ? v : DEFAULT_FLEET.metric);
export const fleetWindow = (v) => (FLEET_WINDOWS.includes(v) ? v : DEFAULT_FLEET.window);

/**
 * The colour domain of a fleet metric. Utilisation is a ratio (0..1) or a percentage (0..100), by the answer's `unit`; temperature
 * has a fixed scale (30..90 C: the ramp means the same on every refresh); power and memory take a round top above the data.
 * Returns {min, max, curve}.
 */
export function fleetDomain(metric, unit, rows) {
  const vals = (rows || []).flatMap((r) => r.raw || r.values || []).filter(finite);
  const peak = vals.length ? Math.max(...vals) : 0;
  if (metric === 'util') return { min: 0, max: unit === 'ratio' ? 1 : 100, curve: 'linear' };
  if (metric === 'temp') return { min: 30, max: 90, curve: 'linear' };
  if (metric === 'power') return { min: 0, max: niceScale(0, Math.max(peak, 100), 4).max, curve: 'linear' };
  // memory: a round number of GiB above the data (a 96 GiB card with 80 GiB used gets 100 GiB, not 139.7)
  if (!(peak > 0)) return { min: 0, max: 1, curve: 'linear' };
  const per = unit === 'B' ? 1 : unit === 'GiB' ? GIB : 1048576; // bytes per unit of the value
  return { min: 0, max: (niceScale(0, (peak * per) / GIB, 4).max * GIB) / per, curve: 'linear' };
}

/** How a value of a fleet metric reads, by the answer's unit (memory is MiB unless the unit says bytes or GiB). */
export function fleetFormat(metric, unit) {
  if (metric === 'util') return unit === 'ratio' ? (v) => `${(v * 100).toFixed(0)}%` : (v) => `${v.toFixed(0)}%`;
  if (metric === 'temp') return (v) => `${v.toFixed(0)} °C`;
  if (metric === 'power') return (v) => `${v.toFixed(0)} W`;
  if (unit === 'B') return (v) => fmtBytes(v, 1);
  if (unit === 'GiB') return (v) => `${v.toFixed(1)} GiB`;
  return (v) => fmtBytes(v * 1048576, 1);
}

/**
 * The heatmap matrix from the fleet answer. Every row's points are put on one time grid (from, to, step_s), a slot with no sample
 * is null (a gap: the host was down or the exporter did not answer), and the grid is merged to at most `maxCols` columns (the mean
 * of the samples in a column; min / avg / max in `stats` come from the unmerged samples). Rows are ordered by host, then GPU
 * number. `hostsUp` ([{host, job, up}], from dcgm.host_up) adds a row of gaps for a host that is down and has no history in the
 * window, and flags the rows of a down host.
 * Returns {metric, unit, window, stepS, binS, from, to, ts, cols, rows: [{id, host, job, gpu, model, values, raw, stats, down, missing}],
 * truncated, problem: {code, message} | null, empty}.
 */
export function shapeFleet(answer, { maxCols = 160, hostsUp = [] } = {}) {
  const a = answer && typeof answer === 'object' ? answer : {};
  const stepS = finite(a.step_s) && a.step_s > 0 ? a.step_s : 60;
  const rawRows = (Array.isArray(a.rows) ? a.rows : []).filter((r) => r && typeof r === 'object');
  const pts = rawRows.map((r) => (Array.isArray(r.points) ? r.points : []).map((p) => (Array.isArray(p) ? [num(p[0]), num(p[1]), null] : null)).filter((p) => p && p[0] != null && p[1] != null));
  const flat = pts.flat();
  const from = finite(a.from) ? a.from : flat.length ? Math.min(...flat.map((p) => p[0])) : null;
  const to = finite(a.to) ? a.to : flat.length ? Math.max(...flat.map((p) => p[0])) : null;
  const n = gridSize(from, to, stepS);
  const factor = mergeFactor(n, maxCols);
  const ts = rebinTimes(from ?? 0, stepS, n, factor);
  const rows = rawRows.map((r, i) => {
    const raw = alignToGrid(pts[i], { from: from ?? 0, stepS, n });
    return {
      id: `${r.host ?? ''}|${r.job ?? ''}|${r.gpu ?? ''}`,
      host: String(r.host ?? ''),
      job: String(r.job ?? ''),
      gpu: r.gpu == null ? null : String(r.gpu),
      model: String(r.model ?? ''),
      values: rebin(raw, factor),
      raw,
      stats: arrayStats(raw),
      down: false,
      missing: false,
    };
  });
  rows.sort((x, y) => x.host.localeCompare(y.host) || (x.gpu == null ? -1 : y.gpu == null ? 1 : cmpGpu(x.gpu, y.gpu)));
  const downHosts = new Set((hostsUp || []).filter((h) => h && h.up === false).map((h) => String(h.host)));
  for (const r of rows) r.down = downHosts.has(r.host);
  const have = new Set(rows.map((r) => r.host));
  const gaps = Array(ts.length).fill(null);
  const rawGaps = Array(n).fill(null);
  for (const h of hostsUp || []) {
    if (!h || h.up !== false || have.has(String(h.host))) continue;
    have.add(String(h.host));
    rows.push({
      id: `${h.host}||`,
      host: String(h.host),
      job: String(h.job ?? ''),
      gpu: null,
      model: '',
      values: gaps.slice(),
      raw: rawGaps.slice(),
      stats: arrayStats([]),
      down: true,
      missing: true,
    });
  }
  rows.sort((x, y) => x.host.localeCompare(y.host) || (x.gpu == null ? -1 : y.gpu == null ? 1 : cmpGpu(x.gpu, y.gpu)));
  const p = a.problem && typeof a.problem === 'object' ? { code: String(a.problem.code ?? ''), message: String(a.problem.message ?? '') } : null;
  return {
    metric: String(a.metric ?? ''),
    unit: String(a.unit ?? ''),
    window: String(a.window ?? ''),
    stepS,
    binS: stepS * factor,
    from,
    to,
    ts,
    cols: ts.length,
    rows,
    truncated: a.truncated === true,
    problem: p,
    empty: !rows.some((r) => r.stats.n > 0),
  };
}

/** The hosts of `dcgm.host_up{host,job}` points of the metrics snapshot: [{host, job, up}] (a host listed once, in first-seen order). */
export function hostsFromSnapshot(points) {
  const out = new Map();
  for (const p of points || []) {
    if (p?.key !== 'dcgm.host_up' || !p.labels?.host) continue;
    if (!out.has(p.labels.host)) out.set(p.labels.host, { host: String(p.labels.host), job: String(p.labels.job ?? ''), up: finite(p.value) ? p.value > 0 : null });
  }
  return [...out.values()];
}

// ---------------------------------------------------------------------------------------------- per-GPU cards of the Status tab

/**
 * The series of an answer grouped by GPU: [{gpu, model, byKey: Map(key -> [normalized series])}], ordered by GPU number. A series
 * with no `gpu` label is not per-GPU and is left out.
 */
export function groupByGpu(answerOrSeries) {
  const series = Array.isArray(answerOrSeries) ? answerOrSeries : normalizeAnswer(answerOrSeries);
  const m = new Map();
  for (const s of series) {
    const gpu = s.label?.gpu;
    if (gpu == null) continue;
    const id = String(gpu);
    if (!m.has(id)) m.set(id, { gpu: id, model: s.label.model != null ? String(s.label.model) : '', byKey: new Map() });
    const g = m.get(id);
    if (!g.byKey.has(s.key)) g.byKey.set(s.key, []);
    g.byKey.get(s.key).push(s);
  }
  return [...m.values()].sort((x, y) => cmpGpu(x.gpu, y.gpu));
}
const one = (g, key) => g.byKey.get(key)?.[0] ?? null;

/**
 * "Is it really busy?": DCGM's utilisation is the share of time ANY kernel was running, so it reads high for a GPU that runs one
 * small kernel all the time. The profiling activity (graphics engine, tensor cores, DRAM) says how much of the chip worked. Inputs
 * are ratios 0..1 (window means) or null. Returns 'unknown' (no utilisation), 'idle' (< 10%), 'misleading' (util >= 50% while no
 * engine is above 25%), 'busy' (an engine >= 50%), 'light' (anything between), or 'util_only' (utilisation but no profiling fields).
 */
export function busyVerdict({ util, gr, tensor, dram }) {
  if (!finite(util)) return 'unknown';
  if (util < 0.1) return 'idle';
  const engines = [gr, tensor, dram].filter(finite);
  if (!engines.length) return 'util_only';
  const top = Math.max(...engines);
  if (util >= 0.5 && top < 0.25) return 'misleading';
  if (top >= 0.5) return 'busy';
  return 'light';
}

/** The numbers of the "really busy?" card of one GPU, from window means: {util, gr, tensor, dram, smClock, encoder, decoder, verdict}. */
export function busyModel(g) {
  const mean = (key) => {
    const s = one(g, key);
    return s ? meanOf(s.points) : null;
  };
  const latest = (key) => {
    const s = one(g, key);
    return s ? lastOf(s.points) : null;
  };
  const m = {
    util: mean('dcgm.gpu_util'),
    gr: mean('dcgm.gr_active'),
    tensor: mean('dcgm.tensor_active'),
    dram: mean('dcgm.dram_active'),
    smClock: latest('dcgm.sm_clock_mhz'),
    encoder: mean('dcgm.enc_util'),
    decoder: mean('dcgm.dec_util'),
  };
  return { ...m, verdict: busyVerdict(m) };
}

/**
 * Memory of one GPU: used / reserved / free on one grid, in GiB (a key the answer lacks is null, not 0). `ts`, `bucketS` and
 * `series` ([{key, values}]) feed the stacked chart; `latest` is the newest figure of each part and their sum (the capacity, only
 * when all three are there).
 */
export function memoryModel(g, { from, to, stepS }, maxCols = 120) {
  const n = gridSize(from, to, stepS);
  const factor = mergeFactor(n, maxCols);
  const PARTS = [
    ['used', 'dcgm.fb_used_b'],
    ['reserved', 'dcgm.fb_reserved_b'],
    ['free', 'dcgm.fb_free_b'],
  ];
  const parts = PARTS.map(([name, key]) => {
    const s = one(g, key);
    const raw = s ? alignToGrid(s.points, { from, stepS, n }).map((v) => (v == null ? null : v / GIB)) : null;
    return { name, key, present: !!s, raw, values: raw ? rebin(raw, factor) : null, latest: raw ? arrayStats(raw).latest : null };
  });
  const latestAll = parts.map((p) => p.latest);
  const present = parts.filter((p) => p.present && p.latest != null);
  return {
    gpu: g.gpu,
    model: g.model,
    parts,
    ts: rebinTimes(from, stepS, n, factor),
    bucketS: stepS * factor,
    series: parts.filter((p) => p.values).map((p) => ({ key: p.name, values: p.values })),
    latest: Object.fromEntries(parts.map((p) => [p.name, p.latest])),
    total: latestAll.every((v) => v != null) ? latestAll.reduce((a, b) => a + b, 0) : null,
    any: present.length > 0,
  };
}

/**
 * Power of one GPU against its limit: the two lines as plain value segments and the headroom. `peak` is the highest power seen, and
 * `nearLimit` is true when it came within 5% of the limit. Limit or power missing: those fields are null.
 */
export function powerModel(g, stepS) {
  const power = one(g, 'dcgm.power_w'),
    limit = one(g, 'dcgm.power_limit_w');
  const peak = power ? peakOf(power.points) : null;
  const lim = limit ? lastOf(limit.points) : null;
  return {
    gpu: g.gpu,
    model: g.model,
    power: power ? valueSegments(power.points, stepS) : [],
    limit: limit ? valueSegments(limit.points, stepS) : [],
    latest: power ? lastOf(power.points) : null,
    mean: power ? meanOf(power.points) : null,
    peak,
    limitW: lim,
    nearLimit: peak != null && lim != null && lim > 0 && peak >= lim * 0.95,
    has: !!(power || limit),
  };
}

/**
 * Energy a GPU used over the window, from the `dcgm.energy_mj_total` counter (millijoules): the increase (a counter reset counts as
 * the new value, see counterIncrease), in kWh, and the average power it implies. Null when the GPU has no such counter or fewer
 * than two samples (never 0). `resets` > 0 means the figure is a lower bound.
 */
export function energyModel(g) {
  const s = one(g, 'dcgm.energy_mj_total');
  const inc = s ? counterIncrease(s.points) : null;
  if (!inc) return null;
  return {
    gpu: g.gpu,
    model: g.model,
    mj: inc.value,
    kwh: inc.value / 3.6e9,
    wh: inc.value / 3.6e6,
    resets: inc.resets,
    seconds: inc.seconds,
    avgW: inc.seconds > 0 ? inc.value / 1000 / inc.seconds : null,
  };
}

/** The total over several GPUs (null when none has a figure). */
export function energyTotal(models) {
  const m = (models || []).filter(Boolean);
  if (!m.length) return null;
  const mj = m.reduce((a, x) => a + x.mj, 0);
  const seconds = Math.max(...m.map((x) => x.seconds));
  return { mj, kwh: mj / 3.6e9, wh: mj / 3.6e6, resets: m.reduce((a, x) => a + x.resets, 0), seconds, avgW: seconds > 0 ? mj / 1000 / seconds : null, gpus: m.length };
}

export const VIOLATION_REASONS = ['power', 'thermal', 'board', 'low_util', 'reliability', 'sync_boost'];

/**
 * Throttle violations of one GPU: `dcgm.violation_us_total{reason}` counts the microseconds the clock was held back for a reason,
 * so its rate / 1e6 is the share of time throttled. Per reason: the line as % of time (`segments`; a reset starts a new segment),
 * the % of the whole window (`pct`) and the seconds throttled. Reasons come in the contract's order; a reason with no counter is
 * left out; `quiet` is true when every reason that is there saw no throttling in the window.
 */
export function violationModel(g, stepS) {
  const list = g.byKey.get('dcgm.violation_us_total') || [];
  const rows = list
    .map((s) => {
      const reason = String(s.label?.reason ?? '');
      const inc = counterIncrease(s.points);
      const segs = rateSegments(s.points, stepS).map((seg) => seg.map((p) => [p[0], Math.min(100, p[1] / 1e4), null]));
      return {
        reason,
        segments: segs,
        pct: inc && inc.seconds > 0 ? Math.min(100, (inc.value / (inc.seconds * 1e6)) * 100) : null,
        seconds: inc ? inc.value / 1e6 : null,
        resets: inc ? inc.resets : 0,
        series: s,
      };
    })
    .sort((a, b) => {
      const ia = VIOLATION_REASONS.indexOf(a.reason),
        ib = VIOLATION_REASONS.indexOf(b.reason);
      return (ia < 0 ? 99 : ia) - (ib < 0 ? 99 : ib) || a.reason.localeCompare(b.reason);
    });
  return { gpu: g.gpu, model: g.model, rows, quiet: rows.length > 0 && rows.every((r) => !(r.pct > 0)), any: rows.length > 0 };
}

/** Remapped rows per GPU from the metrics snapshot: [{gpu, model, correctable, uncorrectable, failure}] (each null when not reported). */
export function remapModel(points) {
  const m = new Map();
  const g = (labels) => {
    const id = String(labels?.gpu ?? '');
    if (!m.has(id)) m.set(id, { gpu: id, model: labels?.model != null ? String(labels.model) : '', correctable: null, uncorrectable: null, failure: null });
    return m.get(id);
  };
  for (const p of points || []) {
    if (!p || !finite(p.value) || p.labels?.gpu == null) continue;
    if (p.key === 'dcgm.remapped_rows') {
      const kind = p.labels?.kind;
      if (kind === 'correctable' || kind === 'uncorrectable') g(p.labels)[kind] = p.value;
    } else if (p.key === 'dcgm.row_remap_failure') g(p.labels).failure = p.value > 0;
  }
  return [...m.values()].filter((r) => r.correctable != null || r.uncorrectable != null || r.failure != null).sort((a, b) => cmpGpu(a.gpu, b.gpu));
}

// ---------------------------------------------------------------------------------------------- XID and diagnostics timeline

const DIAG_SEVERITY = { pass: 'ok', warn: 'warn', fail: 'bad', error: 'bad' };
/**
 * The `dcgm.xid` and `dcgm.diag` events inside [from, to] as marks for a swim lane per GPU:
 * [{id, ts, kind: 'xid'|'diag', gpu, severity: 'ok'|'warn'|'bad'|'muted', summary, data}] oldest first, plus `lanes` (GPU ids, numeric order,
 * the ones with an event) and `outside` (events of these kinds before the window). An XID is always 'bad'; a diagnostic takes its
 * verdict; an event with no GPU goes to the lane '?'.
 */
export function timelineModel(events, { from, to }) {
  const items = [];
  const seen = new Map();
  let outside = 0;
  for (const e of events || []) {
    if (!e || (e.kind !== 'dcgm.xid' && e.kind !== 'dcgm.diag') || !finite(e.ts)) continue;
    if (e.ts < from || e.ts > to) {
      outside++;
      continue;
    }
    const data = e.data && typeof e.data === 'object' ? e.data : {};
    const kind = e.kind === 'dcgm.xid' ? 'xid' : 'diag';
    const base = `${e.ts}|${e.kind}|${data.gpu ?? ''}`; // stable across refreshes: a new event elsewhere does not rename this one
    const dup = seen.get(base) ?? 0;
    seen.set(base, dup + 1);
    items.push({
      id: dup ? `${base}#${dup}` : base,
      ts: e.ts,
      kind,
      gpu: data.gpu == null || data.gpu === '' ? '?' : String(data.gpu),
      severity: kind === 'xid' ? 'bad' : (DIAG_SEVERITY[String(data.verdict ?? '').toLowerCase()] ?? 'muted'),
      summary: String(e.summary ?? ''),
      data,
    });
  }
  items.sort((a, b) => a.ts - b.ts);
  const lanes = [...new Set(items.map((i) => i.gpu))].sort((a, b) => (a === '?' ? 1 : b === '?' ? -1 : cmpGpu(a, b)));
  return { items, lanes, outside };
}

/** The tests of a diagnostic event's `data.tests` ({name: status}) as rows, failures first, then warnings, skips and passes. */
export function diagTests(data) {
  const rank = { fail: 0, warn: 1, skip: 2, pass: 3 };
  const tests = data && typeof data.tests === 'object' && data.tests ? data.tests : {};
  return Object.entries(tests)
    .map(([name, status]) => ({ name, status: String(status), rank: rank[String(status).toLowerCase()] ?? 4 }))
    .sort((a, b) => a.rank - b.rank || a.name.localeCompare(b.name));
}
