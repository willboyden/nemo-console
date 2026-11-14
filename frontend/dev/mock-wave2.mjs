// MOCK data for the wave-2 pages (docs/ARCHITECTURE.md addendum 9.14): the new metric keys of DCGM, Switchyard and Dynamo, the fleet
// route `GET /systems/dcgm/fleet`, extra XID events and Dynamo log events. MOCK-ONLY: every number is invented so the pages can be built and
// screenshotted; the SHAPES follow the contract (key names and label sets as listed in 9.14; the fleet answer `{metric, unit, window, step_s, from,
// to, rows: [{host, job, gpu, model, points: [[ts, value]]}], truncated, problem?}` with 64 rows at most, step 60 / 300 / 1800 s, units as the
// backend gives them: ratio, C, W, B; problem codes dcgm.prometheus.unreachable / dcgm.prometheus.bad_answer / dcgm.fleet.no_jobs). Host names are generic (host-a ...): no real lab host name appears here.
// Values are a deterministic function of (row, time), so two requests agree and a heatmap does not jump on refresh.

const hash32 = (s) => {
  let h = 2166136261;
  for (let i = 0; i < s.length; i++) h = Math.imul(h ^ s.charCodeAt(i), 16777619);
  return h >>> 0;
};
const unit01 = (s) => (hash32(String(s)) % 10007) / 10007;
const clamp = (v, lo, hi) => Math.min(hi, Math.max(lo, v));
const GIB = 1073741824;

// ------------------------------------------------------------------------------------------------ fleet
const MAX_Q = 'NVIDIA RTX PRO 6000 Blackwell Max-Q';
const WS = 'NVIDIA RTX PRO 6000 Blackwell Workstation';
const R5090 = 'NVIDIA GeForce RTX 5090';
const GB10 = 'NVIDIA GB10';
/** The profile of a card: power limit (W), memory (GiB). */
const CARD = { [MAX_Q]: { limit: 300, gib: 96 }, [WS]: { limit: 600, gib: 96 }, [R5090]: { limit: 575, gib: 32 }, [GB10]: { limit: 140, gib: 119 } };

/** The hosts Prometheus scrapes. host-d went down 2.5 hours ago: its history ends there; host-b had a 40-minute outage 5 hours ago. */
export const FLEET_HOSTS = [
  { host: 'host-a', job: 'dcgm', up: true, gpus: [MAX_Q, WS] },
  { host: 'host-b', job: 'dcgm-b', up: true, gpus: [R5090], outage: [5 * 3600, 5 * 3600 - 2400] },
  { host: 'host-c', job: 'dcgm-c', up: true, gpus: [GB10, GB10, GB10, GB10] },
  { host: 'host-d', job: 'dcgm-d', up: false, gpus: [MAX_Q, MAX_Q], downSince: 2.5 * 3600 },
];

export const FLEET_UNITS = { util: 'ratio', temp: 'C', power: 'W', fb_used: 'B' };
export const FLEET_WINDOWS = { '1h': [3600, 60], '24h': [86400, 300], '7d': [604800, 1800] };
export const FLEET_MAX_ROWS = 64;

/** Utilisation 0..1 of one card at time ts: a slow rhythm, bursts, a little noise; some cards idle most of the day. */
function util(host, gpu, ts) {
  const id = `${host}|${gpu}`;
  const period = [2.7, 5, 9, 24][hash32(id) % 4] * 3600;
  const phase = unit01(`${id}|p`) * 6.283;
  const base = 0.12 + 0.7 * unit01(`${id}|b`);
  const wave = 0.5 + 0.5 * Math.sin((ts / period) * 6.283 + phase);
  const burst = Math.floor(ts / 1500 + unit01(`${id}|s`) * 7) % 9 === 0 ? 0.3 : 0;
  const noise = (unit01(`${id}|${Math.floor(ts / 60)}`) - 0.5) * 0.12;
  const idle = unit01(`${id}|i`) < 0.25 ? 0.15 : 1; // a quarter of the cards mostly idle
  return clamp((base * wave * idle + burst * idle + noise) * (0.6 + 0.4 * wave), 0, 1);
}

function fleetValue(metric, host, gpu, model, ts) {
  const u = util(host, gpu, ts);
  const card = CARD[model] ?? CARD[GB10];
  if (metric === 'util') return +u.toFixed(4);
  if (metric === 'temp') return +(34 + 40 * u + (unit01(`${host}|${gpu}|t|${Math.floor(ts / 300)}`) - 0.5) * 3).toFixed(1);
  if (metric === 'power') return +(28 + (card.limit - 28) * Math.pow(u, 1.15)).toFixed(1);
  // memory: models are loaded and stay: a step each few hours, not a copy of utilisation
  const step = Math.floor(ts / (3 * 3600) + unit01(`${host}|${gpu}|m`) * 5) % 4;
  return Math.round(card.gib * GIB * clamp(0.18 + 0.2 * step + 0.05 * unit01(`${host}|${gpu}|${step}`), 0.05, 0.95));
}

/** `mode`: 'ok' | 'prom_down' (200, no rows, a problem) | 'empty' (200, no rows, no problem). Returns the route's answer. */
export function fleetAnswer({ metric, window, now, mode = 'ok' }) {
  const [span, step] = FLEET_WINDOWS[window];
  const end = Math.floor(now / step) * step;
  const start = end - span;
  const doc = { metric, unit: FLEET_UNITS[metric], window, step_s: step, from: start, to: end, rows: [], truncated: false };
  if (mode === 'prom_down') {
    doc.problem = { code: 'dcgm.prometheus.unreachable', message: 'Prometheus at http://prometheus:9090 did not answer (connection refused).' };
    return doc;
  }
  if (mode === 'empty') return doc;
  const rows = [];
  for (const h of FLEET_HOSTS) {
    h.gpus.forEach((model, g) => {
      const points = [];
      for (let ts = start; ts <= end; ts += step) {
        if (h.downSince && ts > now - h.downSince) continue;
        if (h.outage && ts >= now - h.outage[0] && ts <= now - h.outage[1]) continue;
        points.push([ts, fleetValue(metric, h.host, String(g), model, ts)]);
      }
      if (points.length) rows.push({ host: h.host, job: h.job, gpu: String(g), model, points });
    });
  }
  doc.truncated = rows.length > FLEET_MAX_ROWS;
  doc.rows = rows.slice(0, FLEET_MAX_ROWS);
  return doc;
}

// ------------------------------------------------------------------------------------------------ DCGM keys of 9.14 (added to the local GPUs' points)
/** Extra per-GPU points: reserved memory, memory temperature and clock, encoder / decoder, power limit, energy counter, violations, remapped rows. */
export function dcgmWave2Points(gpus, wob) {
  const P = (key, value, unit = '', labels = {}) => ({ key, value, unit, labels });
  return gpus.flatMap(([g, model], i) => {
    const L = { gpu: g, model };
    const card = CARD[model] ?? CARD[MAX_Q];
    const out = [
      P('dcgm.fb_reserved_b', 1.1 * GIB, 'B', L),
      P('dcgm.mem_clock_mhz', 1750, 'MHz', L),
      P('dcgm.enc_util', 0, 'ratio', L),
      P('dcgm.dec_util', 0.02, 'ratio', L),
      P('dcgm.power_limit_w', card.limit, 'W', L),
      P('dcgm.energy_mj_total', 4.1e9 + i * 1e8, 'mJ', L),
      P('dcgm.remapped_rows', i === 0 ? 2 : 0, '1', { ...L, kind: 'correctable' }),
      P('dcgm.remapped_rows', 0, '1', { ...L, kind: 'uncorrectable' }),
      P('dcgm.row_remap_failure', 0, '1', L),
    ];
    if (model !== GB10) out.push(P('dcgm.mem_temp_c', wob(i % 2 ? 58 : 66, 3), 'C', L)); // boards without a memory sensor report no key at all
    for (const reason of ['power', 'thermal', 'board', 'low_util', 'reliability', 'sync_boost']) out.push(P('dcgm.violation_us_total', 1e6, 'us', { ...L, reason }));
    return out;
  });
}

/** The host_up points of the fleet hosts (labels host, job): three up, one down. */
export function hostPoints() {
  const P = (key, value, unit = '', labels = {}) => ({ key, value, unit, labels });
  return FLEET_HOSTS.flatMap((h) => {
    const L = { host: h.host, job: h.job };
    return [P('dcgm.host_up', h.up ? 1 : 0, '1', L), P('dcgm.fleet_gpus', h.up ? h.gpus.length : 0, '1', L), P('dcgm.fleet_temp_max_c', 58, 'C', L), P('dcgm.fleet_power_w', 410, 'W', L)];
  });
}

/** Counter rate (per second) of the DCGM keys that need a per-label rate: energy in mJ/s (= power x 1000), violations in us/s. */
export function dcgmCounterRate(base, labels) {
  const gpu = Number(labels?.gpu) || 0;
  if (base === 'dcgm.energy_mj_total') return (150 + 90 * (gpu % 2 === 0 ? 1.6 : 1) + gpu * 6) * 1000;
  if (base === 'dcgm.violation_us_total') {
    const r = labels?.reason;
    if (gpu === 0 && r === 'power') return 80000; // 8% of the time
    if (gpu === 0 && r === 'thermal') return 6000;
    if (gpu === 1 && r === 'low_util') return 200000;
    return 0;
  }
  return undefined;
}

/** Extra XID events (the console emits them with a string `gpu`), newest first. */
export function dcgmExtraEvents(now) {
  const E = (ago, gpu, xid) => ({ ts: now - ago, system: 'dcgm', kind: 'dcgm.xid', summary: `GPU ${gpu}: XID ${xid}`, data: { gpu: String(gpu), xid, source: 'last_xid' } });
  return [E(3.2 * 3600, 0, 13), E(7.5 * 3600, 1, 31), E(2.4 * 86400, 0, 43), E(3.1 * 86400, 1, 79)];
}

// ------------------------------------------------------------------------------------------------ Switchyard keys of 9.14
export const SY_MODELS = ['nemotron-lightning', 'qwen3.8-27b-1m', 'qwen3.8-flash-next', 'glm-5.3-flash-fp4-local'];
const SY_ROUTES = [
  ['auto-local', 412],
  ['direct-lightning', 96],
  ['direct-27b', 21],
  ['auto-flashnext', 8],
];
/** `sy.route.calls` counts the calls among the newest routing records (a window label says how many); the rest are the 9.14 keys. */
export function switchyardWave2Points(wob) {
  const P = (key, value, unit = '', labels = {}) => ({ key, value, unit, labels });
  const lat = { 'nemotron-lightning': [380, 2900], 'qwen3.8-27b-1m': [1250, 9800], 'qwen3.8-flash-next': [610, 4100], 'glm-5.3-flash-fp4-local': [920, 7200] };
  const out = [...SY_ROUTES.map(([route, n]) => P('sy.route.calls', n, '1', { route, window: 'last_500_records' }))];
  SY_MODELS.forEach((model, i) => {
    out.push(
      P('sy.model.calls', 700 / (i + 1), '1', { model }),
      P('sy.model.errors_total', 3 + i, '1', { model }),
      P('sy.model.latency_ms', wob(lat[model][0], 40), 'ms', { model, q: 'p50' }),
      P('sy.model.latency_ms', wob(lat[model][1], 300), 'ms', { model, q: 'p99' }),
      P('sy.model.cache_hit_ratio', [0.71, 0.38, 0.52, 0.18][i], 'ratio', { model }),
      P('sy.model.max_context_tokens', [262144, 1000000, 262144, 995328][i], 'tok', { model }),
    );
  });
  out.push(
    P('sy.routing_overhead_ms', wob(0.9, 0.2), 'ms', { q: 'p50' }),
    P('sy.routing_overhead_ms', wob(4.2, 0.8), 'ms', { q: 'p99' }),
    P('sy.fallbacks_total', 12, '1', { reason: 'context_window' }),
    P('sy.fallbacks_total', 3, '1', { reason: 'unavailable' }),
    P('sy.client_responses_total', 1100, '1', { outcome: 'ok' }),
    P('sy.client_responses_total', 9, '1', { outcome: 'error' }),
    P('sy.client_responses_total', 31, '1', { outcome: 'cancelled' }),
    P('sy.client_responses_total', 2, '1', { outcome: 'timeout' }),
  );
  // decision source -> target (no route label: the contract lists source and target only)
  for (const [source, target] of [
    ['heuristic', 'nemotron-lightning'],
    ['heuristic', 'qwen3.8-27b-1m'],
    ['judge', 'nemotron-lightning'],
    ['judge', 'qwen3.8-27b-1m'],
    ['judge', 'qwen3.8-flash-next'],
    ['cache', 'nemotron-lightning'],
  ])
    out.push(P('sy.decisions_total', 300, '1', { source, target }));
  return out;
}
export const SY_COUNTER_RATE = {
  'sy.model.calls': (l) => ({ 'nemotron-lightning': 0.36, 'qwen3.8-27b-1m': 0.07, 'qwen3.8-flash-next': 0.03, 'glm-5.3-flash-fp4-local': 0.012 })[l.model] ?? 0.01,
  'sy.model.errors_total': (l) => ({ 'nemotron-lightning': 0.0012, 'qwen3.8-27b-1m': 0.0035, 'qwen3.8-flash-next': 0.0002, 'glm-5.3-flash-fp4-local': 0.0006 })[l.model] ?? 0,
  'sy.fallbacks_total': (l) => (l.reason === 'context_window' ? 0.004 : 0.0007),
  'sy.client_responses_total': (l) => ({ ok: 0.38, error: 0.003, cancelled: 0.01, timeout: 0.0008 })[l.outcome] ?? 0.001,
  'sy.decisions_total': (l) =>
    ({
      'heuristic|nemotron-lightning': 0.21,
      'heuristic|qwen3.8-27b-1m': 0.035,
      'judge|nemotron-lightning': 0.06,
      'judge|qwen3.8-27b-1m': 0.04,
      'judge|qwen3.8-flash-next': 0.015,
      'cache|nemotron-lightning': 0.05,
    })[`${l.source}|${l.target}`] ?? 0.01,
};

// ------------------------------------------------------------------------------------------------ Dynamo keys of 9.14
export function dynamoWave2Points(wob) {
  const P = (key, value, unit = '', labels = {}) => ({ key, value, unit, labels });
  return [
    P('dyn.ttft_window_ms', wob(205, 25), 'ms', { q: 'p50' }),
    P('dyn.ttft_window_ms', wob(470, 70), 'ms', { q: 'p95' }),
    P('dyn.ttft_window_ms', wob(820, 120), 'ms', { q: 'p99' }),
    P('dyn.itl_window_ms', wob(18, 2), 'ms', { q: 'p50' }),
    P('dyn.itl_window_ms', wob(31, 4), 'ms', { q: 'p95' }),
    P('dyn.itl_window_ms', wob(48, 6), 'ms', { q: 'p99' }),
    P('dyn.phase_ms', 14, 'ms', { phase: 'queue', q: 'p50' }),
    P('dyn.phase_ms', 180, 'ms', { phase: 'prefill', q: 'p50' }),
    P('dyn.phase_ms', 2400, 'ms', { phase: 'decode', q: 'p50' }),
    P('dyn.phase_ms', 2610, 'ms', { phase: 'inference', q: 'p50' }),
    P('dyn.phase_ms', 96, 'ms', { phase: 'queue', q: 'p95' }),
    P('dyn.phase_ms', 520, 'ms', { phase: 'prefill', q: 'p95' }),
    P('dyn.phase_ms', 6100, 'ms', { phase: 'decode', q: 'p95' }),
    P('dyn.phase_ms', 6700, 'ms', { phase: 'inference', q: 'p95' }),
    P('dyn.requests_running', Math.round(wob(3, 2)), '1'),
    P('dyn.requests_waiting', Math.round(wob(1, 1)), '1'),
    P('dyn.preemptions_total', 4, '1'),
    P('dyn.tokens', 40000, 'tok', { kind: 'completion' }),
    P('dyn.prefix_cache_hit_ratio', 0.41, 'ratio'),
    P('dyn.spec_accept_ratio', 0.58, 'ratio'),
    P('dyn.capacity', 262144, 'tok', { kind: 'context_length' }),
    P('dyn.capacity', 18240, '1', { kind: 'kv_blocks' }),
    P('dyn.capacity', 256, '1', { kind: 'max_num_seqs' }),
    P('dyn.capacity', 16384, 'tok', { kind: 'max_batched_tokens' }),
    ...[0.82, 0.67, 0.51, 0.38, 0.27].map((v, pos) => P('dyn.spec_accept_pos', v, 'ratio', { pos: String(pos) })),
  ];
}
export const DYN_COUNTER_RATE = { 'dyn.tokens': () => 38, 'dyn.preemptions_total': () => 0.004 };

/** Log events of the frontend and worker (levels ERROR / WARN / INFO / DEBUG / TRACE, a component each), newest first. Includes one line with markup and one redacted. */
export function dynamoLogEvents(now, count = 140) {
  const lines = [
    ['frontend', 'INFO', 'request 200 POST /v1/chat/completions ttft 231 ms'],
    ['worker', 'INFO', 'kv cache usage 21% (3830 of 18240 blocks)'],
    ['frontend', 'DEBUG', 'router: worker 0 selected (prefix match 8192 tokens)'],
    ['worker', 'INFO', 'prefix cache hit 8192 tokens'],
    ['worker', 'DEBUG', 'scheduler: 3 running, 1 waiting, batch 2174 tokens'],
    ['frontend', 'INFO', 'request 200 POST /v1/chat/completions ttft 188 ms'],
    ['worker', 'WARN', 'preempted 1 sequence: kv cache is full (92%)'],
    ['frontend', 'WARN', 'slow request: ttft 4210 ms (queue 3900 ms)'],
    ['frontend', 'INFO', 'request 200 POST /v1/completions ttft 244 ms'],
    ['worker', 'TRACE', 'step 18442: 3 tokens accepted of 5 drafted'],
    ['worker', 'ERROR', 'request aborted: client closed the connection after 12 tokens'],
    ['frontend', 'ERROR', 'upstream worker did not answer within 30 s: retrying on another worker'],
    ['frontend', 'DEBUG', 'health check ok (1 worker registered)'],
    ['worker', 'INFO', 'authorization: Bearer [REDACTED] accepted for model unsloth/Qwen3.8-27B-NVFP4'],
    ['frontend', 'WARN', 'GET /<img src=x onerror=alert(1)> 404 (shown as text, never as markup)'],
  ];
  const out = [];
  for (let i = 0; i < count; i++) {
    const r = unit01(`dl|${i}`);
    // INFO and DEBUG are common, WARN and ERROR rare
    const pickFrom = r < 0.62 ? [0, 1, 3, 5, 8, 13] : r < 0.8 ? [2, 4, 12] : r < 0.9 ? [6, 7, 14] : r < 0.96 ? [10, 11] : [9];
    const [component, level, text] = lines[pickFrom[hash32(`dl|${i}|x`) % pickFrom.length]];
    out.push({ ts: now - i * 17 - (hash32(`dl|${i}|t`) % 9), system: 'dynamo', kind: 'dynamo.log', summary: text, data: { component, level } });
  }
  return out;
}
