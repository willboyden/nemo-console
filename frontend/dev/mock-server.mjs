#!/usr/bin/env node
// Dependency-free mock of the Nemo Console API (docs/ARCHITECTURE.md section 4) plus a static host for the frontend.
// Contract-faithful where it matters to the UI: {error:{code,message}} errors, CSRF header (X-Console: 1) on non-GET,
// Host allowlist (421), same-origin Origin (403), 1 MiB body cap (413), JSON content type (415), SSE cap of 8 (429),
// secret redaction ("[set, N chars]"), strict CSP + security headers on every response, optional bearer (MOCK_KEY).
// All data is canned or simulated. Nothing here is a verified fact about the real systems.
// Usage: node dev/mock-server.mjs [--port 18796] [--host 127.0.0.1]     env: MOCK_KEY=secret  MOCK_ROLE=viewer  MOCK_DYNAMO=up|loading|down
// Dev helpers (not part of the contract): /__dev/scenario /__dev/touch /__dev/role /__dev/fail /__dev/vllm /__shot
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { fileURLToPath } from 'node:url';
import { DEMOS } from './demos.mjs';
import { SIX, MOCKED, SIX_LABEL, sixStatus, sixHealth, sixMetrics, sixHistory, SIX_SPECS, SIX_FILES, SIX_RESTARTS, sixValidate, SIX_ACTIONS, sixRunAction, sixSeedEvents, fiveLiveEvent, GR_PLAY, GR_REFUSAL } from './mock-six.mjs';
import { SWITCHYARD_ROUTES, RELAY_ROUTES, RELAY_PLUGINS, RELAY_CONFIG, DYNAMO_CATALOG, DD_SCRIPTS, DD_SCRIPT, DD_COLS, ddRows, DD_RUNS, DD_PREVIEW_FILES, DD_CREATE_FILES } from './mock-data.mjs';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const args = process.argv.slice(2);
const argv = (n, d) => { const i = args.indexOf(`--${n}`); return i >= 0 ? args[i + 1] : d; };
const PORT = Number(argv('port', process.env.PORT ?? 18796));   // 8796 collided with another local tool
const HOST = argv('host', '127.0.0.1');
if (!['127.0.0.1', 'localhost', '::1'].includes(HOST) && !process.env.MOCK_KEY) { console.error(`refusing --host ${HOST}: a non-loopback bind needs MOCK_KEY set`); process.exit(2); }
const KEY = process.env.MOCK_KEY || '';
// --proxy http://127.0.0.1:8793 forwards /api/ to a REAL backend (screenshot/verification use), keeping the static host and /__shot helpers.
const PROXY = argv('proxy', process.env.MOCK_PROXY || '');
const now = () => Date.now() / 1000;
const sha = (s) => crypto.createHash('sha256').update(s).digest('hex');
let seed = 7; const rnd = () => { seed = (seed * 1664525 + 1013904223) % 4294967296; return seed / 4294967296; };
const pick = (a) => a[Math.floor(rnd() * a.length)];
const wait = (ms) => new Promise((r) => setTimeout(r, ms));

// ---------------------------------------------------------------- state
const state = {
  role: process.env.MOCK_ROLE === 'viewer' ? 'viewer' : 'admin',
  dynamo: process.env.MOCK_DYNAMO || 'up',       // up | loading | down | off (opt-in, stopped on purpose)
  vllmRunning: false,                             // the container that shares the Workstation card
  dynamoSince: now() - 5400,
  failNext: null,
  six: { dcgmFixed: false, guardPlayground: true },   // dev toggles for the 9.8 systems (/__dev/six)                                 // {status, n}
  settings: {
    version: '0.1.0-mock', urls: { switchyard: 'http://127.0.0.1:4010', relay: 'http://127.0.0.1:4040', dynamo: 'http://127.0.0.1:8020', dynamo_worker_metrics: 'http://127.0.0.1:8081', litellm: 'http://127.0.0.1:4000' },
    state_dir: '~/.local/share/nemo-console', poll_interval_s: 2.0, ring_seconds: 900, playground_key_env: { switchyard: 'LITELLM_API_KEY' }, tokens: { switchyard: '[set, 67 chars]' }, trust_loopback: true, restart_required: false,
  },
  audit: [],
  params: { served_model_name: 'unsloth/Qwen3.8-27B-NVFP4', gpu_memory_utilization: 0.88, max_model_len: 262144, enable_prefix_caching: true, hf_token: '[set]' },
};
const files = {
  'switchyard/routes': { content: SWITCHYARD_ROUTES, mtime: now() - 86400, history: [] },
  'relay/routes': { content: RELAY_ROUTES, mtime: now() - 172800, history: [] },
  'relay/plugins': { content: RELAY_PLUGINS, mtime: now() - 604800, history: [] },
  'relay/config': { content: RELAY_CONFIG, mtime: now() - 604800, history: [] },
};
files['switchyard/routes'].history.push({ id: 'rev_0001', ts: now() - 5 * 86400, actor: 'loopback-admin', content: SWITCHYARD_ROUTES.replace('confidence_threshold = 0.5\nrecent_turn_window = 3', 'confidence_threshold = 0.6\nrecent_turn_window = 3'), note: 'before: threshold 0.6 -> 0.5' });
files['switchyard/routes'].history.push({ id: 'rev_0002', ts: now() - 86400, actor: 'loopback-admin', content: SWITCHYARD_ROUTES.replace('picker = "efficient_first"', 'picker = "capable_first"'), note: 'before: picker efficient_first' });
for (const [k, c] of Object.entries(SIX_FILES)) files[k] = { content: c, mtime: now() - 3 * 86400, history: [] };
for (const s of DD_SCRIPTS) files[`datadesigner/${s}`] = { content: `# ${s} — Data Designer config script (mock copy)\n` + DD_SCRIPT, mtime: now() - 2 * 86400, history: [] };
const specs = {
  ...SIX_SPECS, skillevaluator: [],
  switchyard: [{ id: 'routes', label: 'routes.toml', description: 'Routing table for the Switchyard container: llm clients, targets and routes (stage routers and passthroughs).', display_path: 'inference/switchyard/routes.toml', format: 'toml', editable: true, validators: ['switchyard-server --dry-run'], authoritative: true, apply_effect: 'Recreates the switchyard container; about 5 s of dropped requests on :4010. LiteLLM on :4000 is unaffected.' }],
  relay: [
    { id: 'routes', label: 'routes.toml', description: 'Routes for the Relay native Switchyard plugin (host side).', display_path: 'clients/nemo-relay/routes.toml', format: 'toml', editable: true, validators: ['nemo-relay plugins validate (proxy)'], authoritative: false, apply_effect: 'Restarts nemo-relay-gateway.service; agents connected through :4040 reconnect after about 3 s.' },
    { id: 'plugins', label: 'plugins.toml', description: 'Plugin registry and signature policy.', display_path: '~/.config/nemo-relay/plugins.toml', format: 'toml', editable: false, why_readonly: 'It holds the signature-enforcement key policy. Changing it is a human security decision (a security policy decision), not a console action.', validators: [], authoritative: true, apply_effect: '' },
    { id: 'config', label: 'config.toml', description: 'Gateway and attestation configuration.', display_path: '~/.config/nemo-relay/config.toml', format: 'toml', editable: false, why_readonly: 'It sets the attestation mode. Edit it by hand, then restart the gateway.', validators: [], authoritative: true, apply_effect: '' },
  ],
  dynamo: [],
  datadesigner: DD_SCRIPTS.map((s) => ({
    id: s, label: s, description: 'Data Designer config script: build the dataset generator (providers, models, columns). It lives in the scripts dir the console validates and applies it to.',
    display_path: `~/Datasets/data-designer/scripts/${s}`, format: 'text', editable: true,
    validators: ['data-designer-local validate (executes the script)'], authoritative: true,
    apply_effect: 'Writes the script file. Nothing is restarted; validate and apply EXECUTE the script locally with the console user\'s privileges.',
  })),
};

// ---------------------------------------------------------------- helpers
const CSP = "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; frame-ancestors 'none'";
const SEC = { 'Content-Security-Policy': CSP, 'X-Content-Type-Options': 'nosniff', 'Referrer-Policy': 'no-referrer', 'X-Frame-Options': 'DENY', 'Permissions-Policy': 'camera=(), microphone=(), geolocation=()' };
const MIME = { '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8', '.mjs': 'text/javascript; charset=utf-8', '.css': 'text/css; charset=utf-8', '.json': 'application/json', '.svg': 'image/svg+xml' };
const json = (res, code, obj, extra = {}) => { res.writeHead(code, { 'Content-Type': 'application/json', 'Cache-Control': 'no-store', ...SEC, ...extra }); res.end(JSON.stringify(obj)); };
const err = (res, code, c, message) => json(res, code, { error: { code: c, message } });
const readBody = (req) => new Promise((ok, no) => { const c = []; let n = 0; req.on('data', (d) => { n += d.length; if (n > 1048576) { no(Object.assign(new Error('too large'), { status: 413 })); req.destroy(); } else c.push(d); }); req.on('end', () => ok(Buffer.concat(c).toString('utf8'))); req.on('error', no); });
const sseWrite = (res, data, event) => { if (!res.writableEnded) res.write(`${event ? `event: ${event}\n` : ''}data: ${typeof data === 'string' ? data : JSON.stringify(data)}\n\n`); };
let auditSeq = 0;
const audit = (system, method, action, params, status, path) => { state.audit.unshift({ id: ++auditSeq, ts: now(), actor: 'loopback', role: 'admin', method, path: path || `/api/v1/systems/${system}/${action.replace(/^action /, 'actions/').replace(/^config\.(\w+) (\w+)/, 'config/$2/$1').replace(/^params/, 'params')}`, system, params: redact(params), status }); state.audit.length = Math.min(state.audit.length, 500); };
// Content and secret-looking fields never enter the audit log.
function redact(p) { const o = {}; for (const [k, v] of Object.entries(p || {})) o[k] = /content|token|secret|password|key/i.test(k) ? '[redacted]' : v; return o; }
const looksSecret = (s) => /\bsk-[A-Za-z0-9]{12,}|Bearer\s+[A-Za-z0-9._-]{12,}|api_key\s*=\s*"(?!\$)[^"]{8,}"/.test(s);

// ---------------------------------------------------------------- system status
const dynState = () => state.dynamo;
function health(id) { if (MOCKED.includes(id)) return sixHealth(id, state.six); if (id === 'relay') return 'degraded'; if (id === 'dynamo') return dynState() === 'up' ? 'up' : dynState() === 'loading' ? 'degraded' : dynState() === 'off' ? 'off' : 'down'; return 'up'; }
function status(id) {
  const checked_at = now();
  if (MOCKED.includes(id)) return sixStatus(id, checked_at, state.six);
  if (id === 'switchyard') return { system: id, health: 'up', version: '0.3.0', summary: 'Routing on :4010; 2 of 4 tiers reachable (capable tier qwen3.8-27b-1m is dark)',
    endpoints: [{ name: 'api', url: 'http://127.0.0.1:4010/v1', role: 'api', auth: 'forwarded' }, { name: 'health', url: 'http://127.0.0.1:4010/health', role: 'health', auth: 'none' }, { name: 'metrics', url: 'http://127.0.0.1:4010/metrics', role: 'metrics', auth: 'none' }],
    components: [{ kind: 'container', name: 'ai-lab-switchyard-switchyard-1', state: 'running', image: 'switchyard-server:0.3.0', detail: 'healthy, up 19h' }, { kind: 'http', name: 'litellm upstream', state: 'running', detail: 'http://litellm:4000/v1 answers' }],
    problems: state.vllmRunning ? [] : [{ severity: 'warn', code: 'switchyard.tier.capable_dark', message: 'Capable tier target qwen3.8-27b-1m is not answering: route auto-local cannot escalate.', fix: 'scripts/use-model.sh qwen3.8-27b-1m   # NOTE: also stops the other card engine; prefer: docker compose -f inference/vllm/docker-compose.yml up -d vllm-qwen38 after Dynamo down' }],
    checked_at };
  if (id === 'relay') return { system: id, health: 'degraded', version: '0.9.2', summary: 'Gateway up on :4040; plugin loaded; 1 warning (exporter disabled)',
    endpoints: [{ name: 'gateway', url: 'http://127.0.0.1:4040', role: 'api', auth: 'none' }],
    components: [{ kind: 'systemd', name: 'nemo-relay-gateway.service', state: 'running', detail: 'active (running) since 2026-09-24 03:10' }, { kind: 'process', name: 'switchyard plugin', state: 'running', detail: 'loaded, signature verified' }, { kind: 'process', name: 'attestation', state: 'running', detail: 'mode=enforce, key present, last check 4 min ago' }],
    problems: [{ severity: 'warn', code: 'relay.exporter.disabled', message: 'The OTLP exporter is disabled, so Relay traces are not reaching Phoenix.', fix: 'nemo-relay config set exporters.otlp.enabled true && systemctl --user restart nemo-relay-gateway.service' }], checked_at };
  if (id === 'datadesigner') {
    const comps = [
      { kind: 'process', name: 'data-designer-local', state: 'running', detail: 'data-designer-local 0.9.3 (wrapper: injects its own key files, telemetry off)' },
      { kind: 'http', name: 'router http://127.0.0.1:4000', state: 'running', detail: '200 on GET /health/liveliness (generation calls go through it)' },
    ];
    if (dd.live) comps.push({ kind: 'process', name: `create ${dd.live.dataset_name}`, state: 'running', detail: `create run ${dd.live.run_id}, running for ${Math.max(0, Math.round((now() - dd.live.started) / 60))} min (one at a time; it shows in the runs list)` });
    const last = DD_RUNS[0];
    return { system: id, health: 'up', version: '0.9.3',
      summary: `${DD_SCRIPTS.length} scripts · 5 aliases · last run: ${last ? `${last.mode} ${last.dataset_name || last.script} (${last.verdict})` : 'none'}`,
      endpoints: [{ name: 'router (generation)', url: 'http://127.0.0.1:4000/v1', role: 'api', auth: 'bearer' }],
      components: comps,
      problems: [
        { severity: 'warn', code: 'dd.no_key', message: 'The LiteLLM key file the wrapper injects (~/.config/data-designer/litellm.key) does not exist; generation calls would fail. (mock: always shown)', fix: 'clients/data-designer/mint-key.sh' },
        { severity: 'info', code: 'dd.nvidia_providers', message: 'The stock nvidia-* providers need NVIDIA_API_KEY and send data to NVIDIA; the lab aliases on lab-router stay local.' },
      ],
      checked_at };
  }
  const s = dynState();
  const base = { system: 'dynamo', version: '1.5.0', endpoints: [{ name: 'frontend', url: 'http://127.0.0.1:8020/v1', role: 'api', auth: 'none' }, { name: 'worker metrics', url: 'http://ai-lab-dynamo-dynamo-worker-1:8081/metrics', role: 'metrics', auth: 'none' }], checked_at };
  if (s === 'up') return { ...base, health: 'up', summary: 'Serving unsloth/Qwen3.8-27B-NVFP4 on :8020 (worker up 1h30m)', components: [{ kind: 'container', name: 'ai-lab-dynamo-dynamo-frontend-1', state: 'running', image: 'nvcr.io/nvidia/ai-dynamo/vllm-runtime:1.5.0', detail: 'healthy' }, { kind: 'container', name: 'ai-lab-dynamo-dynamo-worker-1', state: 'running', image: 'nvcr.io/nvidia/ai-dynamo/vllm-runtime:1.5.0', detail: 'healthy, registered 1 model' }], problems: [{ severity: 'info', code: 'dynamo.shares_card', message: 'Dynamo holds the Workstation card: router alias qwen3.8-27b-1m is dark until Dynamo is down.', fix: 'cd inference/dynamo && docker compose down' }] };
  if (s === 'loading') return { ...base, health: 'degraded', summary: 'Worker loading model weights (cold start takes about 4 minutes)', components: [{ kind: 'container', name: 'ai-lab-dynamo-dynamo-frontend-1', state: 'running', detail: 'healthy, no worker registered yet' }, { kind: 'container', name: 'ai-lab-dynamo-dynamo-worker-1', state: 'running', detail: 'loading weights 62%' }], problems: [{ severity: 'info', code: 'dynamo.worker.loading', message: 'The worker is still loading. Requests return 503 until it registers.', fix: 'docker logs -f ai-lab-dynamo-dynamo-worker-1' }] };
  if (s === 'off') return { ...base, health: 'off', summary: 'off (opt-in): no Dynamo container is running', components: [{ kind: 'container', name: 'ai-lab-dynamo-dynamo-frontend-1', state: 'stopped' }, { kind: 'container', name: 'ai-lab-dynamo-dynamo-worker-1', state: 'stopped', detail: 'exited (code 1)' }], problems: [{ severity: 'info', code: 'dynamo.off', message: 'Dynamo is opt-in and off: no frontend, worker or mocker container is running or restarting.', fix: 'scripts/use-model.sh qwen3.8-27b-dynamo' }] };
  return { ...base, health: 'down', summary: 'Dynamo is not running', components: [{ kind: 'container', name: 'ai-lab-dynamo-dynamo-frontend-1', state: 'stopped', detail: 'exited 12 min ago' }, { kind: 'container', name: 'ai-lab-dynamo-dynamo-worker-1', state: 'absent' }], problems: [{ severity: 'error', code: 'dynamo.down', message: 'The frontend on :8020 does not answer and no worker container exists.', fix: 'cd inference/dynamo && docker compose up -d' }] };
}
const SYSTEMS = ['switchyard', 'relay', 'dynamo', 'skillevaluator', 'datadesigner', ...SIX];
const EVENTED = ['switchyard', 'relay', 'dynamo'];   // systems with a periodic event feed (datadesigner emits dd.run on runs only)
const LABEL = { switchyard: 'NeMo Switchyard', relay: 'NeMo Relay', dynamo: 'NVIDIA Dynamo', datadesigner: 'NVIDIA Data Designer', ...SIX_LABEL };
function topology() {
  const dh = health('dynamo');
  return { ts: now(), nodes: [
    { id: 'opencode', label: 'opencode', kind: 'client', health: 'up' }, { id: 'claude-local', label: 'claude-local', kind: 'client', health: 'up' }, { id: 'open-webui', label: 'Open WebUI', kind: 'client', health: 'up' },
    { id: 'relay', label: 'NeMo Relay', kind: 'gateway', health: 'degraded', system: 'relay' }, { id: 'switchyard', label: 'Switchyard', kind: 'router', health: 'up', system: 'switchyard' }, { id: 'litellm', label: 'LiteLLM router', kind: 'router', health: 'up' },
    { id: 'sglang', label: 'sglang-flashnext', kind: 'engine', health: 'up' }, { id: 'vllm', label: 'vllm-qwen38', kind: 'engine', health: state.vllmRunning ? 'up' : 'down' },
    { id: 'dynamo-frontend', label: 'Dynamo frontend', kind: 'engine', health: dh, system: 'dynamo' }, { id: 'dynamo-worker', label: 'Dynamo worker', kind: 'engine', health: dh === 'down' ? 'down' : dh },
  ], edges: [
    { src: 'opencode', dst: 'relay', label: 'anthropic' }, { src: 'claude-local', dst: 'relay' }, { src: 'open-webui', dst: 'litellm', label: 'direct' },
    { src: 'relay', dst: 'switchyard' }, { src: 'switchyard', dst: 'litellm' },
    { src: 'litellm', dst: 'sglang' }, { src: 'litellm', dst: 'vllm', active: state.vllmRunning }, { src: 'litellm', dst: 'dynamo-frontend', label: 'opt-in', active: dh === 'up' }, { src: 'dynamo-frontend', dst: 'dynamo-worker', active: dh !== 'down' },
  ] };
}

// ---------------------------------------------------------------- metrics / history
const T0 = now();
const upDyn = () => dynState() === 'up';
function metrics(id) {
  const ts = now(), P = (key, value, unit = '', labels = {}) => ({ key, value, unit, labels });
  const wob = (b, a) => b + Math.sin(ts / 7) * a + rnd() * a * 0.4;
  if (MOCKED.includes(id)) return { system: id, ts, available: true, points: sixMetrics(id, ts, wob) };
  if (id === 'switchyard') return { system: id, ts, available: true, points: [P('sy.requests_total', 1200 + (ts - T0) * 0.4), P('sy.errors_total', 3), P('sy.in_flight', Math.round(wob(2, 2))), P('sy.tokens', 3.1e6, 'tok', { kind: 'prompt' }), P('sy.tokens', 41200, 'tok', { kind: 'completion' }), P('sy.tokens', 9800, 'tok', { kind: 'reasoning' }), P('sy.tokens', 120000, 'tok', { kind: 'cached' }), P('sy.route.calls', 812, '', { route: 'auto-local' }), P('sy.model.calls', 700, '', { model: 'nemotron-lightning' })] };
  if (id === 'relay') return { system: id, ts, available: true, points: [P('relay.gateway_up', 1), P('relay.plugin_loaded', 1), P('relay.atof_files', 214), P('relay.atof_bytes', 48e6, 'B'), P('relay.atif_trajectories', 37), P('relay.exporters_enabled', 0)] };
  if (id === 'datadesigner') {
    const by = {};
    for (const r of DD_RUNS) by[r.verdict] = (by[r.verdict] || 0) + 1;
    if (dd.live) by.running = (by.running || 0) + 1;
    const points = [P('dd.scripts', DD_SCRIPTS.length), P('dd.runs', DD_RUNS.length + (dd.live ? 1 : 0)), P('dd.live_running', dd.live ? 1 : 0), P('dd.last_records', DD_RUNS[0]?.num_records ?? 0)];
    for (const [v, n] of Object.entries(by)) points.push(P(`dd.runs_by_verdict{verdict=${v}}`, n, '1'));
    return { system: id, ts, available: true, points };
  }
  if (!upDyn()) return { system: id, ts, available: false, points: [], error: dynState() === 'loading' ? 'Worker is loading; metrics start when it registers.' : 'Dynamo is not running.' };
  return { system: id, ts, available: true, points: [P('dyn.up', 1), P('dyn.models', 1), P('dyn.requests_total', 640 + (ts - T0) * 0.2), P('dyn.in_flight', Math.round(wob(2, 2))), P('dyn.ttft_ms', wob(230, 30), 'ms', { q: 'p50' }), P('dyn.ttft_ms', wob(520, 80), 'ms', { q: 'p95' }), P('dyn.itl_ms', wob(19, 2), 'ms', { q: 'p50' }), P('dyn.itl_ms', wob(34, 5), 'ms', { q: 'p95' }), P('dyn.kv_cache_usage', wob(0.22, 0.05), 'ratio'), P('dyn.worker_up', 1)] };
}
function history(id, key, window) {
  if (MOCKED.includes(id) && sixHistory(key, 0, rnd) === undefined) return { key, points: [] };
  if (id === 'dynamo' && !upDyn()) return { key, points: [] };
  const n = { '15m': 30, '1h': 60, '24h': 96 }[window] || 30, step = { '15m': 30, '1h': 60, '24h': 900 }[window] || 30, t1 = Math.floor(now());
  const pts = [];
  for (let i = n - 1; i >= 0; i--) {
    const ts = t1 - i * step, k = (t1 - ts) / step;
    let v;
    if (key === 'sy.requests_total') v = 900 + (n - k) * step * 0.35 + Math.sin(ts / 200) * 10 * (n - k) * 0.2;
    else if (/dyn\.ttft_ms\{q=p95/.test(key)) v = 500 + Math.sin(ts / 90) * 80 + rnd() * 40;
    else if (/dyn\.ttft_ms/.test(key)) v = 230 + Math.sin(ts / 110) * 30 + rnd() * 15;
    else if (/dyn\.itl_ms\{q=p95/.test(key)) v = 34 + Math.sin(ts / 70) * 5 + rnd() * 3;
    else if (/dyn\.itl_ms/.test(key)) v = 19 + Math.sin(ts / 80) * 2 + rnd();
    else if (key === 'dyn.in_flight') v = Math.max(0, Math.round(2 + Math.sin(ts / 45) * 2 + rnd() * 2));
    else if (key === 'dyn.kv_cache_usage') v = 0.2 + Math.abs(Math.sin(ts / 300)) * 0.25 + rnd() * 0.03;
    else if (MOCKED.includes(id)) v = sixHistory(key, ts, rnd);
    else v = rnd() * 10;
    pts.push([ts, +v.toFixed(3)]);
  }
  return { key, points: pts };
}
function routingSummary(window) {
  const f = { '15m': 0.25, '1h': 1, '24h': 12 }[window] || 1;
  const r = (n) => Math.round(n * f);
  const by_route = { 'auto-local': r(412), 'direct-lightning': r(96), 'direct-27b': r(21), 'auto-flashnext': r(8) };
  const requests = Object.values(by_route).reduce((a, b) => a + b, 0);
  return { window, requests, errors: Math.round(requests * 0.007), by_route, by_tier: { efficient: r(455), capable: r(67), classifier: r(15) }, by_model: { 'nemotron-lightning': r(520), 'qwen3.8-27b-1m': r(21), 'qwen3.8-flash-next': r(8) },
    tokens: { prompt: r(1973335), completion: r(18571), reasoning: r(5467), cached: r(120400) } };
}

// ---------------------------------------------------------------- events + SSE
const ring = { switchyard: [], relay: [], dynamo: [], datadesigner: [], skillevaluator: [] };
for (const [id, evs] of Object.entries(sixSeedEvents(now()))) ring[id] = evs;
const streams = new Set();
function makeEvent(system) {
  const ts = now();
  if (system === 'switchyard') {
    const route = pick(['auto-local', 'auto-local', 'auto-local', 'direct-lightning', 'direct-27b']), tier = route === 'auto-local' ? pick(['efficient', 'efficient', 'efficient', 'capable']) : route === 'direct-27b' ? 'capable' : 'efficient';
    const model = tier === 'capable' ? 'qwen3.8-27b-1m' : 'nemotron-lightning', pt = Math.round(1500 + rnd() * 40000), ct = Math.round(40 + rnd() * 500), lat = Math.round(400 + rnd() * 4200);
    return { ts, system, kind: 'routing.decision', summary: `${route} -> ${model} (${tier}) ${pt}+${ct} tok ${lat} ms`, data: { route, tier, model, prompt_tokens: pt, completion_tokens: ct, latency_ms: lat } };
  }
  if (system === 'relay') {
    const kind = pick(['relay.atof', 'relay.atof', 'relay.atif']);
    return { ts, system, kind, summary: kind === 'relay.atif' ? `trajectory closed: ${Math.round(4 + rnd() * 30)} steps` : `${pick(['llm.request', 'llm.response', 'tool.call'])} recorded (${Math.round(300 + rnd() * 9000)} B)`, data: { event_type: kind, agent: pick(['opencode', 'claude-local']), bytes: Math.round(300 + rnd() * 9000) } };
  }
  return { ts, system, kind: 'dynamo.log', summary: pick(['request 200 ttft 231 ms', 'kv cache 21% used', 'router: worker 0 selected', 'prefix cache hit 8192 tokens']), data: { level: 'info' } };
}
for (const s of EVENTED) for (let i = 25; i > 0; i--) { const e = makeEvent(s); e.ts -= i * 7; ring[s].unshift(e); }
// Data Designer has no periodic feed: one dd.run event per stored run (metadata only).
for (const r of DD_RUNS) ring.datadesigner.unshift(ddEvent(r));
setInterval(() => {
  if (rnd() < 0.3) { const e = fiveLiveEvent(now(), pick, rnd); ring.guardrails.unshift(e); ring.guardrails.length = Math.min(ring.guardrails.length, 200); for (const c of streams) if (c.system === 'guardrails') sseWrite(c.res, e); }
  for (const s of EVENTED) {
    if (s === 'dynamo' && !upDyn()) continue;
    if (rnd() > (s === 'switchyard' ? 0.7 : 0.35)) continue;
    const e = makeEvent(s); ring[s].unshift(e); ring[s].length = Math.min(ring[s].length, 200);
    for (const c of streams) if (c.system === s) sseWrite(c.res, e);
  }
}, 2200).unref();
setInterval(() => { for (const c of streams) if (!c.res.writableEnded) c.res.write(': ping\n\n'); }, 15000).unref();

// ── Data Designer run events / live-run simulation ─────────────────────────────────────────
function ddEvent(run) {
  return { ts: run.ts ?? now(), system: 'datadesigner', kind: 'dd.run',
    summary: `${run.mode} ${run.dataset_name || run.script} → ${run.verdict}${run.num_records != null ? ` (${run.num_records} records)` : ''}`,
    data: { run_id: run.run_id, script: run.script, mode: run.mode, verdict: run.verdict, num_records: run.num_records,
      dataset_name: run.dataset_name, destination: run.destination, format: run.format, duration_ms: run.duration_ms } };
}
function ddEmit(run) {
  const ev = ddEvent(run);
  ring.datadesigner.unshift(ev); ring.datadesigner.length = Math.min(ring.datadesigner.length, 200);
  for (const c of streams) if (c.system === 'datadesigner') sseWrite(c.res, ev);
  return ev;
}
const ddRunId = (name) => {
  const d = new Date(), p = (n, w = 2) => String(n).padStart(w, '0');
  return `${d.getUTCFullYear()}${p(d.getUTCMonth() + 1)}${p(d.getUTCDate())}T${p(d.getUTCHours())}${p(d.getUTCMinutes())}${p(d.getUTCSeconds())}-${String(d.getUTCMilliseconds()).padStart(3, '0')}-${name}`;
};
function ddFinish(runId) {
  const live = dd.live;
  if (!live || live.run_id !== runId) return;
  dd.live = null;
  if (dd.timer) { clearTimeout(dd.timer); dd.timer = null; }
  const run = { run_id: runId, script: live.script, mode: 'create', verdict: 'succeeded', num_records: live.num_records,
    dataset_name: live.dataset_name, destination: live.destination, format: live.format,
    duration_ms: Math.max(1000, Math.round((now() - live.started) * 1000)), ts: now(),
    columns: DD_COLS, rows: ddRows(Math.min(live.num_records, 500)), files: DD_CREATE_FILES(live) };
  DD_RUNS.unshift(run); DD_RUNS.length = Math.min(DD_RUNS.length, 20);
  ddEmit(run);
}
const dd = { live: null, timer: null };

// ---------------------------------------------------------------- config engine
function validate(system, file, content) {
  const spec = specs[system].find((s) => s.id === file);
  if (system === 'datadesigner') {
    // The CLI is the real validator and it EXECUTES the script (load_config_builder). The mock only checks the shape.
    const ok = /def\s+load_config_builder/.test(content);
    return { ok, validator: spec.validators[0] || 'data-designer-local validate', authoritative: true,
      diagnostics: ok ? [] : [{ severity: 'error', message: 'the script does not define load_config_builder(); the CLI could not build a config', line: 1 }],
      note: 'Validation executes this script locally (it calls load_config_builder) with the console user\'s privileges.' };
  }
  if (MOCKED.includes(system)) return sixValidate(system, file, content);
  const diags = [], lines = content.split('\n'), targets = new Set();
  lines.forEach((l) => { const m = /^\[targets\.([\w-]+)\]/.exec(l); if (m) targets.add(m[1]); });
  lines.forEach((l, i) => {
    const t = l.trim();
    if (/^\[[^\]]*$/.test(t)) diags.push({ severity: 'error', message: 'TOML syntax error: unterminated table header', line: i + 1 });
    const m = /^(capable_target|efficient_target|target)\s*=\s*"([^"]*)"/.exec(t);
    if (m && !targets.has(m[2])) diags.push({ severity: 'error', message: `route references unknown target ${m[2]}`, line: i + 1 });
    if (/^picker\s*=/.test(t) && !/efficient_first|capable_first/.test(t)) diags.push({ severity: 'warn', message: 'picker is not one of efficient_first, capable_first', line: i + 1 });
  });
  if (!/^schema_version\s*=/m.test(content)) diags.push({ severity: 'error', message: 'schema_version is required', line: 1 });
  const ok = !diags.some((d) => d.severity === 'error');
  return { ok, validator: spec.validators[0] || 'toml', authoritative: spec.authoritative, diagnostics: diags, note: spec.authoritative ? null : 'The proxy validator does not load the Relay plugin schema; only starting the gateway proves the file loads.' };
}
async function applyFile(system, file, content, base, restart, note, postCheck) {
  const f = files[`${system}/${file}`];
  const backup = { id: `rev_${String(f.history.length + 1).padStart(4, '0')}`, ts: now(), actor: 'loopback-admin', content: f.content, note: note || 'before apply' };
  f.history.push(backup);
  const prev = f.content;
  f.content = content; f.mtime = now();
  const unhealthy = /unhealthy|broken-target/.test(content);
  await wait(restart ? 900 : 150);
  if (restart && unhealthy) {
    f.content = prev; f.mtime = now();
    return { ok: false, new_sha256: sha(prev), revision_id: backup.id, restarted: true, post_check: 'unhealthy', detail: 'health check failed 3/3: GET :4010/health -> connection refused\nrestoring revision ' + backup.id + '\nservice restarted on the previous file; health check passed', rolled_back: true };
  }
  return { ok: true, new_sha256: sha(content), revision_id: backup.id, restarted: !!restart, post_check: postCheck || (restart ? 'healthy' : 'skipped'), detail: restart ? 'container recreated; /health 200 after 2.1 s' : 'file written; not restarted', rolled_back: false };
}

// ---------------------------------------------------------------- actions
const ACTIONS = {
  ...SIX_ACTIONS,
  switchyard: [
    { id: 'restart', label: 'Restart Switchyard', description: 'Recreates the switchyard container. About 5 s of dropped requests on :4010; in-memory /v1/stats are lost.', danger: 'disruptive', params: [] },
    { id: 'reset_stats', label: 'Reset stats', description: 'Zeroes the in-memory /v1/stats counters. The durable routing log is not touched.', danger: 'destructive', params: [] },
    { id: 'dry_run_config', label: 'Dry-run current config', description: 'Runs switchyard-server --dry-run against the file on disk. Changes nothing.', danger: 'safe', params: [] },
  ],
  relay: [
    { id: 'gateway_restart', label: 'Restart gateway', description: 'Restarts nemo-relay-gateway.service; connected agents reconnect after about 3 s.', danger: 'disruptive', params: [] },
    { id: 'gateway_status', label: 'Gateway status', description: 'systemctl --user status nemo-relay-gateway.service', danger: 'safe', params: [] },
    { id: 'doctor_offline', label: 'Doctor (offline)', description: 'nemo-relay doctor --offline: local checks only, no network.', danger: 'safe', params: [] },
    { id: 'agents_list', label: 'List agents', description: 'nemo-relay agents --json', danger: 'safe', params: [] },
    { id: 'plugins_validate', label: 'Validate plugins', description: 'nemo-relay plugins validate against ~/.config/nemo-relay/plugins.toml', danger: 'safe', params: [] },
  ],
  dynamo: [
    { id: 'preflight', label: 'Preflight', description: 'Reports what shares the Workstation card and which router aliases go dark. Read-only.', danger: 'safe', params: [] },
    { id: 'up', label: 'Up', description: 'docker compose up -d for the Dynamo frontend and worker (cold start about 4 minutes).', danger: 'disruptive', params: [{ key: 'stop_sharing_engine', label: 'Stop the engine sharing the card', help: 'Stops exactly ai-lab-vllm-qwen38-vllm-1 and nothing else.', type: 'bool', default: false, requires_restart: false }] },
    { id: 'down', label: 'Down', description: 'docker compose down for Dynamo. Does not touch other cards.', danger: 'disruptive', params: [] },
    { id: 'restart', label: 'Restart', description: 'Recreates the Dynamo worker and frontend; the model reloads (about 4 minutes).', danger: 'disruptive', params: [] },
    { id: 'up_mock', label: 'Up (mock worker)', description: 'Starts the CPU-only mocker profile. Safe: uses no GPU.', danger: 'safe', params: [] },
  ],
  datadesigner: [
    { id: 'preview', label: 'Preview records', danger: 'disruptive',
      description: 'Runs data-designer-local preview on one script (it EXECUTES the script locally) and keeps the sample dataset, report and record browser in the run dir. The console also converts the parquet to records.jsonl (first 2,000 rows kept).',
      params: [
        { key: 'script', label: 'Script', type: 'enum', choices: DD_SCRIPTS, default: DD_SCRIPTS[0], requires_restart: false, help: 'A config script in the scripts dir.' },
        { key: 'num_records', label: 'Number of records', type: 'int', default: 5, min: 1, max: 100, requires_restart: false, help: 'Sample size. Spends router tokens.' },
      ] },
    { id: 'create', label: 'Create a dataset', danger: 'disruptive',
      description: 'Runs data-designer-local create in the background (it EXECUTES the script locally): spends router tokens and takes minutes. One create at a time. The dataset lands in ~/Datasets/<destination>; the console also keeps records.jsonl (first 2,000 rows) for the runs view.',
      params: [
        { key: 'script', label: 'Script', type: 'enum', choices: DD_SCRIPTS, default: DD_SCRIPTS[0], requires_restart: false, help: 'A config script in the scripts dir.' },
        { key: 'num_records', label: 'Number of records', type: 'int', default: 100, min: 1, max: 100000, requires_restart: false, help: 'Dataset size. Minutes, and router tokens, scale with it.' },
        { key: 'dataset_name', label: 'Dataset name', type: 'string', default: null, requires_restart: false, help: 'Letters, digits, dashes and underscores. Empty = derived from the script name.' },
        { key: 'destination_subdir', label: 'Destination subdirectory', type: 'string', default: null, requires_restart: false, help: 'Relative path under ~/Datasets (no ".."). Empty = the dataset name.' },
        { key: 'format', label: 'User copy format', type: 'enum', choices: ['parquet', 'csv', 'jsonl', 'json'], default: 'parquet', requires_restart: false, help: 'The user copy written to ~/Datasets; parquet files are always kept in the run dir.' },
      ] },
    { id: 'report', label: 'Show a run report', danger: 'safe',
      description: 'A bounded text summary of one stored run (empty = the newest): mode, verdict, counts and up to five sample records.',
      params: [{ key: 'run_id', label: 'Run', type: 'string', default: null, requires_restart: false, help: 'A run id from the runs list; empty = the newest run.' }] },
    { id: 'check_models', label: 'Check models', danger: 'disruptive', params: [],
      description: 'data-designer-local check-models: verifies every model and MCP tool the scripts reference is reachable through the router.' },
    { id: 'show_config', label: 'Show provider config', danger: 'safe', params: [],
      description: 'Read-only dump of ~/.data-designer/model_providers.yaml and model_configs.yaml: providers, aliases and inference parameters. Key fields hold env NAMES, never values.' },
  ],
};
function goLoading() { state.dynamo = 'loading'; setTimeout(() => { if (state.dynamo === 'loading') { state.dynamo = 'up'; state.dynamoSince = now(); } }, 12000).unref(); }
function runAction(system, id, params) {
  const t0 = Date.now(), R = (ok, output, problems = []) => ({ ok, output, duration_ms: Date.now() - t0 + Math.round(rnd() * 400), problems });
  if (MOCKED.includes(system)) return sixRunAction(system, id, params, R);
  if (system === 'datadesigner') {
    const script = DD_SCRIPTS.includes(params.script) ? params.script : DD_SCRIPTS[0];
    if (id === 'preview') {
      const n = Math.max(1, Math.min(100, Number(params.num_records) || 5));
      const run = { run_id: ddRunId(`preview-${script.replace(/\.py$/, '')}`), script, mode: 'preview', verdict: 'succeeded',
        num_records: n, dataset_name: null, destination: null, format: 'parquet', duration_ms: 4000 + Math.round(rnd() * 6000), ts: now(),
        columns: DD_COLS, rows: ddRows(n), files: DD_PREVIEW_FILES(n) };
      DD_RUNS.unshift(run); DD_RUNS.length = Math.min(DD_RUNS.length, 20);
      ddEmit(run);
      return R(true, `preview: wrote ${n} records to <run_dir>/preview_results_20261023_091512/dataset.parquet\nreport: <run_dir>/preview_results_20261023_091512/report.html\nsample records browser: <run_dir>/preview_results_20261023_091512/sample_records/sample_records_browser.html\nconverted dataset.parquet -> records.jsonl (first 2,000 lines kept)`);
    }
    if (id === 'create') {
      if (dd.live) return R(false, `refused: create run ${dd.live.run_id} is already running (one at a time)`, [{ severity: 'error', code: 'dd.create.busy', message: 'One create run at a time. Wait for the current run to end (it shows in Runs).', fix: null }]);
      const n = Math.max(1, Math.min(100000, Number(params.num_records) || 100));
      const stem = script.replace(/\.py$/, '');
      const name = String(params.dataset_name || '').replace(/[^\w-]+/g, '_') || `dataset_${stem}`;
      const dest = String(params.destination_subdir || '').replace(/\.\./g, '_').replace(/[^\w/.-]+/g, '_').replace(/^\/+|\/+$/g, '') || name;
      const fmt = ['parquet', 'csv', 'jsonl', 'json'].includes(params.format) ? params.format : 'parquet';
      const runId = ddRunId(`create-${stem}`);
      dd.live = { run_id: runId, started: now(), script, mode: 'create', num_records: n, dataset_name: name, destination: dest, format: fmt };
      ddEmit({ run_id: runId, script, mode: 'create', verdict: 'running', num_records: n, dataset_name: name, destination: dest, format: fmt, duration_ms: null, ts: now() });
      dd.timer = setTimeout(() => ddFinish(runId), 12000);
      dd.timer.unref();
      return R(true, `create started in the background: run ${runId}\nscript: ${script}  records: ${n}  dataset: ${name}  destination: ~/Datasets/${dest}  format: ${fmt}\nit shows as running in Runs and fills in when it ends; one create at a time`);
    }
    if (id === 'report') {
      const rid = String(params.run_id || '');
      const run = (rid ? DD_RUNS.find((r) => r.run_id === rid) : DD_RUNS[0]);
      if (!run) return R(false, `unknown run ${rid || '(newest)'}`);
      const lines = [`run: ${run.run_id}`, `mode: ${run.mode}  verdict: ${run.verdict}  records: ${run.num_records ?? 0}`,
        run.error ? `error: ${run.error}` : `dataset: ${run.dataset_name ?? '–'}  destination: ${run.destination ?? '–'}  format: ${run.format}`, '',
        '| # | subject | priority |', '|---|---|---|'];
      for (const [i, r] of (run.rows || []).slice(0, 5).entries()) lines.push(`| ${i + 1} | ${String(r.subject).slice(0, 120)} | ${r.priority} |`);
      return R(true, lines.join('\n'));
    }
    if (id === 'check_models') return R(true, 'lab-text glm-5.3-flash-next-local: reachable (lab-router)\nlab-lightning nemotron-lightning: reachable (lab-router)\nlab-embed nomic-embed-text: reachable (lab-router)\nnvidia/*: not checked (NVIDIA_API_KEY not set; those providers send data to NVIDIA)');
    if (id === 'show_config') return R(true, 'providers (~/.data-designer/model_providers.yaml)\n  lab-router    http://127.0.0.1:4000/v1   openai   api_key=LITELLM_DD_KEY (env name)\n\naliases (~/.data-designer/model_configs.yaml)\n  lab-text       glm-5.3-flash-next-local   lab-router   temp 0.2  top_p 0.9\n  lab-lightning  nemotron-lightning         lab-router   temp 0.2\n  lab-embed      nomic-embed-text           lab-router   -\n  nvidia-*       ...                        nvidia       api_key=NVIDIA_API_KEY (sends data to NVIDIA)');
  }
  if (system === 'switchyard') {
    if (id === 'restart') return R(true, 'container ai-lab-switchyard-switchyard-1 recreated\n/health 200 after 2.4 s');
    if (id === 'reset_stats') return R(true, 'POST /v1/stats/reset -> 200\ncounters zeroed');
    return R(true, `configuration OK (dry-run)\ntargets: 3  routes: 5  llm_clients: 1`);
  }
  if (system === 'relay') {
    if (id === 'gateway_restart') return R(true, 'Stopping nemo-relay-gateway.service\nStarted nemo-relay-gateway.service\nlistening on 127.0.0.1:4040');
    if (id === 'gateway_status') return R(true, '● nemo-relay-gateway.service - NeMo Relay gateway\n     Active: active (running) since Thu 2026-09-24 03:10:02 BST; 1h 31min ago\n   Main PID: 48211 (nemo-relay)');
    if (id === 'doctor_offline') return R(true, 'nemo-relay 0.9.2\n[ok]   config parses\n[ok]   plugin dir readable\n[ok]   attestation key present (mode=enforce)\n[warn] exporters.otlp.enabled = false\n[ok]   atof dir writable (214 files, 48 MB)\n5 checks, 1 warning', [{ severity: 'warn', code: 'relay.exporter.disabled', message: 'OTLP exporter disabled', fix: 'nemo-relay config set exporters.otlp.enabled true' }]);
    if (id === 'agents_list') return R(true, JSON.stringify([{ name: 'opencode', connected: true, last_seen: '12s ago', sessions: 2 }, { name: 'claude-local', connected: true, last_seen: '3s ago', sessions: 1 }], null, 2));
    return R(true, 'plugins.toml: 1 plugin\n  switchyard: signature OK (key from NEMO_RELAY_PLUGIN_PUBKEY)');
  }
  if (id === 'preflight') {
    const shares = `ai-lab-vllm-qwen38-vllm-1 (${state.vllmRunning ? 'running' : 'stopped'})`;
    return R(true, `dynamo_state: ${state.dynamo}\nshares_card: ${shares}\nstoppable: ${state.vllmRunning ? 'ai-lab-vllm-qwen38-vllm-1 (running)' : 'none'}\nrouter_aliases_dark: qwen3.8-27b-1m, switchyard capable tier (auto-local cannot escalate)\nother_cards: untouched (sglang-flashnext keeps running on the Max-Q card)\nnote: down/up of Dynamo does not stop any container except an explicit stop_sharing_engine`,
      state.vllmRunning ? [{ severity: 'warn', code: 'dynamo.preflight.sharing_running', message: 'ai-lab-vllm-qwen38-vllm-1 is running on the Workstation card; Up is refused unless it is stopped.', fix: 'docker stop ai-lab-vllm-qwen38-vllm-1   # or tick "stop the engine sharing the card" on Up' }] : []);
  }
  if (id === 'up' || id === 'restart') {
    if (id === 'up' && !['down', 'off'].includes(state.dynamo)) return R(true, 'already running; nothing to do');
    if (id === 'up' && state.vllmRunning && !params.stop_sharing_engine) return R(false, 'refused: ai-lab-vllm-qwen38-vllm-1 shares the card and is running', [{ severity: 'error', code: 'dynamo.up.refused_sharing', message: 'Up refused: the engine sharing the Workstation card is running.', fix: 'Re-run Up with stop_sharing_engine=true, or: docker stop ai-lab-vllm-qwen38-vllm-1' }]);
    if (id === 'up' && state.vllmRunning) { state.vllmRunning = false; }
    goLoading();
    return R(true, `${id === 'up' && params.stop_sharing_engine ? 'stopped ai-lab-vllm-qwen38-vllm-1\n' : ''}docker compose up -d\ncontainer ai-lab-dynamo-dynamo-frontend-1 started\ncontainer ai-lab-dynamo-dynamo-worker-1 started\nworker loading (about 4 minutes cold)`);
  }
  if (id === 'down') { state.dynamo = 'down'; return R(true, 'docker compose down\ncontainer ai-lab-dynamo-dynamo-worker-1 removed\ncontainer ai-lab-dynamo-dynamo-frontend-1 removed'); }
  return R(true, 'mocker profile started (CPU only)');
}

// ---------------------------------------------------------------- params
const FLAGS = Object.fromEntries(DYNAMO_CATALOG.map((p) => [p.key, p]));
function renderOverride(values) {
  const args = [], env = [];
  for (const [k, v] of Object.entries(values)) {
    const p = FLAGS[k]; if (!p || v === undefined) continue;
    if (p.flag && p.flag.startsWith('env:')) { env.push(`      ${p.flag.slice(4)}: "\${${p.flag.slice(4)}}"   # value comes from the server environment`); continue; }
    if (p.type === 'bool') { if (v) args.push(p.flag); }
    else if (p.type === 'string_list') args.push(...[].concat(v));
    else if (p.flag) args.push(p.flag, String(v));
  }
  return `# generated by nemo-console (mock); the repo compose file is not modified\nservices:\n  dynamo-worker:\n    command: [${args.map((a) => JSON.stringify(a)).join(', ')}]\n${env.length ? `    environment:\n${env.join('\n')}\n` : ''}`;
}
function validateParams(values) {
  for (const [k, v] of Object.entries(values)) {
    const p = FLAGS[k];
    if (!p) return `unknown parameter ${k}`;
    if (v === '[set]') return `parameter ${k}: the redacted placeholder must not be sent back`;
    if ((p.type === 'int' || p.type === 'float') && (typeof v !== 'number' || (p.min != null && v < p.min) || (p.max != null && v > p.max))) return `parameter ${k}: out of range`;
    if (p.type === 'enum' && !p.choices.includes(v)) return `parameter ${k}: not an allowed choice`;
  }
  return null;
}

// ---------------------------------------------------------------- playground
const REPLY = 'Routing keeps the cheap model on the easy turns and hands the hard ones to the capable tier. Here is the short version: classify the request, pick the tier, and forward it through LiteLLM.';
const PLAY = {
  get guardrails() { return state.six.guardPlayground ? GR_PLAY : null; },
  switchyard: { wire: 'openai_chat', models: ['auto-local', 'auto-flashnext', 'direct-lightning', 'direct-27b'], default_model: 'auto-local', note: 'Sent to Switchyard on :4010 with the console credential resolved server-side.' },
  relay: { wire: 'anthropic_messages', models: ['auto-local'], default_model: 'auto-local', note: 'Sent through the Relay gateway on :4040 (Anthropic Messages wire).' },
  dynamo: { wire: 'openai_chat', models: ['unsloth/Qwen3.8-27B-NVFP4'], default_model: 'unsloth/Qwen3.8-27B-NVFP4', note: 'Sent to the Dynamo frontend on :8020.' },
};
async function playgroundChat(req, res, system, body) {
  const spec = PLAY[system];
  if (!spec) return err(res, 404, 'not_found', 'no playground for this system');
  if (!body || !spec.models.includes(body.model) || !Array.isArray(body.messages) || !body.messages.length) return err(res, 422, 'invalid_request', 'model must be one of the offered models and messages must be non-empty');
  if (body.max_tokens != null && (!Number.isInteger(body.max_tokens) || body.max_tokens < 1 || body.max_tokens > 4096)) return err(res, 422, 'invalid_request', 'max_tokens must be 1..4096');
  if (system === 'dynamo' && !upDyn()) return err(res, 503, 'upstream_unavailable', dynState() === 'loading' ? 'Dynamo worker is still loading' : 'Dynamo is not running');
  const extra = system === 'switchyard' ? { 'X-Served-By-Route': body.model, 'X-Served-By-Tier': body.model === 'direct-27b' ? 'capable' : pick(['efficient', 'efficient', 'capable']) } : {};
  res.writeHead(200, { 'Content-Type': 'text/event-stream', 'Cache-Control': 'no-store', Connection: 'keep-alive', ...SEC, ...extra });
  const blocked = system === 'guardrails' && /ignore previous|password|secret/i.test(String(body.messages[body.messages.length - 1]?.content));
  const words = (blocked ? GR_REFUSAL : REPLY).split(' '), n = Math.min(words.length, body.max_tokens || 256);
  let closed = false; req.on('close', () => { closed = true; });
  if (spec.wire === 'anthropic_messages') sseWrite(res, { type: 'message_start', message: { usage: { input_tokens: 812 } } }, 'message_start');
  for (let i = 0; i < n && !closed; i++) {
    const text = (i ? ' ' : '') + words[i];
    if (spec.wire === 'anthropic_messages') sseWrite(res, { type: 'content_block_delta', delta: { type: 'text_delta', text } }, 'content_block_delta');
    else sseWrite(res, { choices: [{ delta: { content: text } }] });
    await wait(45);
  }
  if (spec.wire === 'anthropic_messages') { sseWrite(res, { type: 'message_delta', usage: { output_tokens: n } }, 'message_delta'); sseWrite(res, { type: 'message_stop' }, 'message_stop'); }
  else { sseWrite(res, { choices: [{ delta: {}, finish_reason: 'stop' }], usage: { prompt_tokens: 812, completion_tokens: n } }); sseWrite(res, '[DONE]'); }
  res.end();
}

// ---------------------------------------------------------------- API router
async function api(req, res, url) {
  const M = req.method, p = url.pathname.replace(/^\/api\/v1/, '') || '/';
  if (state.failNext && state.failNext.n > 0) { state.failNext.n--; const st = state.failNext.status; return err(res, st, { 401: 'unauthorized', 403: 'forbidden', 409: 'conflict', 413: 'too_large', 415: 'unsupported_media_type', 421: 'bad_host', 429: 'rate_limited', 502: 'bad_gateway', 503: 'unavailable' }[st] || 'error', `injected ${st} (mock /__dev/fail)`); }
  // security: Host allowlist, Origin, CSRF header, bearer, content type
  const host = (req.headers.host || '').replace(/:\d+$/, '');
  if (!['127.0.0.1', 'localhost', '[::1]'].includes(host)) return err(res, 421, 'bad_host', 'Host not allowed');
  const unsafe = !['GET', 'HEAD'].includes(M);
  if (unsafe) {
    if (req.headers.origin && new URL(req.headers.origin).host !== req.headers.host) return err(res, 403, 'bad_origin', 'cross-origin request refused');
    if (req.headers['x-console'] !== '1') return err(res, 403, 'csrf', 'missing X-Console: 1 header');
    if (req.headers['content-type'] && !/^application\/json/.test(req.headers['content-type'])) return err(res, 415, 'unsupported_media_type', 'application/json required');
  }
  if (KEY && p !== '/health' && req.headers.authorization !== `Bearer ${KEY}`) return err(res, 401, 'unauthorized', 'bearer token required');
  const admin = state.role === 'admin';
  if (unsafe && !admin) return err(res, 403, 'forbidden', 'this action requires the admin role');
  let body = {};
  if (unsafe) {
    let raw = ''; try { raw = await readBody(req); } catch (e) { return err(res, 413, 'too_large', 'request body exceeds 1 MiB'); }
    if (raw) { try { body = JSON.parse(raw); } catch { return err(res, 422, 'invalid_json', 'body is not valid JSON'); } }
  }
  let m;
  if (p === '/health' && M === 'GET') return json(res, 200, { status: 'ok', version: '0.1.0-mock', trust: KEY ? 'bearer' : 'loopback-admin', role: state.role, systems: Object.fromEntries(SYSTEMS.map((s) => [s, health(s)])) });
  if (!admin && p === '/audit') return err(res, 403, 'forbidden', 'the audit log is admin-only');
  if (p === '/overview') return json(res, 200, { systems: SYSTEMS.map(status), topology: topology(), ts: now() });
  if (p === '/topology') return json(res, 200, topology());
  if (p === '/systems') return json(res, 200, SYSTEMS.map((id) => ({ id, label: LABEL[id], health: health(id) })));
  if (p === '/routing/summary') return json(res, 200, routingSummary(url.searchParams.get('window') || '1h'));
  if (p === '/audit') { const sys = url.searchParams.get('system'); return json(res, 200, state.audit.filter((a) => !sys || a.system === sys).slice(0, Number(url.searchParams.get('limit') || 100))); }
  if (p === '/settings' && M === 'GET') return json(res, 200, state.settings);
  if (p === '/settings' && M === 'PUT') {
    for (const [k, v] of Object.entries(body.urls || {})) { if (!/^http:\/\/(127\.0\.0\.1|localhost|ai-lab-[\w.-]+|litellm)(:\d+)?(\/|$)/.test(v)) return err(res, 422, 'invalid_url', `${k}: only loopback or ai-lab bus URLs are allowed`); state.settings.urls[k] = v; }
    if (body.poll_interval_s !== undefined) { if (!(body.poll_interval_s >= 0.5)) return err(res, 422, 'invalid_interval', 'poll_interval_s must be at least 0.5'); state.settings.poll_interval_s = body.poll_interval_s; state.settings.restart_required = true; }
    for (const [k, v] of Object.entries(body.playground_key_env || {})) { if (!/^[A-Z][A-Z0-9_]*$/.test(v)) return err(res, 422, 'invalid_env_name', `${k}: must be an environment variable NAME`); state.settings.playground_key_env[k] = v; }
    audit('core', 'PUT', 'settings', body, 200, '/api/v1/settings'); return json(res, 200, state.settings);
  }
  if ((m = p.match(/^\/systems\/(\w+)(\/.*)?$/))) {
    const id = m[1], sub = m[2] || '/status';
    if (!SYSTEMS.includes(id)) return err(res, 404, 'not_found', `unknown system ${id}`);
    if (sub === '/status') return json(res, 200, status(id));
    if (sub === '/metrics') return json(res, 200, metrics(id));
    if (sub === '/metrics/history') return json(res, 200, history(id, url.searchParams.get('key') || '', url.searchParams.get('window') || '15m'));
    if (sub === '/events') return json(res, 200, ring[id].slice(0, Number(url.searchParams.get('limit') || 50)));
    if (sub === '/events/stream') {
      if (streams.size >= 8) return err(res, 429, 'too_many_streams', 'at most 8 concurrent event streams');
      res.writeHead(200, { 'Content-Type': 'text/event-stream', 'Cache-Control': 'no-store', Connection: 'keep-alive', ...SEC });
      res.write(': ok\n\n'); const c = { res, system: id }; streams.add(c); req.on('close', () => streams.delete(c)); return;
    }
    if (sub === '/config') return json(res, 200, specs[id]);
    let rm;
    if (id === 'datadesigner' && (rm = sub.match(/^\/runs\/([^/]+)\/(records|files)$/))) {
      const rid = decodeURIComponent(rm[1]);
      // A live create run has no records.jsonl yet: an empty (not a missing) records response.
      const run = DD_RUNS.find((r) => r.run_id === rid) || (dd.live?.run_id === rid ? { rows: [], columns: [], files: [], run_id: rid } : null);
      if (!run) return err(res, 404, 'not_found', `unknown run ${rm[1]}`);
      if (rm[2] === 'files') return json(res, 200, { run_id: run.run_id, files: run.files || [] });
      const limit = Math.max(1, Math.min(200, Number(url.searchParams.get('limit')) || 50));
      const offset = Math.max(0, Number(url.searchParams.get('offset')) || 0);
      const all = run.rows || [];
      return json(res, 200, { run_id: run.run_id, total: all.length, columns: run.columns || (all[0] ? Object.keys(all[0]) : []),
        rows: all.slice(offset, offset + limit), truncated: all.length > 2000 });
    }
    let mm;
    if ((mm = sub.match(/^\/config\/([\w.-]+)(\/(validate|apply|history|rollback))?$/))) {
      const spec = specs[id].find((s) => s.id === mm[1]);
      if (!spec) return err(res, 404, 'not_found', `unknown config file ${mm[1]}`);
      const f = files[`${id}/${spec.id}`], act = mm[3];
      if (!act && M === 'GET') return json(res, 200, { spec, content: f.content, sha256: sha(f.content), mtime: f.mtime });
      if (act === 'history') return json(res, 200, f.history.map((r) => ({ id: r.id, ts: r.ts, actor: r.actor, sha256: sha(r.content), note: r.note })).reverse());
      if (act === 'validate' && M === 'POST') { if (typeof body.content !== 'string') return err(res, 422, 'invalid_request', 'content is required'); return json(res, 200, validate(id, spec.id, body.content)); }
      if (act === 'apply' && M === 'POST') {
        if (!spec.editable) return err(res, 403, 'read_only', `${spec.display_path} is read-only in this console`);
        if (body.confirm !== true) return err(res, 422, 'confirm_required', 'confirm:true is required');
        if (typeof body.content !== 'string' || body.content.length > 262144) return err(res, body.content ? 413 : 422, body.content ? 'too_large' : 'invalid_request', 'content missing or over 256 KiB');
        if (looksSecret(body.content)) return err(res, 422, 'secret_in_content', 'content looks like it contains a secret; reference an env var name instead');
        if (body.base_sha256 !== sha(f.content)) return err(res, 409, 'stale_base', 'the file changed on disk since it was read (base_sha256 mismatch)');
        const restart = body.restart !== false && id !== 'datadesigner' && (!SIX.includes(id) || SIX_RESTARTS.has(id));   // a script has nothing to restart
        const r = await applyFile(id, spec.id, body.content, body.base_sha256, restart, undefined, id === 'datadesigner' || (SIX.includes(id) && !SIX_RESTARTS.has(id)) ? 'healthy' : undefined);
        audit(id, 'POST', `config.apply ${spec.id}`, { restart: body.restart, content: body.content, base_sha256: body.base_sha256 }, r.ok ? 200 : 200); return json(res, 200, r);
      }
      if (act === 'rollback' && M === 'POST') {
        if (!spec.editable) return err(res, 403, 'read_only', 'read-only file');
        const rev = f.history.find((r) => r.id === body.revision_id);
        if (!rev) return err(res, 404, 'not_found', 'unknown revision');
        if (body.confirm !== true) return err(res, 422, 'confirm_required', 'confirm:true is required');
        const r = await applyFile(id, spec.id, rev.content, sha(f.content), true, `before rollback to ${rev.id}`);
        audit(id, 'POST', `config.rollback ${spec.id}`, { revision_id: rev.id }, 200); return json(res, 200, r);
      }
      return err(res, 404, 'not_found', `${M} ${p}`);
    }
    if (sub === '/params' && M === 'GET') return json(res, 200, id === 'dynamo' ? { catalog: DYNAMO_CATALOG, values: state.params } : { catalog: [], values: {} });
    if (sub === '/params/preview' && M === 'POST') {
      if (id !== 'dynamo') return err(res, 404, 'not_found', 'no params');
      const bad = validateParams(body.values || {}); if (bad) return err(res, 422, 'invalid_params', bad);
      const w = []; if ((body.values || {}).gpu_memory_utilization > 0.95) w.push('gpu_memory_utilization above 0.95 leaves almost no headroom on a card shared with vllm-qwen38.');
      if ((body.values || {}).kv_cache_dtype === 'fp8') w.push('fp8 KV cache: verify output quality on your prompts.');
      return json(res, 200, { rendered: renderOverride(body.values || {}), warnings: w });
    }
    if (sub === '/params' && M === 'PUT') {
      if (id !== 'dynamo') return err(res, 404, 'not_found', 'no params');
      if (body.confirm !== true) return err(res, 422, 'confirm_required', 'confirm:true is required');
      const bad = validateParams(body.values || {}); if (bad) return err(res, 422, 'invalid_params', bad);
      const keep = state.params.hf_token; state.params = { ...body.values }; if (keep && state.params.hf_token === undefined) state.params.hf_token = keep;
      goLoading(); audit(id, 'PUT', 'params', body.values, 200);
      return json(res, 200, { ok: true, new_sha256: sha(renderOverride(state.params)), revision_id: null, restarted: true, post_check: 'healthy', detail: 'override written to ~/.local/state/nemo-console/dynamo/compose.override.yml\nworker recreated; loading weights', rolled_back: false });
    }
    if (sub === '/actions' && M === 'GET') return json(res, 200, ACTIONS[id]);
    if ((mm = sub.match(/^\/actions\/(\w+)$/)) && M === 'POST') {
      const a = ACTIONS[id].find((x) => x.id === mm[1]);
      if (!a) return err(res, 404, 'not_found', `unknown action ${mm[1]}`);
      if (a.danger !== 'safe' && body.confirm !== true) return err(res, 422, 'confirm_required', `${a.id} is ${a.danger}: confirm:true is required`);
      await wait(a.danger === 'safe' ? 300 : 700);
      const r = runAction(id, a.id, body.params || {}); audit(id, 'POST', `action ${a.id}`, body.params || {}, r.ok ? 200 : 200); return json(res, 200, r);
    }
    if (sub === '/playground' && M === 'GET') return json(res, 200, PLAY[id] || null);
    if (sub === '/playground/chat' && M === 'POST') return playgroundChat(req, res, id, body);
  }
  return err(res, 404, 'not_found', `${M} ${p}`);
}

// ---------------------------------------------------------------- dev helpers + static
function dev(req, res, url) {
  const q = url.searchParams, p = url.pathname;
  // These helpers mutate state over GET, so they need the custom header a cross-site <img>/form cannot send (it forces a CORS preflight).
  if (req.headers['x-console'] !== '1') return err(res, 403, 'csrf', 'missing X-Console: 1 header');
  if (p === '/__dev/scenario') { const d = q.get('dynamo'); if (['up', 'loading', 'down', 'off'].includes(d)) { state.dynamo = d; if (d === 'loading') goLoading(); } return json(res, 200, { dynamo: state.dynamo }); }
  if (p === '/__dev/vllm') { state.vllmRunning = q.get('run') === '1'; return json(res, 200, { vllmRunning: state.vllmRunning }); }
  if (p === '/__dev/role') { state.role = ['viewer', 'public'].includes(q.get('set')) ? q.get('set') : 'admin'; return json(res, 200, { role: state.role }); }
  if (p === '/__dev/touch') { const f = files['switchyard/routes']; f.content += `\n# edited on disk by another process at ${new Date().toISOString()}\n`; f.mtime = now(); return json(res, 200, { sha256: sha(f.content) }); }
  if (p === '/__dev/fail') { state.failNext = { status: Number(q.get('status') || 503), n: Number(q.get('n') || 1) }; return json(res, 200, state.failNext); }
  if (p === '/__dev/six') { if (q.has('dcgm_fixed')) state.six.dcgmFixed = q.get('dcgm_fixed') === '1'; if (q.has('playground')) state.six.guardPlayground = q.get('playground') === '1'; return json(res, 200, state.six); }
  if (p === '/__dev/dd') {
    const do_ = q.get('do');
    if (do_ === 'start' && !dd.live) runAction('datadesigner', 'create', { script: DD_SCRIPTS[0], num_records: 500, dataset_name: 'support_tickets_v2', destination_subdir: 'support_tickets_v2', format: 'parquet' });
    if (do_ === 'finish' && dd.live) ddFinish(dd.live.run_id);
    return json(res, 200, { live: dd.live?.run_id || null });
  }
  return json(res, 404, { error: { code: 'not_found', message: 'unknown dev helper' } });
}
// /__shot serves index.html plus a slow 1x1 image so the browser `load` event (when screenshot tools fire) is held until the SPA has rendered.
function serveShot(res, url) {
  if (url.pathname === '/__slow.gif') return setTimeout(() => { res.writeHead(200, { 'Content-Type': 'image/gif', ...SEC }); res.end(Buffer.from('R0lGODlhAQABAAAAACw=', 'base64')); }, Math.min(20000, Number(url.searchParams.get('ms') || 5000)));
  if (url.pathname === '/__demo.js') { res.writeHead(200, { 'Content-Type': 'text/javascript', ...SEC }); return res.end(DEMOS[url.searchParams.get('name')] || ''); }
  const demo = /^[\w-]+$/.test(url.searchParams.get('demo') || '') ? `<script type="module" src="/__demo.js?name=${url.searchParams.get('demo')}"></script>` : '';
  const html = fs.readFileSync(path.join(ROOT, 'index.html'), 'utf8').replace('</body>', `<img src="/__slow.gif?ms=${Number(url.searchParams.get('ms') || 5000)}" width="1" height="1" alt="">${demo}</body>`);
  res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8', ...SEC }); res.end(html);
}
function serveStatic(req, res, url) {
  // Reject traversal spellings on the RAW path, before any decoding.
  if (/\.\.|%2e|%2f|%5c|\\/i.test(String(req.url).split('?')[0])) { res.writeHead(400, SEC); return res.end('bad path'); }
  let rel;
  try { rel = decodeURIComponent(url.pathname); } catch { res.writeHead(400, SEC); return res.end('bad path'); }
  if (rel === '/') rel = '/index.html';
  const abs = path.resolve(ROOT, '.' + rel);
  if (!abs.startsWith(ROOT + path.sep) || /^\/(dev|tests|node_modules)\//.test(rel) || /package\.json$/.test(rel)) { res.writeHead(404, SEC); return res.end('not found'); }
  fs.readFile(abs, (e, buf) => {
    if (e) { res.writeHead(404, SEC); return res.end('not found'); }
    res.writeHead(200, { 'Content-Type': MIME[path.extname(abs)] || 'application/octet-stream', 'Cache-Control': 'no-store', ...SEC }); res.end(buf);
  });
}

function proxy(req, res) {
  const t = new URL(PROXY);
  const up = http.request({ host: t.hostname, port: t.port, path: req.url, method: req.method, headers: { ...req.headers, host: t.host, ...(req.headers.origin ? { origin: `http://${t.host}` } : {}) } }, (r) => { res.writeHead(r.statusCode, r.headers); r.pipe(res); });
  up.on('error', (e) => err(res, 502, 'bad_gateway', String(e.message)));
  res.on('close', () => up.destroy()); req.pipe(up);
}
export const server = http.createServer(async (req, res) => {
  const url = new URL(req.url, 'http://x');
  const hostHdr = String(req.headers.host || '');
  if (!(/^(127\.0\.0\.1|localhost|\[::1\])$/.test(hostHdr.replace(/:\d+$/, '')) && (hostHdr.match(/:(\d+)$/)?.[1] ?? '80') === String(server.address()?.port))) return err(res, 421, 'bad_host', 'Host not allowed');
  try {
    if (PROXY && url.pathname.startsWith('/api/')) return proxy(req, res);
    if (url.pathname.startsWith('/api/v1')) return await api(req, res, url);
    if (url.pathname.startsWith('/__dev/')) return dev(req, res, url);
    if (['/__shot', '/__slow.gif', '/__demo.js'].includes(url.pathname)) return serveShot(res, url);
    return serveStatic(req, res, url);
  } catch (e) { if (!res.headersSent) err(res, 500, 'internal', String(e.message)); else res.end(); }
});
export const _state = state;
if (process.argv[1] === fileURLToPath(import.meta.url)) {
  server.listen(PORT, HOST, () => console.log(`mock nemo-console: http://${HOST}:${server.address().port}/  (${KEY ? 'bearer required' : 'no auth'}, role ${state.role}, dynamo ${state.dynamo})`));
}
