// MOCK data for the wave-4 pages (docs/ARCHITECTURE.md addendum 9.16): the Guardrails rails route, the OpenShell persisted lifecycle events and the decisions route, the Flare targets with
// per-target counters, the up / down histories, and the Audit history with `GET /audit/summary` and the `since` / `until` / `actor` / `status` filters. MOCK-ONLY: every number is invented so
// the pages can be built and screenshotted; the SHAPES follow the contract (rails `{configs: [{id, main_model, input, output, streaming, retrieval}], problems}`; decisions `{sandbox,
// rows: [{ts, category, verdict, kind, target}], truncated}` with 404 for a sandbox that is not in the list; audit summary `{window, by_day, by_actor, errors, denied_total}`;
// openshell.sandbox events `{name, id, phase, action: present | created | phase | deleted, previous_phase}`). Names are generic (host-a, example.test, spark-a ...): no real lab host name
// appears here. Everything is a deterministic function of the clock-relative time and its own seeded generators, so two requests agree and a chart does not jump on refresh.

const DAY = 86400;
const hash32 = (s) => {
  let h = 2166136261;
  for (let i = 0; i < s.length; i++) h = Math.imul(h ^ s.charCodeAt(i), 16777619);
  return h >>> 0;
};
const unit01 = (s) => (hash32(String(s)) % 100003) / 100003;
/** A seeded generator: the same sequence on every start. */
const lcg = (seed) => {
  let s = seed >>> 0;
  return () => {
    s = (Math.imul(s, 1664525) + 1013904223) >>> 0;
    return s / 4294967296;
  };
};
const pad = (n, w = 2) => String(n).padStart(w, '0');

// ------------------------------------------------------------------------------------------------ guardrails: rails
export const RAILS_MODES = ['ok', 'empty', 'problem', 'unsupported'];
/** `GET /systems/guardrails/rails`: the rail pipeline of each served config (flow names only). `mode` is the dev toggle (/__dev/wave4?rails=). */
export function railsAnswer(mode = 'ok') {
  if (mode === 'empty') return { configs: [], problems: [] };
  const configs = [
    {
      id: 'lab-input-output',
      main_model: 'chat-model-small',
      input: ['self check input', 'check jailbreak', 'mask sensitive data on input'],
      output: ['self check output', 'mask sensitive data on output'],
      streaming: { enabled: true, chunk_size: 200, context_size: 50 },
      retrieval: ['check retrieval relevance'],
    },
    { id: 'lab-default', main_model: 'chat-model-small', input: ['self check input'], output: [], streaming: { enabled: false, chunk_size: null, context_size: null }, retrieval: [] },
    { id: 'lab-bare', main_model: null, input: [], output: [], streaming: null, retrieval: [] },
  ];
  // problems are {config, code, count} (codes: configs.capped, config.unreadable, config.too_large, config.not_utf8, config.yaml_error, config.not_a_mapping, flows.dropped, flows.capped, main_model.dropped);
  // an unreadable config is still listed, with empty lists and nulls
  if (mode === 'problem') configs.push({ id: 'lab-broken', main_model: null, input: [], output: [], streaming: null, retrieval: [] });
  const problems =
    mode === 'problem'
      ? [
          { config: 'lab-broken', code: 'config.yaml_error', count: null },
          { config: 'lab-input-output', code: 'flows.dropped', count: 2 },
        ]
      : [];
  return { configs, problems };
}

// ------------------------------------------------------------------------------------------------ openshell: lifecycle events
const hex4 = (s) => hash32(s).toString(16).padStart(8, '0').slice(0, 4);
/**
 * The sandboxes of the mock gateway as lifecycle stories. Each: {name, id, steps: [[secondsAgo, action, phase, previous]]}. Data Designer creates one per run and deletes it afterwards:
 * created (Provisioning) -> phase (Ready) -> deleted; one run ended in Error before it was deleted; three sandboxes exist now (an older one, seen at baseline with `present`).
 */
let STORIES = null;
function stories() {
  if (STORIES) return STORIES;
  const out = [];
  const rnd = lcg(0x5a17d);
  // finished runs over the last three days, newest ~25 min ago: created, Ready after 8-40 s, deleted 2-9 minutes later
  const n = 22;
  for (let i = 0; i < n; i++) {
    const ago = 1500 + (i / (n - 1)) * (2.8 * DAY) + (rnd() - 0.5) * 3000;
    const mode = i % 3 === 1 ? 'preview' : 'create';
    const name = `dd-${mode}-${hex4(`run${i}`)}`;
    const ready = 8 + Math.round(rnd() * 32),
      life = 120 + Math.round(rnd() * 420);
    const steps = [
      [ago, 'created', 'Provisioning', null],
      [ago - ready, 'phase', 'Ready', 'Provisioning'],
    ];
    if (i === 6 || i === 15) {
      // the sandbox never became ready: Provisioning -> Error, deleted a little later
      steps.pop();
      steps.push([ago - 55, 'phase', 'Error', 'Provisioning'], [ago - 55 - 20, 'phase', 'Deleting', 'Error'], [ago - 55 - 35, 'deleted', 'Deleting', 'Deleting']);
    } else {
      steps.push([ago - life, 'phase', 'Deleting', 'Ready'], [ago - life - 6, 'deleted', 'Deleting', 'Deleting']);
    }
    out.push({ name, id: `sbx-${hex4(name)}${hex4(`${name}x`)}`, steps });
  }
  // the ones that exist now
  out.push({ name: 'lab-shell-01', id: 'sbx-c0ffee01a1b2', steps: [[20 * 3600, 'present', 'Ready', null]] });
  out.push({
    name: 'dd-create-9c1e',
    id: 'sbx-9c1e00ab12cd',
    steps: [
      [12 * 60, 'created', 'Provisioning', null],
      [12 * 60 - 21, 'phase', 'Ready', 'Provisioning'],
    ],
  });
  out.push({ name: 'dd-preview-77ab', id: 'sbx-77ab00ef34aa', steps: [[190, 'created', 'Provisioning', null]] });
  STORIES = out;
  return out;
}

/** The persisted `openshell.sandbox` events (newest first), as the adapter writes them. */
export function openshellEvents(now) {
  const evs = [];
  for (const s of stories()) {
    for (const [ago, action, phase, previous] of s.steps) {
      const ts = now - ago;
      const summary = action === 'phase' ? `sandbox ${s.name}: ${previous} -> ${phase}` : action === 'deleted' ? `sandbox ${s.name} deleted (was ${phase})` : `sandbox ${s.name} ${action} (${phase})`;
      evs.push({ ts, system: 'openshell', kind: 'openshell.sandbox', summary, data: { name: s.name, id: s.id, phase, action, previous_phase: previous } });
    }
  }
  return evs.sort((a, b) => b.ts - a.ts);
}

/** The sandboxes that exist at `ago` seconds before now, by phase bucket: {Provisioning, Ready, Error, Deleting, Other}. */
export function phaseCountsAt(ago) {
  const counts = { Provisioning: 0, Ready: 0, Error: 0, Deleting: 0, Other: 0 };
  for (const s of stories()) {
    let cur = null;
    for (const [a, action, phase] of s.steps) {
      if (a >= ago) cur = action === 'deleted' ? null : phase; // this step had already happened
    }
    if (cur) counts[counts[cur] != null ? cur : 'Other']++;
  }
  return counts;
}
/** The names of the sandboxes that exist now (the gateway's current list). */
export function presentSandboxNames() {
  return stories()
    .filter((s) => s.steps.at(-1)[1] !== 'deleted')
    .map((s) => s.name);
}

// ------------------------------------------------------------------------------------------------ openshell: decisions
export const DECISION_MODES = ['ok', 'empty', 'many', 'fail', 'sparse'];
// The backend's fixed lists: category in ssh, config, network, http, dns, process, filesystem, policy, auth, other; kind in open, close, connect, request, applying, built, exec, read, write, resolve, load, other.
const CATS = [
  // category, weight, kinds, targets (host:port or null), denied share
  ['network', 40, ['connect'], ['registry.example.test:443', 'router.example.test:4000', 'pypi.example.test:443', 'cdn.example.test:443', '192.0.2.44:8080'], 0.22],
  ['http', 20, ['request'], ['router.example.test:4000', 'api.example.test:443'], 0.1],
  ['dns', 8, ['resolve'], ['registry.example.test:53', 'cdn.example.test:53'], 0.12],
  ['ssh', 6, ['open', 'close'], [null], 0],
  ['filesystem', 14, ['read', 'write'], [null], 0.07],
  ['process', 8, ['exec'], [null], 0.05],
  ['config', 4, ['applying', 'built'], [null], 0],
];
/**
 * `GET /systems/openshell/decisions?sandbox=&limit=`: rows of one sandbox's log, oldest first. A sandbox not in the current list is a 404 (the caller answers it); `mode` is the dev toggle
 * (/__dev/wave4?decisions=): `empty` (a log with no decision), `many` (more than the limit: truncated), `fail` (500).
 */
export function decisionsAnswer(sandbox, limit, now, mode = 'ok') {
  if (mode === 'empty') return { sandbox, rows: [], truncated: false };
  // a failed CLI call is a 200 with no rows and a problem
  if (mode === 'fail') return { sandbox, rows: [], truncated: false, problem: { code: 'openshell.logs.failed', message: 'openshell logs exited with an error (mock)' } };
  // real logs often hold little more than the SSH open
  if (mode === 'sparse') return { sandbox, rows: [{ ts: Math.round((now - 95) * 1000) / 1000, category: 'ssh', verdict: 'allowed', kind: 'open', target: null }], truncated: false };
  const rnd = lcg(hash32(sandbox) ^ 0x9e3779b1);
  const total = mode === 'many' ? Math.max(limit + 400, 2400) : 120 + (hash32(sandbox) % 90);
  // (the backend keeps the NEWEST `limit` rows, oldest first)
  const span = 25 * 60;
  const rows = [];
  const weights = CATS.reduce((a, c) => a + c[1], 0);
  for (let i = 0; i < total; i++) {
    let r = rnd() * weights,
      c = CATS[0];
    for (const x of CATS) {
      if (r < x[1]) {
        c = x;
        break;
      }
      r -= x[1];
    }
    // activity comes in bursts: a few steps of start-up, then work, then a quieter tail
    const u = Math.pow(rnd(), 1.6);
    const ts = now - span + u * span;
    const denied = rnd() < c[4];
    rows.push({ ts: Math.round(ts * 1000) / 1000, category: c[0], verdict: denied ? 'denied' : 'allowed', kind: c[2][Math.floor(rnd() * c[2].length)], target: c[3][Math.floor(rnd() * c[3].length)] });
  }
  rows.sort((a, b) => a.ts - b.ts);
  const truncated = rows.length > limit;
  return { sandbox, rows: truncated ? rows.slice(rows.length - limit) : rows, truncated };
}

// ------------------------------------------------------------------------------------------------ the 0 / 1 histories and the phase counts (series route hooks)
/** Periods (seconds ago: from, to) a system was down, so the up / down strips have something to show. */
const DOWN = {
  'gr.up': [
    [95 * 60, 88 * 60],
    [26 * 3600, 25.5 * 3600],
    [3 * DAY, 3 * DAY - 1800],
  ],
  'os.gateway_up': [[2.2 * DAY, 2.1 * DAY]],
  'dyn.up': [[40 * 60, 31 * 60]],
  'relay.gateway_up': [],
};
/** The share of the bucket [ts, ts + step) in which the system was up, 0..1. */
function upShare(base, ts, step, now) {
  let down = 0;
  for (const [a, b] of DOWN[base] ?? []) {
    const lo = Math.max(ts, now - a),
      hi = Math.min(ts + step, now - b);
    if (hi > lo) down += hi - lo;
  }
  return 1 - Math.min(1, down / step);
}

/** Per-key gauge functions of the series route: (labels, ts, step, now) => number | null (null = no sample: a gap). */
export const WAVE4_GAUGES = {
  'gr.up': (l, ts, step, now) => upShare('gr.up', ts, step, now),
  'os.gateway_up': (l, ts, step, now) => upShare('os.gateway_up', ts, step, now),
  'relay.gateway_up': (l, ts, step, now) => upShare('relay.gateway_up', ts, step, now),
  'dyn.up': (l, ts, step, now) => upShare('dyn.up', ts, step, now),
  'os.sandboxes_by_phase': (l, ts, step, now) => {
    // a step holds between samples: the count at the middle of the bucket, averaged over a few points when the bucket is long
    const n = step > 120 ? 5 : 1;
    let sum = 0;
    for (let i = 0; i < n; i++) sum += phaseCountsAt(Math.max(0, now - (ts + ((i + 0.5) * step) / n)))[l.phase] ?? 0;
    return Math.round((sum / n) * 100) / 100;
  },
  'os.sandboxes': (l, ts, step, now) => {
    const c = phaseCountsAt(Math.max(0, now - (ts + step / 2)));
    return Object.values(c).reduce((a, b) => a + b, 0);
  },
};

// ------------------------------------------------------------------------------------------------ flare
export const FLARE_MODES = ['default', 'mixed', 'none'];
/** The targets: [name, chip, family, hosts, modes]. Generic names: a local GPU box, a remote one, four single Sparks and the four together. */
export const FLARE_TARGETS = [
  ['local', 'sm120', 'gb202', 'local', 'tp1 tp2'],
  ['gpu-box', 'sm120', 'gb202', 'gpu-box', 'tp1'],
  ['spark-a', 'sm121', 'gb10', 'spark-a', 'tp1'],
  ['spark-b', 'sm121', 'gb10', 'spark-b', 'tp1'],
  ['spark-c', 'sm121', 'gb10', 'spark-c', 'tp1'],
  ['spark-d', 'sm121', 'gb10', 'spark-d', 'tp1'],
  ['spark-all', 'sm121', 'gb10', 'spark-a spark-b spark-c spark-d', 'tp4'],
];
/** The state of each target by dev mode: serving | loading | busy | absent | stopped | failed | unknown. */
export function flareStates(mode = 'default') {
  if (mode === 'none') return Object.fromEntries(FLARE_TARGETS.map(([t]) => [t, 'absent']));
  if (mode === 'mixed') return { local: 'serving', 'gpu-box': 'failed', 'spark-a': 'busy', 'spark-b': 'absent', 'spark-c': 'stopped', 'spark-d': 'unknown', 'spark-all': 'loading' };
  return { local: 'serving', 'gpu-box': 'loading', 'spark-a': 'busy', 'spark-b': 'busy', 'spark-c': 'busy', 'spark-d': 'busy', 'spark-all': 'serving' };
}
export const FLARE_PACKAGES = { local: 2, 'gpu-box': 1, 'spark-a': 1, 'spark-b': 0, 'spark-c': 1, 'spark-d': 1, 'spark-all': 1 };
const FLARE_MODELS = { local: ['example-27b-fp8', 262144], 'spark-all': ['example-glm-flash', 524288] };

/** The status components of Flare in the adapter's shape (flare.rs `describe`): one `flare @ <target>` per target. */
export function flareComponents(mode = 'default') {
  const states = flareStates(mode);
  const comps = [];
  for (const [t, chip, fam, hosts, modes] of FLARE_TARGETS) {
    const st = states[t];
    const pk = FLARE_PACKAGES[t];
    const [model, ctx] = FLARE_MODELS[t] ?? ['example-27b-fp8', 262144];
    const head = `${chip} ${fam} (${hosts}), modes ${modes}`;
    const image = st === 'failed' || t === 'spark-b' ? 'image not loaded' : 'image lab/flare:902c88e loaded';
    let state = 'absent',
      detail;
    if (st === 'serving') {
      state = 'running';
      detail = `${head} · serving ${model}, context ${ctx} · ${image} · ${pk} package(s) · GPU busy: 1 CUDA process(es) on the GPU`;
    } else if (st === 'loading') {
      state = 'running';
      detail = `${head} · container running · ${image} · ${pk} package(s) · GPU busy: 1 CUDA process(es) on the GPU`;
    } else if (st === 'busy') {
      detail = `${head} · container absent · ${image} · ${pk} package(s) · GPU busy: 1 CUDA process(es) on the GPU`;
    } else if (st === 'failed') {
      state = 'failed';
      detail = `${head} · container exited · ${image} · ${pk} package(s) · GPU free`;
    } else if (st === 'stopped') {
      state = 'stopped';
      detail = `${head} · container created · ${image} · ${pk} package(s) · GPU free`;
    } else if (st === 'unknown') {
      state = 'unknown';
      detail = `${head} · check failed: ssh timed out`;
    } else detail = `${head} · container absent · ${image} · ${pk} package(s) · GPU free`;
    comps.push({ kind: 'container', name: `flare @ ${t}`, state, image: st === 'absent' || st === 'unknown' ? null : 'lab/flare:902c88e', detail });
  }
  return comps;
}
/** The targets that serve (their engine counters exist). */
export const flareServing = (mode = 'default') => FLARE_TARGETS.map(([t]) => t).filter((t) => flareStates(mode)[t] === 'serving');

/** Per-second rate of each Flare counter by target (for the series route: `COUNTER_RATE` takes a function of the labels). */
export const FLARE_RATES = {
  'flare.generated_total': (l) => ({ local: 31, 'spark-all': 64 })[l.target] ?? 12,
  'flare.prefill_total': (l) => ({ local: 150, 'spark-all': 340 })[l.target] ?? 60,
  'flare.cache_reused_total': (l) => ({ local: 85, 'spark-all': 190 })[l.target] ?? 30,
  'flare.requests_completed_total': (l) => ({ local: 0.04, 'spark-all': 0.06 })[l.target] ?? 0.02, // slow: shown per minute
  'flare.requests_failed_total': (l) => ({ local: 0.0015, 'spark-all': 0 })[l.target] ?? 0,
};
/** The counter points of the metrics snapshot for the serving targets. */
export function flareCounterPoints(mode, P) {
  const out = [];
  for (const t of flareServing(mode)) {
    const L = { target: t };
    const base = t === 'spark-all' ? 3 : 1;
    out.push(
      P('flare.generated_total', 182400 + base * 1000, 'tokens', L),
      P('flare.prefill_total', 912000 + base * 1000, 'tokens', L),
      P('flare.cache_reused_total', 240000 + base * 1000, 'tokens', L),
      P('flare.requests_completed_total', 412 + base, '1', L),
      P('flare.requests_failed_total', t === 'local' ? 3 : 0, '1', L),
      P('flare.active_requests', base + 1, '1', L),
      P('flare.pending_requests', t === 'spark-all' ? 1 : 0, '1', L),
      P('flare.draft_acceptance', 0.71, 'ratio', L),
      P('flare.max_model_len', FLARE_MODELS[t]?.[1] ?? 262144, 'tokens', L),
    );
  }
  return out;
}

// ------------------------------------------------------------------------------------------------ audit
export const AUDIT_ACTORS = ['ops-console', 'ci-runner', 'maintainer-a', 'maintainer-b', 'report-bot'];
const AUDIT_SYSTEMS = [
  // system, weight, [action ids]
  ['switchyard', 26, ['config.apply routes', 'config.rollback routes', 'action restart', 'action dry_run_config']],
  ['dynamo', 14, ['action up', 'action down', 'action restart', 'params']],
  ['relay', 9, ['config.apply plugins', 'action restart']],
  ['guardrails', 12, ['action check', 'config.apply lab-input-output']],
  ['openshell', 6, ['action delete_sandbox', 'action list_sandboxes']],
  ['dcgm', 7, ['action diag', 'action diag_level2']],
  ['aiperf', 8, ['action profile']],
  ['evaluator', 6, ['action run']],
  ['datadesigner', 5, ['action create', 'action preview']],
  ['flare', 3, ['action preflight', 'action up', 'action down']],
  ['core', 4, ['settings']],
];
const pathFor = (system, action) => {
  if (action === 'settings') return '/api/v1/settings';
  const m = /^config\.(\w+) (\S+)$/.exec(action);
  if (m) return `/api/v1/systems/${system}/config/${m[2]}/${m[1]}`;
  if (action === 'params') return `/api/v1/systems/${system}/params`;
  return `/api/v1/systems/${system}/actions/${action.replace(/^action /, '')}`;
};
/**
 * Thirty days of audited calls, newest first, ids 1..n oldest to newest (the live mock appends its own after them): five actors, eleven systems, about 7% ending in an error status; a quiet
 * weekend. Rows carry the stored shape {id, ts, actor, role, method, path, system, params, status} with the parameters already redacted.
 */
export function auditSeeds(now) {
  const rnd = lcg(0xa0d17);
  const rows = [];
  const weights = AUDIT_SYSTEMS.reduce((a, s) => a + s[1], 0);
  for (let d = 29; d >= 0; d--) {
    // the 24 hours that ended d days ago (so the last 24 hours are as full as any other)
    const from = now - (d + 1) * DAY;
    const dow = new Date((from + DAY / 2) * 1000).getUTCDay();
    const n = d === 0 ? 14 + Math.round(rnd() * 8) : Math.round((dow === 0 || dow === 6 ? 4 : 13) * (0.5 + rnd() * 1.2) * (d < 3 ? 1.3 : 1));
    for (let i = 0; i < n; i++) {
      const ts = Math.floor(from + rnd() * DAY);
      if (ts > now - 60) continue;
      let r = rnd() * weights,
        sys = AUDIT_SYSTEMS[0];
      for (const s of AUDIT_SYSTEMS) {
        if (r < s[1]) {
          sys = s;
          break;
        }
        r -= s[1];
      }
      const actor = AUDIT_ACTORS[Math.min(AUDIT_ACTORS.length - 1, Math.floor(Math.pow(rnd(), 1.7) * AUDIT_ACTORS.length))];
      const action = sys[2][Math.floor(rnd() * sys[2].length)];
      const bad = rnd() < (sys[0] === 'dynamo' ? 0.14 : 0.05);
      const status = bad ? [409, 422, 502, 503, 500][Math.floor(rnd() * 5)] : 200;
      const method = /^config\.apply|settings|^params/.test(action) ? 'PUT' : 'POST';
      rows.push({
        ts,
        actor,
        role: actor === 'report-bot' ? 'viewer' : 'admin',
        method: action.startsWith('config.') ? 'POST' : method,
        path: pathFor(sys[0], action),
        system: sys[0],
        params: action === 'action restart' ? { restart: true } : action === 'action profile' ? { preset: 'chat-short' } : {},
        status,
      });
    }
  }
  rows.sort((a, b) => a.ts - b.ts);
  rows.forEach((r, i) => (r.id = i + 1));
  return rows.reverse();
}

export const SUMMARY_WINDOWS = { '24h': DAY, '7d': 7 * DAY, '30d': 30 * DAY };
const dayOf = (ts) => new Date(ts * 1000).toISOString().slice(0, 10);
/** `GET /audit/summary?window=`: aggregate counts over the rows (no parameters, no paths). `denied` is the console's own counter of refused requests. */
export function auditSummary(rows, window, now, denied = 23) {
  // addendum 9.16 (pinned): {window, from, to, bucket_s, by_day, by_actor, errors, denied_total, truncated}; a null system is the console's own calls (settings)
  const span = SUMMARY_WINDOWS[window];
  const since = now - span;
  const inWin = rows.filter((r) => r.ts >= since && r.ts <= now);
  const key = (...p) => p.join('\u0000');
  const day = new Map(),
    actor = new Map();
  for (const r of inWin) {
    const sys = r.system || 'core';
    const kd = key(dayOf(r.ts), sys);
    day.set(kd, (day.get(kd) || 0) + 1);
    const ka = key(r.actor || 'unknown', sys);
    actor.set(ka, (actor.get(ka) || 0) + 1);
  }
  const bucket = window === '24h' ? 3600 : window === '7d' ? 6 * 3600 : DAY;
  const errs = new Map();
  for (const r of inWin) {
    if ((r.status ?? 200) < 400) continue;
    const b = Math.floor(r.ts / bucket) * bucket;
    errs.set(b, (errs.get(b) || 0) + 1);
  }
  const sysOut = (s) => (s === 'core' ? null : s);
  return {
    window,
    from: since,
    to: now,
    bucket_s: bucket,
    by_day: [...day]
      .map(([k, count]) => ({ day: k.split('\u0000')[0], system: sysOut(k.split('\u0000')[1]), count }))
      .sort((a, b) => (a.day < b.day ? -1 : a.day > b.day ? 1 : String(a.system) < String(b.system) ? -1 : 1)),
    by_actor: [...actor]
      .map(([k, count]) => ({ actor: k.split('\u0000')[0], system: sysOut(k.split('\u0000')[1]), count }))
      .sort((a, b) => b.count - a.count || (a.actor < b.actor ? -1 : 1))
      .slice(0, 200),
    errors: [...errs].map(([ts_bucket, count]) => ({ ts_bucket, count })).sort((a, b) => a.ts_bucket - b.ts_bucket),
    denied_total: denied,
    truncated: actor.size > 200,
  };
}

/**
 * The log filters of `GET /audit` (addendum 9.16): `since` / `until` epoch seconds (an unparseable one is null and the caller answers 422), `actor` exact, `status` ok (< 400) | error (>= 400),
 * `system`, `limit`. Returns {rows} or {error}.
 */
export function filterAudit(rows, sp) {
  const num = (k) => {
    const v = sp.get(k);
    if (v == null || v === '') return null;
    return /^\d{1,11}(\.\d+)?$/.test(v) ? Number(v) : NaN;
  };
  const since = num('since'),
    until = num('until');
  if (Number.isNaN(since) || Number.isNaN(until)) return { error: 'since and until are epoch seconds' };
  const status = sp.get('status');
  if (status && status !== 'ok' && status !== 'error') return { error: "status must be 'ok' or 'error'" };
  const actor = sp.get('actor'),
    system = sp.get('system');
  const limit = Math.max(1, Math.min(500, Number(sp.get('limit')) || 100));
  const out = rows.filter(
    (r) =>
      (since == null || r.ts >= since) &&
      (until == null || r.ts <= until) &&
      (!actor || r.actor === actor) &&
      (!system || r.system === system) &&
      (!status || (status === 'ok' ? (r.status ?? 200) < 400 : (r.status ?? 200) >= 400)),
  );
  return { rows: out.slice(0, limit) };
}
