// Per-system cases: the read routes for every system id, invalid ids, SSE streams, config file round trips
// (validate -> apply -> history -> rollback), structured params, every action of every adapter (each answered by the
// fake CLIs / fake upstreams), the playground, and the Data Designer run artifacts.
import { appendFileSync, readdirSync, readFileSync, existsSync } from 'node:fs';
import { join } from 'node:path';

const API = '/api/v1';
export const SYSTEM_IDS = ['switchyard', 'relay', 'dynamo', 'skillevaluator', 'datadesigner', 'openshell', 'dcgm', 'aiperf', 'guardrails', 'evaluator'];
const sys = (id, sub = '') => `${API}/systems/${id}${sub}`;
const ZERO_SHA = '0'.repeat(64);

/** Read routes of every system (status, metrics, history, events, actions, config list, playground, params). */
export function systemReadCases(tag = 'read') {
  const c = [];
  const R = (route, o) => c.push({ route, ...o });
  for (const id of SYSTEM_IDS) {
    R('GET /api/v1/systems/{system_id}/status', { id: `${tag}.${id}.status`, path: sys(id, '/status') });
    R('GET /api/v1/systems/{system_id}/metrics', { id: `${tag}.${id}.metrics`, path: sys(id, '/metrics') });
    R('GET /api/v1/systems/{system_id}/actions', { id: `${tag}.${id}.actions`, path: sys(id, '/actions') });
    R('GET /api/v1/systems/{system_id}/config', { id: `${tag}.${id}.config`, path: sys(id, '/config') });
    R('GET /api/v1/systems/{system_id}/playground', { id: `${tag}.${id}.playground`, path: sys(id, '/playground') });
    R('GET /api/v1/systems/{system_id}/params', { id: `${tag}.${id}.params`, path: sys(id, '/params') });
    R('GET /api/v1/systems/{system_id}/events', { id: `${tag}.${id}.events`, path: sys(id, '/events?limit=20') });
    R('GET /api/v1/systems/{system_id}/metrics/history', { id: `${tag}.${id}.history`, path: sys(id, '/metrics/history?window=15m&key=x.requests_total') });
  }
  return c;
}

/** Variants of the read routes: viewer, defaults, validation errors, unknown systems and ids. */
export function systemReadVariantCases() {
  const c = [];
  const R = (route, o) => c.push({ route, ...o });
  R('GET /api/v1/systems/{system_id}/status', { id: 'sysv.status.viewer', path: sys('relay', '/status'), auth: 'viewer' });
  R('GET /api/v1/systems/{system_id}/status', { id: 'sysv.status.no-auth', path: sys('relay', '/status'), auth: 'none' });
  R('GET /api/v1/systems/{system_id}/status', { id: 'sysv.status.unknown-system', path: sys('bogus', '/status') });
  R('GET /api/v1/systems/{system_id}/status', { id: 'sysv.status.upper-system', path: sys('Switchyard', '/status') });
  R('GET /api/v1/systems/{system_id}/status', { id: 'sysv.status.encoded-system', path: `${API}/systems/%73witchyard/status` });
  R('GET /api/v1/systems/{system_id}/status', { id: 'sysv.status.dotdot', path: `${API}/systems/../systems/relay/status` });
  R('GET /api/v1/systems/{system_id}/metrics', { id: 'sysv.metrics.viewer', path: sys('dcgm', '/metrics'), auth: 'viewer' });
  R('GET /api/v1/systems/{system_id}/metrics', { id: 'sysv.metrics.unknown-system', path: sys('nope', '/metrics') });
  const H = 'GET /api/v1/systems/{system_id}/metrics/history';
  R(H, { id: 'sysv.history.default', path: sys('switchyard', '/metrics/history?key=sy.requests_total') });
  R(H, { id: 'sysv.history.1h', path: sys('switchyard', '/metrics/history?window=1h&key=sy.requests_total') });
  R(H, { id: 'sysv.history.24h', path: sys('dcgm', '/metrics/history?window=24h&key=dcgm.gpu_temp_c') });
  R(H, { id: 'sysv.history.labels', path: sys('switchyard', '/metrics/history?window=15m&key=sy.tokens%7Bkind%3Dprompt%7D') });
  R(H, { id: 'sysv.history.no-key', path: sys('switchyard', '/metrics/history') });
  R(H, { id: 'sysv.history.bad-window', path: sys('switchyard', '/metrics/history?window=7d&key=sy.requests_total') });
  R(H, { id: 'sysv.history.bad-key', path: sys('switchyard', '/metrics/history?key=Bad-Key') });
  R(H, { id: 'sysv.history.key-too-long', path: sys('switchyard', `/metrics/history?key=s${'x'.repeat(70)}`) });
  R(H, { id: 'sysv.history.unknown-system', path: sys('bogus', '/metrics/history?key=sy.requests_total') });
  R(H, { id: 'sysv.history.viewer', path: sys('relay', '/metrics/history?key=relay.gateway_up'), auth: 'viewer' });
  const E = 'GET /api/v1/systems/{system_id}/events';
  R(E, { id: 'sysv.events.default', path: sys('relay', '/events') });
  R(E, { id: 'sysv.events.limit-1', path: sys('relay', '/events?limit=1') });
  R(E, { id: 'sysv.events.limit-500', path: sys('switchyard', '/events?limit=500') });
  R(E, { id: 'sysv.events.limit-0', path: sys('relay', '/events?limit=0') });
  R(E, { id: 'sysv.events.limit-501', path: sys('relay', '/events?limit=501') });
  R(E, { id: 'sysv.events.limit-abc', path: sys('relay', '/events?limit=abc') });
  R(E, { id: 'sysv.events.limit-negative', path: sys('relay', '/events?limit=-3') });
  R(E, { id: 'sysv.events.unknown-system', path: sys('bogus', '/events') });
  R(E, { id: 'sysv.events.viewer', path: sys('switchyard', '/events?limit=3'), auth: 'viewer' });
  R('GET /api/v1/systems/{system_id}/actions', { id: 'sysv.actions.viewer', path: sys('dynamo', '/actions'), auth: 'viewer' });
  R('GET /api/v1/systems/{system_id}/actions', { id: 'sysv.actions.unknown-system', path: sys('bogus', '/actions') });
  R('GET /api/v1/systems/{system_id}/config', { id: 'sysv.config.viewer', path: sys('relay', '/config'), auth: 'viewer' });
  R('GET /api/v1/systems/{system_id}/config', { id: 'sysv.config.unknown-system', path: sys('bogus', '/config') });
  const F = 'GET /api/v1/systems/{system_id}/config/{file_id}';
  R(F, { id: 'sysv.file.unknown-file', path: sys('switchyard', '/config/nope') });
  R(F, { id: 'sysv.file.bad-id-upper', path: sys('switchyard', '/config/Routes') });
  R(F, { id: 'sysv.file.bad-id-chars', path: sys('switchyard', '/config/rou%24tes') });
  R(F, { id: 'sysv.file.bad-id-long', path: sys('switchyard', `/config/${'a'.repeat(70)}`) });
  R(F, { id: 'sysv.file.encoded-slash', path: sys('switchyard', '/config/..%2Froutes') });
  R(F, { id: 'sysv.file.unknown-system', path: sys('bogus', '/config/routes') });
  R(F, { id: 'sysv.file.no-config-system', path: sys('skillevaluator', '/config/routes') });
  R(F, { id: 'sysv.file.no-auth', path: sys('switchyard', '/config/routes'), auth: 'none' });
  const HI = 'GET /api/v1/systems/{system_id}/config/{file_id}/history';
  R(HI, { id: 'sysv.file-history.unknown-file', path: sys('switchyard', '/config/nope/history') });
  R(HI, { id: 'sysv.file-history.unknown-system', path: sys('bogus', '/config/routes/history') });
  R(HI, { id: 'sysv.file-history.viewer', path: sys('relay', '/config/routes/history'), auth: 'viewer' });
  R('GET /api/v1/systems/{system_id}/playground', { id: 'sysv.playground.unknown-system', path: sys('bogus', '/playground') });
  R('GET /api/v1/systems/{system_id}/playground', { id: 'sysv.playground.viewer', path: sys('switchyard', '/playground'), auth: 'viewer' });
  R('GET /api/v1/systems/{system_id}/params', { id: 'sysv.params.unknown-system', path: sys('bogus', '/params') });
  R('GET /api/v1/systems/{system_id}/params', { id: 'sysv.params.viewer', path: sys('dynamo', '/params'), auth: 'viewer' });
  return c;
}

/** Every config file of every system, as admin and as viewer (viewers get secrets redacted, env names kept). */
export function configReadCases(files) {
  const c = [];
  for (const [id, ids] of Object.entries(files)) {
    for (const f of ids) {
      c.push({ id: `cfgread.${id}.${f}`, route: 'GET /api/v1/systems/{system_id}/config/{file_id}', path: sys(id, `/config/${f}`) });
      c.push({ id: `cfgread.${id}.${f}.viewer`, route: 'GET /api/v1/systems/{system_id}/config/{file_id}', path: sys(id, `/config/${f}`), auth: 'viewer' });
      c.push({ id: `cfgread.${id}.${f}.history`, route: 'GET /api/v1/systems/{system_id}/config/{file_id}/history', path: sys(id, `/config/${f}/history`) });
    }
  }
  return c;
}

/** SSE: open each system's event stream and read it for a bounded time. */
export function sseCases(readMs = 1500) {
  const c = [];
  for (const id of SYSTEM_IDS) {
    c.push({ id: `sse.${id}`, route: 'GET /api/v1/systems/{system_id}/events/stream', path: sys(id, '/events/stream'), readMs });
  }
  c.push({ id: 'sse.viewer', route: 'GET /api/v1/systems/{system_id}/events/stream', path: sys('aiperf', '/events/stream'), readMs: 600, auth: 'viewer' });
  c.push({ id: 'sse.unknown-system', route: 'GET /api/v1/systems/{system_id}/events/stream', path: sys('bogus', '/events/stream'), readMs: 600 });
  c.push({ id: 'sse.no-auth', route: 'GET /api/v1/systems/{system_id}/events/stream', path: sys('relay', '/events/stream'), readMs: 600, auth: 'none' });
  return c;
}

/** Concurrency: one mutation per system at a time (409 busy), and the SSE connection cap (8 -> the 9th is refused). */
export function concurrencyCases() {
  const sse = { method: 'GET', path: sys('relay', '/events/stream'), readMs: 2500 };
  return [
    { id: 'conc.busy-409', route: 'POST /api/v1/systems/{system_id}/actions/{action}', concurrent: [
      { method: 'POST', path: sys('relay', '/actions/gateway_restart'), json: { confirm: true }, timeoutMs: 120_000 },
      { method: 'POST', path: sys('relay', '/actions/gateway_status'), json: {}, delayMs: 700 },
    ] },
    { id: 'conc.sse-cap-429', route: 'GET /api/v1/systems/{system_id}/events/stream', concurrent: [
      ...Array.from({ length: 8 }, () => sse),
      { method: 'GET', path: sys('relay', '/events/stream'), readMs: 500, delayMs: 1200 },
    ] },
  ];
}

/** Relay follow_events tails the active ATOF file: a line appended while the stream is open must arrive as an event. */
export function sseAppendCases() {
  const line = JSON.stringify({ kind: 'event', name: 'llm.request', category: 'llm', timestamp: '2026-10-01T10:00:00Z',
    session_id: 'parity-session', data: { model: 'auto-local', route: 'auto-local', status: 200 } });
  return [{
    id: 'sse.relay.append', route: 'GET /api/v1/systems/{system_id}/events/stream', path: sys('relay', '/events/stream'), readMs: 4000,
    during: { afterMs: 1200, run: (side) => appendFileSync(join(side.vars.home, '.local/share/nemo-relay/atof/relay-events.jsonl'), `${line}\n`) },
  }];
}

/**
 * The config round trip for one editable file: GET (capture sha + content) -> validate -> refusals (confirm, stale
 * base, bad sha, secret, NUL, size, extra field, wrong types) -> apply -> history -> rollback refusals -> rollback.
 * `edit(content)` is the JS template of the new content, given as a {{var}} expression via `newContent`.
 */
export function configRoundTrip(id, file, { tag = file, restart = false, appendText = '\n# parity edit\n', invalid, rollbackRestart = true, unhealthyMarker } = {}) {
  const v = `${id}_${tag}`.replace(/[^A-Za-z0-9]/g, '_');
  const base = sys(id, `/config/${file}`);
  const c = [];
  const R = (route, o) => c.push({ route, ...o, id: `cfg.${id}.${tag}.${o.id}` });
  const V = 'POST /api/v1/systems/{system_id}/config/{file_id}/validate';
  const A = 'POST /api/v1/systems/{system_id}/config/{file_id}/apply';
  const B = 'POST /api/v1/systems/{system_id}/config/{file_id}/rollback';
  R('GET /api/v1/systems/{system_id}/config/{file_id}', { id: 'get', path: base, capture: { [`${v}_sha`]: '/sha256', [`${v}_content`]: '/content' } });
  R(V, { id: 'validate.same', method: 'POST', path: `${base}/validate`, json: { content: `{{${v}_content}}` } });
  R(V, { id: 'validate.edited', method: 'POST', path: `${base}/validate`, json: { content: `{{${v}_content}}${appendText}` } });
  if (invalid !== undefined) R(V, { id: 'validate.invalid', method: 'POST', path: `${base}/validate`, json: { content: invalid } });
  R(V, { id: 'validate.secret', method: 'POST', path: `${base}/validate`, json: { content: 'api_key = "sk-abcdefghijklmnopqrstuvwxyz"\n' } });
  R(V, { id: 'validate.missing-content', method: 'POST', path: `${base}/validate`, json: {} });
  R(V, { id: 'validate.content-number', method: 'POST', path: `${base}/validate`, json: { content: 5 } });
  R(V, { id: 'validate.extra', method: 'POST', path: `${base}/validate`, json: { content: 'x', confirm: true } });
  R(V, { id: 'validate.viewer', method: 'POST', path: `${base}/validate`, json: { content: 'x' }, auth: 'viewer' });
  R(A, { id: 'apply.no-confirm', method: 'POST', path: `${base}/apply`, json: { content: `{{${v}_content}}${appendText}`, base_sha256: `{{${v}_sha}}`, restart } });
  R(A, { id: 'apply.stale-base', method: 'POST', path: `${base}/apply`, json: { content: `{{${v}_content}}${appendText}`, base_sha256: ZERO_SHA, restart, confirm: true } });
  R(A, { id: 'apply.bad-sha', method: 'POST', path: `${base}/apply`, json: { content: 'x', base_sha256: 'abc', confirm: true } });
  R(A, { id: 'apply.upper-sha', method: 'POST', path: `${base}/apply`, json: { content: 'x', base_sha256: 'A'.repeat(64), confirm: true } });
  R(A, { id: 'apply.missing-base', method: 'POST', path: `${base}/apply`, json: { content: 'x', confirm: true } });
  R(A, { id: 'apply.secret', method: 'POST', path: `${base}/apply`, json: { content: 'token: "ghp_abcdefghijklmnopqrstuvwxyz0123"\n', base_sha256: `{{${v}_sha}}`, confirm: true } });
  R(A, { id: 'apply.nul', method: 'POST', path: `${base}/apply`, json: { content: 'a\u0000b', base_sha256: `{{${v}_sha}}`, confirm: true } });
  R(A, { id: 'apply.too-large', method: 'POST', path: `${base}/apply`, json: { content: '#'.repeat(256 * 1024 + 1), base_sha256: `{{${v}_sha}}`, confirm: true } });
  R(A, { id: 'apply.restart-string', method: 'POST', path: `${base}/apply`, json: { content: 'x', base_sha256: ZERO_SHA, restart: 'no', confirm: true } });
  R(A, { id: 'apply.confirm-int', method: 'POST', path: `${base}/apply`, json: { content: `{{${v}_content}}${appendText}`, base_sha256: ZERO_SHA, confirm: 1 } });
  R(A, { id: 'apply.viewer', method: 'POST', path: `${base}/apply`, json: { content: 'x', base_sha256: ZERO_SHA, confirm: true }, auth: 'viewer' });
  if (invalid !== undefined) R(A, { id: 'apply.invalid', method: 'POST', path: `${base}/apply`, json: { content: invalid, base_sha256: `{{${v}_sha}}`, restart, confirm: true } });
  R(A, { id: 'apply.ok', method: 'POST', path: `${base}/apply`, json: { content: `{{${v}_content}}${appendText}`, base_sha256: `{{${v}_sha}}`, restart, confirm: true },
    capture: { [`${v}_rev`]: '/revision_id' } });
  R('GET /api/v1/systems/{system_id}/config/{file_id}', { id: 'get.after-apply', path: base, capture: { [`${v}_sha2`]: '/sha256' } });
  R('GET /api/v1/systems/{system_id}/config/{file_id}/history', { id: 'history.after-apply', path: `${base}/history` });
  R(A, { id: 'apply.stale-old-base', method: 'POST', path: `${base}/apply`, json: { content: `{{${v}_content}}`, base_sha256: `{{${v}_sha}}`, restart, confirm: true } });
  R(B, { id: 'rollback.no-confirm', method: 'POST', path: `${base}/rollback`, json: { revision_id: `{{${v}_rev}}` } });
  R(B, { id: 'rollback.bad-id', method: 'POST', path: `${base}/rollback`, json: { revision_id: 'xyz', confirm: true } });
  R(B, { id: 'rollback.short-id', method: 'POST', path: `${base}/rollback`, json: { revision_id: 'abcdef1', confirm: true } });
  R(B, { id: 'rollback.unknown-id', method: 'POST', path: `${base}/rollback`, json: { revision_id: 'abcdef0123456789', confirm: true } });
  R(B, { id: 'rollback.missing-id', method: 'POST', path: `${base}/rollback`, json: { confirm: true } });
  R(B, { id: 'rollback.viewer', method: 'POST', path: `${base}/rollback`, json: { revision_id: `{{${v}_rev}}`, confirm: true }, auth: 'viewer' });
  R(B, { id: 'rollback.ok', method: 'POST', path: `${base}/rollback`, json: { revision_id: `{{${v}_rev}}`, confirm: true }, timeoutMs: rollbackRestart ? 180_000 : 60_000 });
  R('GET /api/v1/systems/{system_id}/config/{file_id}/history', { id: 'history.after-rollback', path: `${base}/history` });
  R('GET /api/v1/systems/{system_id}/config/{file_id}', { id: 'get.after-rollback', path: base, capture: { [`${v}_sha3`]: '/sha256' } });
  if (unhealthyMarker) {
    // the fake restart fails while the file carries the marker: post-apply is unhealthy, the core restores the old
    // file and runs post_apply again (rolled_back: true)
    R(A, { id: 'apply.auto-rollback', method: 'POST', path: `${base}/apply`, timeoutMs: 180_000,
      json: { content: `{{${v}_content}}\n# ${unhealthyMarker}\n`, base_sha256: `{{${v}_sha3}}`, restart: true, confirm: true } });
    R('GET /api/v1/systems/{system_id}/config/{file_id}/history', { id: 'history.after-auto-rollback', path: `${base}/history` });
    R('GET /api/v1/systems/{system_id}/config/{file_id}', { id: 'get.after-auto-rollback', path: base });
  }
  return c;
}

/** Refusals on read-only files and files of other systems. */
export function configRefusalCases() {
  const c = [];
  const A = 'POST /api/v1/systems/{system_id}/config/{file_id}/apply';
  const V = 'POST /api/v1/systems/{system_id}/config/{file_id}/validate';
  const B = 'POST /api/v1/systems/{system_id}/config/{file_id}/rollback';
  c.push({ id: 'cfgx.relay.plugins.apply-readonly', route: A, method: 'POST', path: sys('relay', '/config/plugins/apply'), json: { content: 'x = 1\n', base_sha256: ZERO_SHA, confirm: true } });
  c.push({ id: 'cfgx.relay.plugins.validate', route: V, method: 'POST', path: sys('relay', '/config/plugins/validate'), json: { content: '[policy]\nsignature = "required"\n' } });
  c.push({ id: 'cfgx.relay.config.validate-bad-toml', route: V, method: 'POST', path: sys('relay', '/config/config/validate'), json: { content: '[unclosed\n' } });
  c.push({ id: 'cfgx.openshell.gateway.apply-readonly', route: A, method: 'POST', path: sys('openshell', '/config/gateway/apply'), json: { content: 'x = 1\n', base_sha256: ZERO_SHA, confirm: true } });
  c.push({ id: 'cfgx.openshell.policy.validate', route: V, method: 'POST', path: sys('openshell', '/config/policy/validate'), json: { content: 'version: 1\n' } });
  c.push({ id: 'cfgx.openshell.policy.rollback-readonly', route: B, method: 'POST', path: sys('openshell', '/config/policy/rollback'), json: { revision_id: 'abcdef0123456789', confirm: true } });
  c.push({ id: 'cfgx.guardrails.actions-py.validate', route: V, method: 'POST', path: sys('guardrails', '/config/lab-input-output--actions.py/validate'), json: { content: 'def x():\n    return 2\n' } });
  c.push({ id: 'cfgx.guardrails.actions-py.apply', route: A, method: 'POST', path: sys('guardrails', '/config/lab-input-output--actions.py/apply'), json: { content: 'x', base_sha256: ZERO_SHA, confirm: true } });
  c.push({ id: 'cfgx.dynamo.compose.apply-readonly', route: A, method: 'POST', path: sys('dynamo', '/config/compose/apply'), json: { content: 'services: {}\n', base_sha256: ZERO_SHA, confirm: true } });
  c.push({ id: 'cfgx.unknown-file.validate', route: V, method: 'POST', path: sys('switchyard', '/config/nope/validate'), json: { content: 'x' } });
  c.push({ id: 'cfgx.unknown-file.apply', route: A, method: 'POST', path: sys('switchyard', '/config/nope/apply'), json: { content: 'x', base_sha256: ZERO_SHA, confirm: true } });
  c.push({ id: 'cfgx.unknown-system.apply', route: A, method: 'POST', path: sys('bogus', '/config/routes/apply'), json: { content: 'x', base_sha256: ZERO_SHA, confirm: true } });
  c.push({ id: 'cfgx.unknown-file.rollback', route: B, method: 'POST', path: sys('switchyard', '/config/nope/rollback'), json: { revision_id: 'abcdef0123456789', confirm: true } });
  c.push({ id: 'cfgx.malformed-json', route: V, method: 'POST', path: sys('switchyard', '/config/routes/validate'), body: '{"content": "x"', contentType: 'application/json' });
  c.push({ id: 'cfgx.form-encoded', route: V, method: 'POST', path: sys('switchyard', '/config/routes/validate'), body: 'content=x', contentType: 'application/x-www-form-urlencoded' });
  c.push({ id: 'cfgx.no-body', route: V, method: 'POST', path: sys('switchyard', '/config/routes/validate') });
  c.push({ id: 'cfgx.array-body', route: A, method: 'POST', path: sys('switchyard', '/config/routes/apply'), json: [{ content: 'x' }] });
  return c;
}

/** Structured params: only Dynamo has a catalog; every other system answers "unsupported". */
export function paramsCases(validValues, invalidValues) {
  const c = [];
  const P = 'POST /api/v1/systems/{system_id}/params/preview';
  const U = 'PUT /api/v1/systems/{system_id}/params';
  c.push({ id: 'params.dynamo.get', route: 'GET /api/v1/systems/{system_id}/params', path: sys('dynamo', '/params') });
  c.push({ id: 'params.dynamo.preview.empty', route: P, method: 'POST', path: sys('dynamo', '/params/preview'), json: {} });
  c.push({ id: 'params.dynamo.preview.valid', route: P, method: 'POST', path: sys('dynamo', '/params/preview'), json: { values: validValues } });
  for (const [i, vals] of invalidValues.entries()) {
    c.push({ id: `params.dynamo.preview.invalid-${i}`, route: P, method: 'POST', path: sys('dynamo', '/params/preview'), json: { values: vals } });
  }
  c.push({ id: 'params.dynamo.preview.extra', route: P, method: 'POST', path: sys('dynamo', '/params/preview'), json: { values: {}, confirm: true } });
  c.push({ id: 'params.dynamo.preview.values-list', route: P, method: 'POST', path: sys('dynamo', '/params/preview'), json: { values: [] } });
  c.push({ id: 'params.dynamo.preview.viewer', route: P, method: 'POST', path: sys('dynamo', '/params/preview'), json: { values: {} }, auth: 'viewer' });
  c.push({ id: 'params.dynamo.put.no-confirm', route: U, method: 'PUT', path: sys('dynamo', '/params'), json: { values: validValues } });
  c.push({ id: 'params.dynamo.put.invalid', route: U, method: 'PUT', path: sys('dynamo', '/params'), json: { values: invalidValues[0], confirm: true } });
  c.push({ id: 'params.dynamo.put.viewer', route: U, method: 'PUT', path: sys('dynamo', '/params'), json: { values: validValues, confirm: true }, auth: 'viewer' });
  c.push({ id: 'params.dynamo.put.ok', route: U, method: 'PUT', path: sys('dynamo', '/params'), json: { values: validValues, confirm: true }, timeoutMs: 180_000 });
  c.push({ id: 'params.dynamo.get.after', route: 'GET /api/v1/systems/{system_id}/params', path: sys('dynamo', '/params') });
  c.push({ id: 'params.dynamo.config.override.after', route: 'GET /api/v1/systems/{system_id}/config/{file_id}', path: sys('dynamo', '/config/override') });
  for (const id of ['switchyard', 'aiperf']) {
    c.push({ id: `params.${id}.preview.unsupported`, route: P, method: 'POST', path: sys(id, '/params/preview'), json: { values: {} } });
    c.push({ id: `params.${id}.put.unsupported`, route: U, method: 'PUT', path: sys(id, '/params'), json: { values: {}, confirm: true } });
  }
  c.push({ id: 'params.unknown-system.preview', route: P, method: 'POST', path: sys('bogus', '/params/preview'), json: { values: {} } });
  c.push({ id: 'params.unknown-system.put', route: U, method: 'PUT', path: sys('bogus', '/params'), json: { values: {} } });
  return c;
}

/** Generic action-route checks (validation, roles, unknown ids) on one system. */
export function actionRouteCases() {
  const R = 'POST /api/v1/systems/{system_id}/actions/{action}';
  const a = (id, action, o) => ({ id: `act.route.${id}`, route: R, method: 'POST', path: sys(o.system || 'switchyard', `/actions/${action}`), ...o });
  return [
    a('unknown-action', 'nope', { json: {} }),
    a('bad-action-id', 'Dry_Run', { json: {} }),
    a('bad-action-chars', 'dry%20run', { json: {} }),
    a('unknown-system', 'restart', { system: 'bogus', json: { confirm: true } }),
    a('viewer-admin-action', 'dry_run_config', { json: {}, auth: 'viewer' }),
    a('no-confirm-disruptive', 'restart', { json: {} }),
    a('confirm-false-destructive', 'reset_stats', { json: { confirm: false } }),
    a('confirm-string', 'restart', { json: { confirm: 'yes' } }),
    a('confirm-string-true', 'dry_run_config', { json: { confirm: 'true' } }),
    a('params-not-object', 'dry_run_config', { json: { params: [] } }),
    a('unknown-param', 'dry_run_config', { json: { params: { x: 1 } } }),
    a('extra-field', 'dry_run_config', { json: { force: true } }),
    a('malformed', 'dry_run_config', { body: '{', contentType: 'application/json' }),
    a('text-plain', 'dry_run_config', { body: '{}', contentType: 'text/plain' }),
    a('no-body', 'dry_run_config', {}),
    a('null-body', 'dry_run_config', { body: 'null', contentType: 'application/json' }),
    a('int-param-wrong-type', 'sandbox_logs', { system: 'openshell', json: { params: { name: 'x', lines: '100' } } }),
    a('int-param-float', 'sandbox_logs', { system: 'openshell', json: { params: { name: 'x', lines: 2.5 } } }),
    a('int-param-integral-float', 'gateway_logs', { system: 'openshell', json: { params: { lines: 3.0 } } }),
    a('int-param-below-min', 'gateway_logs', { system: 'openshell', json: { params: { lines: 0 } } }),
    a('int-param-above-max', 'gateway_logs', { system: 'openshell', json: { params: { lines: 501 } } }),
    a('bool-param-int', 'prove_policy', { system: 'openshell', json: { params: { compact: 1 } } }),
    a('enum-param-bad', 'diag', { system: 'dcgm', json: { params: { gpu: '7' }, confirm: true } }),
    a('string-param-newline', 'sandbox_get', { system: 'openshell', json: { params: { name: 'a\nb' } } }),
    a('string-param-long', 'sandbox_get', { system: 'openshell', json: { params: { name: 'x'.repeat(1025) } } }),
    a('string-param-number', 'sandbox_get', { system: 'openshell', json: { params: { name: 5 } } }),
  ];
}

const ACT = 'POST /api/v1/systems/{system_id}/actions/{action}';
/** One action call. */
export const action = (system, actionId, params, o = {}) => ({
  id: `act.${system}.${actionId}${o.tag ? `.${o.tag}` : ''}`, route: ACT, method: 'POST', path: sys(system, `/actions/${actionId}`),
  json: { ...(params ? { params } : {}), ...(o.confirm ? { confirm: true } : {}) }, timeoutMs: o.timeoutMs ?? 240_000, ...(o.extra || {}),
});

/** Wait until no run.json under <state>/<system>/runs says "running" (background runs: aiperf profile, Data Designer
 *  create, Evaluator run, SkillEvaluator tier3), so their CLI calls land in this case, not the next one. */
export function waitRuns(system, timeoutMs = 60_000) {
  return async (side) => {
    const dir = join(side.vars.state, system, 'runs');
    const t0 = Date.now();
    while (Date.now() - t0 < timeoutMs) {
      let running = false;
      if (existsSync(dir)) {
        for (const r of readdirSync(dir)) {
          try { if (JSON.parse(readFileSync(join(dir, r, 'run.json'), 'utf8')).verdict === 'running') running = true; } catch { /* being written */ }
        }
      }
      if (!running) return;
      await new Promise((r) => setTimeout(r, 200));
    }
    side.notes.push(`${system}: a background run was still "running" after ${timeoutMs} ms`);
  };
}

/** Capture the newest run id of a system (a directory name under <state>/<system>/runs) into side.vars[name]. */
export function captureNewestRun(system, name) {
  return async (side) => {
    const dir = join(side.vars.state, system, 'runs');
    const runs = existsSync(dir) ? readdirSync(dir).sort() : [];
    if (runs.length) side.vars[name] = runs[runs.length - 1];
  };
}

/** Playground chat through each system that has one, stream and non-stream, plus validation and upstream errors. */
export function playgroundCases(tag = 'pg') {
  const C = 'POST /api/v1/systems/{system_id}/playground/chat';
  const msg = [{ role: 'user', content: 'ping' }];
  const p = (id, system, json, o = {}) => ({ id: `${tag}.${id}`, route: C, method: 'POST', path: sys(system, '/playground/chat'), json, ...o });
  return [
    p('switchyard.stream', 'switchyard', { model: 'auto-local', messages: msg }),
    p('switchyard.nonstream', 'switchyard', { model: 'auto-local', messages: msg, stream: false, max_tokens: 64 }),
    p('switchyard.system-prompt', 'switchyard', { model: 'direct-lightning', messages: msg, stream: false, system: 'be brief' }),
    p('switchyard.max-tokens-clamped', 'switchyard', { model: 'auto-local', messages: msg, stream: false, max_tokens: 100000 }),
    p('switchyard.content-list', 'switchyard', { model: 'auto-local', messages: [{ role: 'user', content: [{ type: 'text', text: 'ping' }] }], stream: false }),
    p('switchyard.upstream-error', 'switchyard', { model: 'parity-upstream-error', messages: msg, stream: false }),
    p('relay.nonstream', 'relay', { model: 'auto-local', messages: msg, stream: false }),
    p('relay.stream', 'relay', { model: 'auto-local', messages: msg }),
    p('dynamo.nonstream', 'dynamo', { model: 'unsloth/Qwen3.8-27B-NVFP4', messages: msg, stream: false }),
    p('guardrails.nonstream', 'guardrails', { model: 'aitop/nemotron-lightning', messages: msg, stream: false }),
    p('guardrails.stream', 'guardrails', { model: 'aitop/nemotron-lightning', messages: msg }),
    p('no-playground', 'skillevaluator', { model: 'x', messages: msg }),
    p('unknown-system', 'bogus', { model: 'x', messages: msg }),
    p('viewer', 'switchyard', { model: 'auto-local', messages: msg }, { auth: 'viewer' }),
    p('bad-model', 'switchyard', { model: 'bad model!', messages: msg }),
    p('empty-model', 'switchyard', { model: '', messages: msg }),
    p('no-messages', 'switchyard', { model: 'auto-local', messages: [] }),
    p('missing-messages', 'switchyard', { model: 'auto-local' }),
    p('too-many-messages', 'switchyard', { model: 'auto-local', messages: Array.from({ length: 201 }, () => ({ role: 'user', content: 'x' })) }),
    p('bad-role', 'switchyard', { model: 'auto-local', messages: [{ role: 'root', content: 'x' }] }),
    p('message-extra', 'switchyard', { model: 'auto-local', messages: [{ role: 'user', content: 'x', name: 'n' }] }),
    p('content-number', 'switchyard', { model: 'auto-local', messages: [{ role: 'user', content: 5 }] }),
    p('max-tokens-zero', 'switchyard', { model: 'auto-local', messages: msg, max_tokens: 0 }),
    p('max-tokens-float', 'switchyard', { model: 'auto-local', messages: msg, max_tokens: 1.5 }),
    p('stream-string', 'switchyard', { model: 'auto-local', messages: msg, stream: 'no' }),
    p('extra-field', 'switchyard', { model: 'auto-local', messages: msg, temperature: 0.2 }),
    p('system-too-long', 'switchyard', { model: 'auto-local', messages: msg, system: 'x'.repeat(100_001) }),
  ];
}

/** Data Designer run artifact routes (after preview / create ran). */
export function ddRunCases() {
  const RR = 'GET /api/v1/systems/{system_id}/runs/{run_id}/records';
  const RF = 'GET /api/v1/systems/{system_id}/runs/{run_id}/files';
  const dd = (sub) => sys('datadesigner', sub);
  return [
    { id: 'ddrun.preview.records', route: RR, path: dd('/runs/{{ddPreviewRun}}/records') },
    { id: 'ddrun.preview.records.page', route: RR, path: dd('/runs/{{ddPreviewRun}}/records?limit=1&offset=1') },
    { id: 'ddrun.preview.records.offset-past', route: RR, path: dd('/runs/{{ddPreviewRun}}/records?offset=50') },
    { id: 'ddrun.preview.records.viewer', route: RR, path: dd('/runs/{{ddPreviewRun}}/records'), auth: 'viewer' },
    { id: 'ddrun.preview.records.limit-0', route: RR, path: dd('/runs/{{ddPreviewRun}}/records?limit=0') },
    { id: 'ddrun.preview.records.limit-201', route: RR, path: dd('/runs/{{ddPreviewRun}}/records?limit=201') },
    { id: 'ddrun.preview.records.offset-neg', route: RR, path: dd('/runs/{{ddPreviewRun}}/records?offset=-1') },
    { id: 'ddrun.preview.files', route: RF, path: dd('/runs/{{ddPreviewRun}}/files') },
    { id: 'ddrun.create.records', route: RR, path: dd('/runs/{{ddCreateRun}}/records') },
    { id: 'ddrun.create.files', route: RF, path: dd('/runs/{{ddCreateRun}}/files') },
    { id: 'ddrun.unknown-run.records', route: RR, path: dd('/runs/20200101T000000-000-nope/records') },
    { id: 'ddrun.unknown-run.files', route: RF, path: dd('/runs/20200101T000000-000-nope/files') },
    { id: 'ddrun.bad-run-id.records', route: RR, path: dd('/runs/not-a-run/records') },
    { id: 'ddrun.bad-run-id.files', route: RF, path: dd('/runs/..%2F..%2Fetc/files') },
    { id: 'ddrun.other-system.records', route: RR, path: sys('aiperf', '/runs/20200101T000000-000-x/records') },
    { id: 'ddrun.other-system.files', route: RF, path: sys('relay', '/runs/20200101T000000-000-x/files') },
    { id: 'ddrun.unknown-system.records', route: RR, path: sys('bogus', '/runs/20200101T000000-000-x/records') },
    { id: 'ddrun.no-auth.files', route: RF, path: dd('/runs/{{ddPreviewRun}}/files'), auth: 'none' },
  ];
}
