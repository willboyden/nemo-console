// MOCK of the Relay runtime pieces of docs/ARCHITECTURE.md addendum 9.17: the gateway-resource metric points, the capture answer's
// `resources` object and `resource_metrics` level, the `relay.resources.off` hint, and `GET /systems/relay/diagnostics`. MOCK-ONLY:
// every number and name here is invented so the cards can be built and screenshotted; the SHAPES follow the contract (the answer
// the console builds, not raw Relay output: no `detail`, `correlation`, owner, instance or process ids ever appear in it).
//
// Dev switches (see dev/mock-server.mjs): MOCK_RESOURCES=on|quiet|stale|idle|off|polling_off|unreadable   MOCK_DIAG=ok|clean|truncated|disabled|unreachable|unsupported_version|error
//   (or /__dev/traces?resources=&diag=).  A viewer is refused the diagnostics route with 403, like the real one.
//   Shapes as the real backend answers them: the `resource_metrics` row has detail {polling, interval_ms}, observed {last_ts} (null when
//   nothing was seen) and `group: "other"` (this row only); `resources` is {configured, enabled, scope, interval_s, gpu, network}, all six
//   null when plugins.toml cannot be read (a component that is switched off counts as not configured); `relay.resources.off` fires whenever
//   the row is off; an unreadable file gives `capture.config.unreadable` instead.

export const RESOURCE_MODES = ['on', 'quiet', 'stale', 'idle', 'off', 'polling_off', 'unreadable'];
export const DIAG_MODES = ['ok', 'clean', 'truncated', 'disabled', 'unreachable', 'unsupported_version', 'error'];
export const RESOURCES_ON_COMMAND = 'python3 -I ops/nemo-relay-levels.py resources on';
const INTERVAL_S = 60;
const SCOPE = 'application_process'; // the value Relay's own configuration uses (`measurement_scope`)
/** Modes in which the sampler is on: configured, polling enabled. */
const SAMPLING = new Set(['on', 'quiet', 'stale', 'idle']);
/** Modes in which the row is `off` (so the backend adds the `relay.resources.off` hint). */
const OFF = new Set(['off', 'polling_off']);

/** The capture answer's `resources` object for a mode. */
export function resourcesOf(mode) {
  if (mode === 'unreadable') return { configured: null, enabled: null, scope: null, interval_s: null, gpu: null, network: null };
  if (mode === 'off') return { configured: false, enabled: false, scope: null, interval_s: null, gpu: null, network: null };
  if (mode === 'polling_off') return { configured: true, enabled: false, scope: SCOPE, interval_s: null, gpu: false, network: false };
  return { configured: true, enabled: true, scope: SCOPE, interval_s: INTERVAL_S, gpu: false, network: false };
}

/** The `resource_metrics` row of the capture matrix: active while the newest mark is within 3 intervals, stale beyond, idle with none seen. */
export function resourceLevel(mode, nowS) {
  const on = SAMPLING.has(mode);
  const lastTs = mode === 'on' || mode === 'quiet' ? Math.floor(nowS) - 41 : mode === 'stale' ? Math.floor(nowS) - 1500 : null;
  return {
    id: 'resource_metrics',
    group: 'other', // the one level the backend files under "Other" itself (addendum 9.17)
    label: 'Gateway resource metrics (CPU, memory, disk of the gateway process)',
    enabled: on,
    state: { on: 'active', quiet: 'active', stale: 'stale', idle: 'idle', unreadable: 'unknown' }[mode] ?? 'off',
    // the pinned detail of 9.12; what the sampler is set to sample is in the capture answer's `resources`
    detail: on ? { polling: true, interval_ms: INTERVAL_S * 1000 } : { polling: mode === 'polling_off' ? false : null, interval_ms: null },
    observed: { last_ts: lastTs },
  };
}

/** The hint the backend adds when the row is off (code `relay.resources.off`), or, with an unreadable plugins.toml, `capture.config.unreadable`; else null. */
export const resourceHint = (mode) =>
  OFF.has(mode)
    ? {
        code: 'relay.resources.off',
        message: `Gateway resource metrics are off: the gateway's CPU, memory and disk are not sampled. Add a \`resource_metrics\` component with polling enabled to plugins.toml and restart the gateway (in the lab: \`${RESOURCES_ON_COMMAND}\`).`,
      }
    : mode === 'unreadable'
      ? { code: 'capture.config.unreadable', message: 'plugins.toml could not be read or does not parse, so the capture configuration is unknown.' }
      : null;

/**
 * The points the Relay metrics snapshot gains while the sampler runs (none when it is off: a key is emitted only for a finite value).
 * `wob(base, amplitude)` is the mock server's own wobble.
 */
export function resourcePoints(mode, wob) {
  if (mode === 'off' || mode === 'polling_off' || mode === 'unreadable' || mode === 'idle') return []; // nothing sampled yet, so no key
  const quiet = mode === 'quiet';
  const P = (key, value, unit) => ({ key, value: Math.max(0, +value.toFixed(4)), unit, labels: {} });
  return [
    P('relay.gw_cpu', wob(0.00052, 0.00018), 'cores'), // a lightly loaded gateway uses a few hundredths of a core or less: the axis must not round it to 0
    P('relay.gw_rss', wob(188e6, 4e6), 'B'),
    P('relay.gw_mem_accounted', wob(236e6, 5e6), 'B'),
    P('relay.gw_threads', Math.round(wob(31, 2)), '1'),
    P('relay.gw_fds', Math.round(wob(74, 6)), '1'),
    P('relay.gw_disk_read', wob(2.1e4, 6e3), 'B/s'),
    P('relay.gw_disk_write', wob(9.4e4, 2e4), 'B/s'),
    P('relay.atof_fs_free', 412e9, 'B'),
    P('relay.atof_fs_used', 0.37, 'ratio'),
    P('relay.gw_cpu_pressure', wob(1.4, 0.8), 'ms/s'),
    P('relay.gw_mem_pressure', 0, 'ms/s'),
    P('relay.gw_limit_events', 0, '1'),
    // a gateway that has routed nothing in the window reports 0 for the rates (and no overhead quantile: there is nothing to take it of), not nothing
    P('relay.route_rps', quiet ? 0 : wob(0.62, 0.2), 'req/s'),
    P('relay.route_failures', quiet ? 0 : wob(0.004, 0.003), 'req/s'),
    P('relay.route_tokens', quiet ? 0 : wob(84, 20), 'tok/s'),
    ...(quiet ? [] : [P('relay.route_overhead_p95', wob(0.31, 0.08), 'ms')]),
  ];
}

// ---------------------------------------------------------------------------------------------------------------- diagnostics
const iso = (epochS, nanos = '000000000') => `${new Date(Math.floor(epochS) * 1000).toISOString().slice(0, 19)}.${nanos}Z`;
// components in the order Relay registers them; [key, instances, successes, failures, last failure age s | null]
const COMPONENTS = [
  ['cli.command', 1, 4, 0, null],
  ['cli.configuration', 1, 2, 0, null],
  ['cli.gateway', 1, 11, 0, null],
  ['cli.acquire', 1, 6, 0, null],
  ['cli.child', 3, 9, 0, null],
  ['cli.hook', 2, 318, 0, null],
  ['cli.upstream', 1, 1204, 3, 2520],
  ['plugin.native_abi', 2, 47, 0, null],
  ['plugin.worker_protocol', 2, 188, 6, 780],
  ['plugin.policy', 1, 95, 0, null],
  ['plugin.configuration', 1, 12, 0, null],
];
const EVENT_KINDS = [
  // code, component, severity, outcome
  ['upstream.request.completed', 'cli.upstream', 'info', 'success'],
  ['hook.decision.recorded', 'cli.hook', 'info', 'success'],
  ['worker.reply.timeout', 'plugin.worker_protocol', 'error', 'failure'],
  ['upstream.request.retried', 'cli.upstream', 'warn', 'failure'],
  ['policy.evaluated', 'plugin.policy', 'info', 'success'],
  ['worker.frame.dropped', 'plugin.worker_protocol', 'warn', 'unspecified'],
  ['native.emission.accepted', 'plugin.native_abi', 'info', 'success'],
  ['child.spawned', 'cli.child', 'info', 'success'],
];
const LOSS = {
  events_evicted: 0,
  events_omitted: 0,
  components_evicted: 0,
  components_rejected: 0,
  keys_rejected: 0,
  observations_rejected: 0,
  messages_truncated: 0,
  details_omitted: 0,
  correlations_truncated: 0,
  correlations_omitted: 0,
  metadata_rejected: 0,
  classifications_overflowed: 0,
  windows_rejected: 0,
  queries_rejected: 0,
  stale_observations: 0,
  projection_deliveries_omitted: 0,
  projections_panicked: 0,
  projections_suppressed: 143,
  console_dropped: 0,
};

/** The `GET /systems/relay/diagnostics` answer for a mode: `ok` (a few failures), `clean` (none), `truncated` (the gateway holds more events than were gathered), or one of the four reasons. */
export function diagnosticsAnswer(mode, nowS) {
  if (['disabled', 'unreachable', 'unsupported_version', 'error'].includes(mode)) {
    return {
      available: false,
      reason: mode,
      relay_version: null,
      captured_at: null,
      elapsed_s: null,
      totals: null,
      last_failure_at: null,
      components: [],
      events: [],
      loss: {},
      retention: null,
      memory: null,
    };
  }
  const clean = mode === 'clean';
  const truncated = mode === 'truncated';
  const t = Math.floor(nowS);
  const components = COMPONENTS.map(([key, instances, successes, failures, age]) => {
    const f = clean ? 0 : failures;
    return { key, instances, successes, failures: f, occurrences: successes + f, last_failure_at: f && age != null ? iso(t - age, '412907003') : null };
  });
  // the order the server gives them: most failures first, then by key (the page does not re-sort)
  components.sort((a, b) => b.failures - a.failures || (a.key < b.key ? -1 : a.key > b.key ? 1 : 0));
  const sum = (k) => components.reduce((a, c) => a + c[k], 0);
  const failures = sum('failures');
  const unspecified = 43;
  const events = [];
  const total = 150;
  const latestSeq = 4187;
  for (let i = 0; i < total; i++) {
    let [code, component, severity, outcome] = EVENT_KINDS[(i * 5 + (i % 3)) % EVENT_KINDS.length];
    if (clean && outcome === 'failure') [code, component, severity, outcome] = EVENT_KINDS[0];
    events.push({
      seq: latestSeq - i,
      at: iso(t - 4 - i * 37, String(100000000 + ((i * 7919) % 899999999))),
      component,
      code,
      failure_class: outcome === 'failure' ? 'transient' : 'none',
      severity,
      outcome,
      count: 1 + ((i * 13) % 5),
    });
  }
  return {
    available: true,
    reason: null,
    relay_version: '0.11.1',
    captured_at: iso(t, '295079873'),
    elapsed_s: 190112,
    totals: { recorded: sum('occurrences') + unspecified, successes: sum('successes'), failures, unspecified, evictions: 0 },
    last_failure_at: failures ? iso(t - 780, '412907003') : null,
    components,
    events,
    loss: { ...LOSS },
    // `truncated`: the gateway holds more events than the console gathered (it follows continuation pages up to a cap), so the retained count is above the list
    retention: truncated
      ? { events_retained: total + 106, oldest_seq: latestSeq - total - 105, latest_seq: latestSeq, truncated: true }
      : { events_retained: total, oldest_seq: latestSeq - total + 1, latest_seq: latestSeq, truncated: false },
    memory: { reserved_bytes: 2595089, events: total, event_capacity: 256, detail_bytes: 38400, detail_capacity_bytes: 262144 },
  };
}
