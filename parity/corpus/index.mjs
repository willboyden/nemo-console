// The corpus: scenarios and the ORDERED case list of each. Cases run in this order on both backends (state carries
// over from case to case: config revisions, settings, audit rows, the fake machine's containers).
//
// A case is a plain object:
//   id        unique id                          route   "METHOD /template" row of routes.python.json ("CLI ..." for CLI cases)
//   method    default GET                        path    raw request path (sent as is: no normalisation by the client)
//   auth      'admin' (default) | 'viewer' | 'none' | 'bad' | 'raw:<Authorization value>'
//   host      Host header (default 127.0.0.1:<port>; null = no Host header)    csrf: false = no X-Console header
//   headers   extra headers                      json    body sent as JSON    body + contentType: raw body
//   chunked   send the body chunked              readMs  read a streaming body for this long (SSE), then disconnect
//   capture   {var: '/json/pointer' | {header}}  -> {{var}} in later cases (per backend)
//   during    {afterMs, run(side)}  side effect while the request is open (e.g. append to a tailed file)
//   after     async (side) => ...   runs before the case's CLI / upstream logs are read (background jobs)
//   kind      'cli' with args: [...] runs the backend's CLI instead of an HTTP request
//   timeoutMs request timeout (default 120 s)      pauseMs  wait this long before the case (lets TTL caches expire)
// Built-in vars: {{port}} {{origin}} {{home}} {{repo}} {{state}}.
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { FIXTURES } from '../lib/seed.mjs';
import { securityCases, roleSweepCases, staticCases, globalReadCases, auditCases, settingsCases, metricsCases, cliCases } from './core.mjs';
import {
  SYSTEM_IDS, systemReadCases, systemReadVariantCases, configReadCases, sseCases, sseAppendCases, configRoundTrip,
  configRefusalCases, paramsCases, actionRouteCases, action, waitRuns, captureNewestRun, playgroundCases, ddRunCases,
  concurrencyCases,
} from './systems.mjs';

const LIFECYCLE_ENV = {
  NEMO_CONSOLE_BACKGROUND_TASKS: 'true', NEMO_CONSOLE_POLL_INTERVAL_S: '0.5',
  NEMO_CONSOLE_SSE_IDLE_TIMEOUT_S: '1.5', NEMO_CONSOLE_SSE_HEARTBEAT_S: '0.4', NEMO_CONSOLE_SHUTDOWN_GRACE_S: '2',
  NEMO_CONSOLE_CAPABLE_PROBE: 'true', NEMO_CONSOLE_CAPABLE_PROBE_RETRY_S: '0', NEMO_CONSOLE_CAPABLE_PROBE_TIMEOUT_S: '2',
};

export const SCENARIOS = {
  // seeded lab repo + HOME, fake CLIs on PATH, fake upstreams: every adapter reaches its healthy and action paths
  full: { name: 'full', seed: true, fakes: true, upstreams: true },
  // like the e2e suite: empty repo root and HOME, nothing on PATH, every upstream refuses connections
  empty: { name: 'empty', seed: false, fakes: false, upstreams: false },
  // NEMO_CONSOLE_TRUST_LOOPBACK=true: a loopback caller without a key is admin, unless a proxy header says otherwise
  loopback: { name: 'loopback', seed: false, fakes: false, upstreams: false, env: { NEMO_CONSOLE_TRUST_LOOPBACK: 'true' } },
  // the moving parts: background tasks ON (metrics poller every 0.5 s, rollups), the active capable-tier probe, a short
  // SSE idle timeout and heartbeat, and SIGTERM while a stream is open. Calls happen between cases, so only the
  // whole-run distinct argv / upstream sets are compared (wholeRunCallsOnly).
  lifecycle: { name: 'lifecycle', seed: true, fakes: true, upstreams: true, wholeRunCallsOnly: true, env: LIFECYCLE_ENV },
  // the same with the router unreachable: the probe's verdict is "broken" (tried twice) and Switchyard degrades
  'probe-broken': { name: 'probe-broken', seed: true, fakes: true, upstreams: true, wholeRunCallsOnly: true,
    env: { ...LIFECYCLE_ENV, NEMO_CONSOLE_URLS__LITELLM: 'http://127.0.0.1:9' } },
};

// config files of the seeded repo, per system (GET /config lists them)
const FILES = {
  switchyard: ['routes'], relay: ['routes', 'plugins', 'config'], dynamo: ['compose', 'override'],
  datadesigner: ['reviews_smoke.py', 'tiny.yaml'], openshell: ['gateway', 'policy', 'provider-profile', 'compose'],
  dcgm: ['counters'], aiperf: ['profiles'],
  guardrails: ['lab-input-output--actions.py', 'lab-input-output--config.yml', 'lab-input-output--kb--faq.md', 'lab-input-output--prompts.yml'],
  evaluator: ['suites.yaml'],
};

const DYNAMO_VALID = { max_model_len: 131072, gpu_memory_utilization: 0.6, num_speculative_tokens: 3, enable_prefix_caching: false, router_mode: 'kv', reasoning_parser: 'none' };
const DYNAMO_INVALID = [{ max_num_seqs: 0 }, { router_mode: 'direct' }, { max_model_len: true }, { gpu_memory_utilization: 1.5 },
  { nope: 1 }, { max_num_seqs: 16.5 }, { max_num_seqs: 16.0 }, { tool_call_parser: 5 }, { enable_prefix_caching: 'false' }];

function actionsFull() {
  const A = (system, id, params, o = {}) => action(system, id, params, o);
  return [
    // switchyard
    A('switchyard', 'dry_run_config', {}),
    A('switchyard', 'restart', {}, { confirm: true }),
    A('switchyard', 'reset_stats', {}, { confirm: true }),
    // relay
    A('relay', 'gateway_status', {}), A('relay', 'doctor_offline', {}), A('relay', 'agents_list', {}),
    A('relay', 'plugins_validate', {}), A('relay', 'gateway_restart', {}, { confirm: true }),
    // dynamo: the card state machine (worker + an engine sharing its card at start)
    A('dynamo', 'preflight', {}),
    A('dynamo', 'up_mock', {}, { tag: 'worker-running' }),
    A('dynamo', 'down', {}, { confirm: true }),
    A('dynamo', 'up_mock', {}),
    A('dynamo', 'up', {}, { confirm: true, tag: 'mocker-running' }),
    A('dynamo', 'down', {}, { confirm: true, tag: 'again' }),
    A('dynamo', 'up', {}, { confirm: true, tag: 'card-in-use' }),
    A('dynamo', 'up', { stop_sharing_engine: true }, { confirm: true, tag: 'stop-sharer' }),
    A('dynamo', 'restart', {}, { confirm: true }),
    A('dynamo', 'preflight', {}, { tag: 'after' }),
    A('dynamo', 'up', { stop_sharing_engine: 'yes' }, { confirm: true, tag: 'bad-bool' }),
    // skillevaluator
    A('skillevaluator', 'doctor', {}),
    A('skillevaluator', 'report', {}, { tag: 'none-yet' }),
    A('skillevaluator', 'evaluate', { skill: 'robust-scripts', tiers: 'static' }),
    A('skillevaluator', 'evaluate', { skill: 'robust-scripts', tiers: 'tier1' }, { tag: 'tier1' }),
    A('skillevaluator', 'evaluate', { skill: 'serve-model', tiers: 'tier1+2' }, { tag: 'tier12' }),
    A('skillevaluator', 'evaluate', { skill: 'nope', tiers: 'tier1' }, { tag: 'bad-skill' }),
    A('skillevaluator', 'evaluate_all_static', {}),
    A('skillevaluator', 'report', {}),
    A('skillevaluator', 'report', { run_id: 'bogus' }, { tag: 'bogus' }),
    A('skillevaluator', 'report', { run_id: '20200101T000000-000-nope' }, { tag: 'unknown' }),
    A('skillevaluator', 'evaluate_live', { skill: 'robust-scripts', attempts: 2, dataset: 'auto', skip_baseline: true }, { confirm: true, extra: { after: waitRuns('skillevaluator') } }),
    A('skillevaluator', 'report', {}, { tag: 'after-live' }),
    A('skillevaluator', 'evaluate_live', { skill: 'serve-model', agent_model: 'bad model!' }, { confirm: true, tag: 'bad-model' }),
    A('skillevaluator', 'evaluate_live', { attempts: 6 }, { confirm: true, tag: 'attempts-over' }),
    // datadesigner
    A('datadesigner', 'show_config', {}),
    A('datadesigner', 'check_models', { script: 'reviews_smoke.py' }, { confirm: true }),
    A('datadesigner', 'report', {}, { tag: 'none-yet' }),
    A('datadesigner', 'preview', { script: 'reviews_smoke.py', num_records: 2 }, { confirm: true, extra: { after: captureNewestRun('datadesigner', 'ddPreviewRun') } }),
    A('datadesigner', 'report', {}),
    A('datadesigner', 'create', { script: 'tiny.yaml', num_records: 2, format: 'jsonl' }, { confirm: true, extra: { after: async (side) => { await waitRuns('datadesigner')(side); await captureNewestRun('datadesigner', 'ddCreateRun')(side); } } }),
    A('datadesigner', 'create', { script: 'reviews_smoke.py', num_records: 2, format: 'csv', dataset_name: 'parity_csv', destination_subdir: 'parity/csv' }, { confirm: true, tag: 'csv', extra: { after: waitRuns('datadesigner') } }),
    A('datadesigner', 'create', { script: 'reviews_smoke.py', num_records: 2, destination_subdir: '../escape' }, { confirm: true, tag: 'escape' }),
    A('datadesigner', 'create', { script: 'nope.py' }, { confirm: true, tag: 'bad-script' }),
    A('datadesigner', 'report', { run_id: '{{ddCreateRun}}' }, { tag: 'create' }),
    A('datadesigner', 'report', { run_id: 'bogus' }, { tag: 'bogus' }),
    // openshell
    A('openshell', 'doctor', {}), A('openshell', 'list_sandboxes', {}),
    A('openshell', 'sandbox_get', { name: 'nc-adapter-probe' }),
    A('openshell', 'sandbox_get', { name: 'nope' }, { tag: 'unknown' }),
    A('openshell', 'sandbox_logs', { name: 'nc-adapter-probe', lines: 5 }),
    A('openshell', 'gateway_logs', { lines: 5 }),
    A('openshell', 'prove_policy', {}),
    A('openshell', 'prove_policy', { compact: false, scopes: 'chat,models' }, { tag: 'scopes' }),
    A('openshell', 'prove_policy', { scopes: 'BAD!' }, { tag: 'bad-scope' }),
    A('openshell', 'gateway_stop', {}, { confirm: true, tag: 'refused' }),
    A('openshell', 'delete_sandbox', { name: 'nc-adapter-probe' }, { confirm: true }),
    A('openshell', 'gateway_stop', {}, { confirm: true }),
    A('openshell', 'list_sandboxes', {}, { tag: 'gateway-down' }),
    A('openshell', 'gateway_up', {}, { confirm: true }),
    // dcgm
    A('dcgm', 'discovery', {}),
    A('dcgm', 'diag', { gpu: '0' }, { confirm: true }),
    A('dcgm', 'diag_level2', { gpu: '1' }, { confirm: true }),
    A('dcgm', 'restart_exporter', {}, { confirm: true }),
    // aiperf
    A('aiperf', 'report', {}, { tag: 'none-yet' }),
    A('aiperf', 'profile', { preset: 'smoke-lightning' }, { confirm: true, extra: { after: waitRuns('aiperf') } }),
    A('aiperf', 'report', {}),
    A('aiperf', 'report', { run_id: 'bogus' }, { tag: 'bogus' }),
    A('aiperf', 'profile', { preset: 'nope' }, { confirm: true, tag: 'bad-preset' }),
    // guardrails
    A('guardrails', 'check', { config: 'lab-input-output' }, { confirm: true }),
    A('guardrails', 'restart', {}, { confirm: true }),
    A('guardrails', 'stop', {}, { confirm: true }),
    A('guardrails', 'up', {}, { confirm: true }),
    // evaluator
    A('evaluator', 'report', {}, { tag: 'none-yet' }),
    A('evaluator', 'run', { suite: 'smoke-lightning' }, { confirm: true, extra: { after: waitRuns('evaluator') } }),
    A('evaluator', 'report', {}),
    A('evaluator', 'report', { run_id: 'bogus' }, { tag: 'bogus' }),
    A('evaluator', 'run', { suite: 'nope' }, { confirm: true, tag: 'bad-suite' }),
  ];
}

function actionsEmpty() {
  const A = (system, id, params, o = {}) => action(system, id, params, o);
  return [
    A('switchyard', 'dry_run_config', {}), A('switchyard', 'restart', {}, { confirm: true }), A('switchyard', 'reset_stats', {}, { confirm: true }),
    A('relay', 'doctor_offline', {}), A('relay', 'plugins_validate', {}), A('relay', 'gateway_restart', {}, { confirm: true }),
    A('dynamo', 'preflight', {}), A('dynamo', 'up', {}, { confirm: true }), A('dynamo', 'down', {}, { confirm: true }),
    A('skillevaluator', 'doctor', {}), A('skillevaluator', 'evaluate', { tiers: 'static' }),
    A('datadesigner', 'show_config', {}), A('datadesigner', 'report', {}),
    A('openshell', 'doctor', {}), A('openshell', 'gateway_up', {}, { confirm: true }),
    A('dcgm', 'discovery', {}), A('dcgm', 'restart_exporter', {}, { confirm: true }),
    A('aiperf', 'report', {}), A('aiperf', 'profile', {}, { confirm: true }),
    A('guardrails', 'check', {}, { confirm: true }), A('guardrails', 'up', {}, { confirm: true }),
    A('evaluator', 'report', {}), A('evaluator', 'run', {}, { confirm: true }),
  ];
}

function fullCases() {
  return [
    ...securityCases(),
    ...roleSweepCases(),
    ...staticCases(),
    ...globalReadCases(),
    ...systemReadCases('read'),
    ...systemReadVariantCases(),
    ...configReadCases(FILES),
    ...sseCases(1500),
    ...sseAppendCases(),
    ...concurrencyCases(),
    ...playgroundCases('pg'),
    ...actionRouteCases(),
    // config round trips (validate -> apply -> history -> rollback), one per editable file kind
    ...configRoundTrip('switchyard', 'routes', { invalid: '[routes\nbroken = \n', unhealthyMarker: 'parity-recreate-fail' }),
    ...configRoundTrip('relay', 'routes', { invalid: '[[routes]\n' }),
    ...configRoundTrip('dynamo', 'override', { appendText: readFileSync(join(FIXTURES, 'dynamo', 'golden_override.yml'), 'utf8'), invalid: 'services:\n  dynamo-worker:\n    privileged: true\n' }),
    ...configRoundTrip('datadesigner', 'reviews_smoke.py', { tag: 'script', invalid: '# parity-invalid\n' }),
    ...configRoundTrip('dcgm', 'counters', { invalid: 'DCGM_FI_DEV_GPU_TEMP, gauge\n' }),
    { id: 'cfg.dcgm.counters.validate.unknown-field', route: 'POST /api/v1/systems/{system_id}/config/{file_id}/validate', method: 'POST', path: '/api/v1/systems/dcgm/config/counters/validate', json: { content: 'DCGM_FI_NOPE_NOT_A_FIELD, gauge, nope\nDCGM_FI_DEV_GPU_TEMP, gauge, temp\n' } },
    ...configRoundTrip('aiperf', 'profiles', { invalid: 'presets: [\n' }),
    ...configRoundTrip('guardrails', 'lab-input-output--prompts.yml', { tag: 'prompts', invalid: '# parity-invalid\nprompts: []\n' }),
    ...configRoundTrip('evaluator', 'suites.yaml', { tag: 'suites', invalid: 'suites: [\n' }),
    ...configRefusalCases(),
    ...actionsFull(),
    ...paramsCases(DYNAMO_VALID, DYNAMO_INVALID),
    { id: 'params.dynamo.preview.nan', route: 'POST /api/v1/systems/{system_id}/params/preview', method: 'POST', path: '/api/v1/systems/dynamo/params/preview', body: '{"values":{"gpu_memory_utilization":NaN}}', contentType: 'application/json' },
    ...ddRunCases(),
    ...settingsCases(),
    ...playgroundCases('pg-after-settings').filter((c) => /\.(relay|dynamo|switchyard)\.nonstream$/.test(c.id)),
    // the 5 s engine-probe / topology caches of both backends expire before the state is read again
    ...systemReadCases('reread').map((c, i) => (i === 0 ? { ...c, pauseMs: 5200 } : c)),
    ...globalReadCases().map((c) => ({ ...c, id: `${c.id}.after` })),
    ...auditCases('end'),
    ...cliCases(),
    ...metricsCases(),
  ];
}

function emptyCases() {
  return [
    ...securityCases().filter((c) => !c.id.includes('over-cap')),
    ...staticCases(),
    ...globalReadCases(),
    ...systemReadCases('read'),
    ...systemReadVariantCases(),
    ...SYSTEM_IDS.map((id) => ({ id: `empty.config-get.${id}`, route: 'GET /api/v1/systems/{system_id}/config/{file_id}', path: `/api/v1/systems/${id}/config/routes` })),
    ...sseCases(800),
    ...playgroundCases('pg').filter((c) => /\.(switchyard|relay|dynamo|guardrails)\.nonstream$|no-playground|bad-model/.test(c.id)),
    ...actionRouteCases().filter((c) => /unknown-action|no-confirm|viewer/.test(c.id)),
    ...configRefusalCases().filter((c) => /unknown-file|unknown-system/.test(c.id)),
    ...actionsEmpty(),
    ...paramsCases(DYNAMO_VALID, DYNAMO_INVALID.slice(0, 2)).filter((c) => !/put\.ok|after/.test(c.id)),
    ...settingsCases().filter((c) => /poll\b|url-loopback|key-env-value|get\.after$/.test(c.id)),
    ...auditCases('end'),
    ...cliCases(),
    ...metricsCases(),
  ];
}

function loopbackCases() {
  const H = 'GET /api/v1/health';
  return [
    { id: 'lb.health.no-key', route: H, path: '/api/v1/health', auth: 'none' },
    { id: 'lb.health.xff', route: H, path: '/api/v1/health', auth: 'none', headers: { 'X-Forwarded-For': '203.0.113.9' } },
    { id: 'lb.health.forwarded', route: H, path: '/api/v1/health', auth: 'none', headers: { Forwarded: 'for=203.0.113.9' } },
    { id: 'lb.health.viewer-key', route: H, path: '/api/v1/health', auth: 'viewer' },
    { id: 'lb.health.bad-key', route: H, path: '/api/v1/health', auth: 'bad' },
    { id: 'lb.systems.no-key', route: 'GET /api/v1/systems', path: '/api/v1/systems', auth: 'none' },
    { id: 'lb.systems.xff', route: 'GET /api/v1/systems', path: '/api/v1/systems', auth: 'none', headers: { 'X-Forwarded-For': '127.0.0.1' } },
    { id: 'lb.audit.no-key', route: 'GET /api/v1/audit', path: '/api/v1/audit', auth: 'none' },
    { id: 'lb.settings.no-key', route: 'GET /api/v1/settings', path: '/api/v1/settings', auth: 'none' },
    { id: 'lb.settings.put.no-key', route: 'PUT /api/v1/settings', method: 'PUT', path: '/api/v1/settings', auth: 'none', json: { poll_interval_s: 4 } },
    { id: 'lb.settings.put.no-key.no-csrf', route: 'PUT /api/v1/settings', method: 'PUT', path: '/api/v1/settings', auth: 'none', csrf: false, json: { poll_interval_s: 4 } },
    { id: 'lb.action.no-key', route: 'POST /api/v1/systems/{system_id}/actions/{action}', method: 'POST', path: '/api/v1/systems/aiperf/actions/report', auth: 'none', json: {} },
    { id: 'lb.metrics.no-key', route: 'GET /metrics', path: '/metrics', auth: 'none' },
    ...auditCases('lb').slice(0, 2),
  ];
}

function lifecycleCases() {
  const S = 'GET /api/v1/systems/{system_id}/status';
  const H = 'GET /api/v1/systems/{system_id}/metrics/history';
  const E = 'GET /api/v1/systems/{system_id}/events/stream';
  const hist = (sys, window, key) => ({ id: `life.poller.${window}.${sys}.${key}`, route: H, historyShape: true,
    path: `/api/v1/systems/${sys}/metrics/history?window=${window}&key=${encodeURIComponent(key)}` });
  return [
    // the first Switchyard status schedules the probe (one tiny completion per running capable target); the second,
    // after the probe ran, shows its verdict
    { id: 'life.probe.status-1', route: S, path: '/api/v1/systems/switchyard/status' },
    { id: 'life.probe.status-2', route: S, path: '/api/v1/systems/switchyard/status', pauseMs: 2500 },
    // no event for 1.5 s: heartbeats every 0.4 s, then `event: idle-timeout` and the stream ends on its own
    { id: 'life.sse.idle-timeout', route: E, path: '/api/v1/systems/relay/events/stream', readMs: 6000, collapseKeepAlive: true },
    // the poller has run for a few seconds by now: the ring (15m) and the minute rollups (1h, 24h) hold the points
    hist('switchyard', '15m', 'sy.route.calls'), hist('switchyard', '1h', 'sy.route.calls'), hist('switchyard', '24h', 'sy.route.calls'),
    hist('dcgm', '15m', 'dcgm.gpu_temp_c'), hist('dcgm', '1h', 'dcgm.gpu_temp_c'),
    { id: 'life.metrics.after-poller', route: 'GET /metrics', path: '/metrics' },
    // SIGTERM while a stream is open: the stream ends with `event: shutdown`, the process exits (code compared)
    { id: 'life.sse.shutdown', route: E, path: '/api/v1/systems/relay/events/stream', readMs: 12_000, collapseKeepAlive: true,
      during: { afterMs: 700, run: (side) => side.b.proc.kill('SIGTERM') }, waitExit: true },
  ];
}

const BUILDERS = { full: fullCases, empty: emptyCases, loopback: loopbackCases, lifecycle: lifecycleCases, 'probe-broken': lifecycleCases };

export function casesFor(scenario) {
  const cases = BUILDERS[scenario]();
  const seen = new Set();
  for (const c of cases) {
    if (!c.id || !c.route) throw new Error(`corpus case without id/route: ${JSON.stringify(c).slice(0, 200)}`);
    if (seen.has(c.id)) throw new Error(`duplicate case id ${c.id} in scenario ${scenario}`);
    seen.add(c.id);
  }
  return cases;
}

export const ALL_SCENARIOS = Object.keys(SCENARIOS);

// ---- workstream B cases ----
// Config validation edge cases of the four workstream B systems (dcgm, aiperf, guardrails, openshell), run after the rest of
// the full scenario (validate writes nothing): the ADR 0015 rail-weakening rule and its exact marker line, YAML errors
// with a context (aiperf prints PyYAML's `e.problem`, guardrails the first line of `str(e)`, openshell `str(e)`), a
// Python exception escaping the YAML loader (an impossible date), tomllib errors and difflib's "did you mean".
function wsbCases() {
  const V = 'POST /api/v1/systems/{system_id}/config/{file_id}/validate';
  const v = (id, system, file, content) => ({ id: `wsb.${system}.${id}`, route: V, method: 'POST',
    path: `/api/v1/systems/${system}/config/${file}/validate`, json: { content } });
  const live = readFileSync(join(FIXTURES, 'guardrails', 'config-tree', 'lab-input-output', 'config.yml'), 'utf8');
  const noInput = live.replace('  input:\n    flows:\n      - self check input\n', '');
  const noRails = live.slice(0, live.indexOf('rails:'));
  const MARK = '# nemo-console: allow-weaken-rails';
  const GR = 'lab-input-output--config.yml';
  return [
    v('weaken.no-marker', 'guardrails', GR, noInput),
    v('weaken.marker', 'guardrails', GR, `${MARK}\n${noInput}`),
    v('weaken.marker-indented', 'guardrails', GR, `${noInput}    ${MARK}   \n`),
    v('weaken.marker-with-text', 'guardrails', GR, `${MARK} please\n${noInput}`),
    v('weaken.marker-as-value', 'guardrails', GR, `${noInput}note: "${MARK}"\n`),
    v('weaken.all-rails', 'guardrails', GR, noRails),
    v('weaken.rename-flow', 'guardrails', GR, live.replace('- self check output', '- self check outputs')),
    v('weaken.add-flow', 'guardrails', GR, live.replace('      - self check input\n', '      - self check input\n      - extra flow\n')),
    v('yaml.context-error', 'guardrails', GR, 'rails:\n  input: [a, b\n'),
    v('yaml.bad-date', 'guardrails', GR, 'models: 2026-02-30\n'),
    v('yaml.prompts-bad-date', 'guardrails', 'lab-input-output--prompts.yml', 'prompts: 2026-02-30\n'),
    v('yaml.context-error', 'aiperf', 'profiles', 'version: 1\nprofiles: {a: [1, 2\n'),
    v('yaml.scanner-error', 'aiperf', 'profiles', 'version: 1\nprofiles:\n  a: "unterminated\n'),
    v('typo.top-level', 'aiperf', 'profiles', 'version: 1\nprofils: {}\n'),
    v('yaml.bad-date', 'aiperf', 'profiles', 'version: 1\nprofiles:\n  x:\n    model: 2026-02-30\n'),
    v('yaml.error', 'openshell', 'policy', 'network_policies: [1\n'),
    v('yaml.bad-date', 'openshell', 'policy', 'a: 2026-02-30\n'),
    v('toml.error', 'openshell', 'gateway', '[openshell\n'),
    v('toml.duplicate', 'openshell', 'gateway', 'a = 1\na = 2\n'),
    v('toml.ok', 'openshell', 'gateway', '[openshell.drivers.docker]\nnetwork = "openshell-docker"\n'),
    v('close-match', 'dcgm', 'counters', 'DCGM_FI_DEV_GPU_TEMPP, gauge, x\n'),
    v('comments-only', 'dcgm', 'counters', '# nothing\n\n'),
  ];
}
{
  const fullBase = BUILDERS.full;
  BUILDERS.full = () => [...fullBase(), ...wsbCases()];
}
