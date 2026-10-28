// Core cases: security middleware (Host / Origin / CSRF / auth / body cap / methods / trailing slashes), the static
// frontend, and the system-independent API routes. Case fields: see corpus/index.mjs.
import { readFileSync } from 'node:fs';
const API = '/api/v1';

/** Security middleware and auth, on a mix of open, viewer and admin routes. */
export function securityCases() {
  const c = [];
  const R = (route, o) => c.push({ route, ...o });
  // health is open: the caller's role is reported (public / viewer / admin), a bad key stays public
  R('GET /api/v1/health', { id: 'sec.health.public', path: `${API}/health`, auth: 'none' });
  R('GET /api/v1/health', { id: 'sec.health.admin', path: `${API}/health` });
  R('GET /api/v1/health', { id: 'sec.health.viewer', path: `${API}/health`, auth: 'viewer' });
  R('GET /api/v1/health', { id: 'sec.health.bad-key', path: `${API}/health`, auth: 'bad' });
  R('GET /api/v1/health', { id: 'sec.health.basic-scheme', path: `${API}/health`, auth: 'raw:Basic dXNlcjpwYXNz' });
  R('GET /api/v1/health', { id: 'sec.health.query-ignored', path: `${API}/health?x=1&y` });
  // Host allowlist (DNS rebinding): 421 before auth
  R('GET /api/v1/health', { id: 'sec.host.evil', path: `${API}/health`, host: 'evil.example:{{port}}' });
  R('GET /api/v1/systems', { id: 'sec.host.evil.guarded', path: `${API}/systems`, host: 'evil.example' });
  R('GET /api/v1/systems', { id: 'sec.host.no-port', path: `${API}/systems`, host: '127.0.0.1' });
  R('GET /api/v1/systems', { id: 'sec.host.localhost', path: `${API}/systems`, host: 'localhost:{{port}}' });
  R('GET /api/v1/systems', { id: 'sec.host.localhost-upper', path: `${API}/systems`, host: 'LOCALHOST:{{port}}' });
  R('GET /api/v1/systems', { id: 'sec.host.ipv6', path: `${API}/systems`, host: '[::1]:{{port}}' });
  R('GET /api/v1/systems', { id: 'sec.host.missing', path: `${API}/systems`, host: null });
  R('GET /api/v1/systems', { id: 'sec.host.wrong-port', path: `${API}/systems`, host: '127.0.0.1:1' });
  R('GET /{path:path}', { id: 'sec.host.evil.static', path: '/', host: 'evil.example', auth: 'none' });
  R('POST /api/v1/systems/{system_id}/actions/{action}', { id: 'sec.host.evil.post', method: 'POST', path: `${API}/systems/switchyard/actions/dry_run_config`, host: 'evil.example', json: {} });
  // auth: 401 without / with a wrong key, 403 for a viewer on admin routes
  R('GET /api/v1/systems', { id: 'sec.auth.none', path: `${API}/systems`, auth: 'none' });
  R('GET /api/v1/systems', { id: 'sec.auth.bad', path: `${API}/systems`, auth: 'bad' });
  R('GET /api/v1/systems', { id: 'sec.auth.empty-bearer', path: `${API}/systems`, auth: 'raw:Bearer ' });
  R('GET /api/v1/systems', { id: 'sec.auth.token-scheme', path: `${API}/systems`, auth: 'raw:Token {{port}}' });
  R('GET /api/v1/systems', { id: 'sec.auth.lowercase-bearer', path: `${API}/systems`, auth: 'raw:bearer {{adminKeyRaw}}' });
  R('GET /api/v1/systems', { id: 'sec.auth.viewer', path: `${API}/systems`, auth: 'viewer' });
  R('GET /api/v1/audit', { id: 'sec.auth.viewer-on-admin', path: `${API}/audit`, auth: 'viewer' });
  R('GET /api/v1/audit', { id: 'sec.auth.none-on-admin', path: `${API}/audit`, auth: 'none' });
  R('GET /metrics', { id: 'sec.metrics.none', path: '/metrics', auth: 'none' });
  R('GET /api/v1/openapi.json', { id: 'sec.openapi.none', path: `${API}/openapi.json`, auth: 'none' });
  R('HEAD /api/v1/openapi.json', { id: 'sec.openapi.head.none', method: 'HEAD', path: `${API}/openapi.json`, auth: 'none' });
  R('GET /api/v1/health', { id: 'sec.auth.forwarded-header', path: `${API}/health`, auth: 'none', headers: { 'X-Forwarded-For': '127.0.0.1' } });
  // CSRF: every non-GET needs X-Console: 1 (also with a valid bearer); Origin must match Host
  const post = `${API}/systems/switchyard/actions/dry_run_config`;
  const route = 'POST /api/v1/systems/{system_id}/actions/{action}';
  R(route, { id: 'sec.csrf.missing', method: 'POST', path: post, csrf: false, json: {} });
  R(route, { id: 'sec.csrf.zero', method: 'POST', path: post, csrf: false, headers: { 'X-Console': '0' }, json: {} });
  R(route, { id: 'sec.csrf.missing.no-auth', method: 'POST', path: post, csrf: false, auth: 'none', json: {} });
  R(route, { id: 'sec.origin.cross', method: 'POST', path: post, headers: { Origin: 'http://evil.example' }, json: {} });
  R(route, { id: 'sec.origin.null', method: 'POST', path: post, headers: { Origin: 'null' }, json: {} });
  R(route, { id: 'sec.origin.other-port', method: 'POST', path: post, headers: { Origin: 'http://127.0.0.1:1' }, json: {} });
  R(route, { id: 'sec.origin.same', method: 'POST', path: post, headers: { Origin: '{{origin}}' }, json: {}, auth: 'viewer' });
  R('GET /api/v1/systems', { id: 'sec.origin.cross-on-get', path: `${API}/systems`, headers: { Origin: 'http://evil.example' } });
  R(route, { id: 'sec.auth.none.post', method: 'POST', path: post, auth: 'none', json: {} });
  // body cap 1 MiB: declared length and chunked upload
  const big = 'x'.repeat(1024 * 1024 + 10);
  R('PUT /api/v1/settings', { id: 'sec.body.content-length-over-cap', method: 'PUT', path: `${API}/settings`, body: big, contentType: 'application/json' });
  R('PUT /api/v1/settings', { id: 'sec.body.chunked-over-cap', method: 'PUT', path: `${API}/settings`, body: big, contentType: 'application/json', chunked: true });
  R('PUT /api/v1/settings', { id: 'sec.body.over-cap.no-auth', method: 'PUT', path: `${API}/settings`, body: big, contentType: 'application/json', auth: 'none' });
  R('PUT /api/v1/settings', { id: 'sec.body.chunked-small', method: 'PUT', path: `${API}/settings`, body: '{}', contentType: 'application/json', chunked: true });
  // json.loads limits (A-6): nesting past the recursion limit and ints over 4300 digits are "an error parsing the
  // body" (400); a body nested 500 deep (under Rust's 512, D-12) parses and fails validation like any other
  R('PUT /api/v1/settings', { id: 'sec.body.json-too-deep', method: 'PUT', path: `${API}/settings`, body: '['.repeat(20000) + ']'.repeat(20000), contentType: 'application/json' });
  // the exact body that aborted the Rust process before A-6 (workstream C's report): 200,000 nested arrays, about 400 KB
  R('PUT /api/v1/settings', { id: 'sec.body.json-attack-400k', method: 'PUT', path: `${API}/settings`, body: '['.repeat(200000) + ']'.repeat(200000), contentType: 'application/json' });
  R('POST /api/v1/systems/{system_id}/actions/{action}', { id: 'sec.body.json-attack-400k.action', method: 'POST', path: `${API}/systems/datadesigner/actions/preview`, body: '['.repeat(200000) + ']'.repeat(200000), contentType: 'application/json' });
  R('PUT /api/v1/settings', { id: 'sec.body.json-deep-ok', method: 'PUT', path: `${API}/settings`, body: '['.repeat(500) + ']'.repeat(500), contentType: 'application/json' });
  R('PUT /api/v1/settings', { id: 'sec.body.json-deep-object-ok', method: 'PUT', path: `${API}/settings`, body: '{"a":'.repeat(500) + '1' + '}'.repeat(500), contentType: 'application/json' });
  R('PUT /api/v1/settings', { id: 'sec.body.json-deep-in-field', method: 'PUT', path: `${API}/settings`, body: '{"urls":' + '{"a":'.repeat(499) + '1' + '}'.repeat(500), contentType: 'application/json' });
  R('PUT /api/v1/settings', { id: 'sec.body.json-int-4301-digits', method: 'PUT', path: `${API}/settings`, body: `{"poll_interval_s": ${'1'.repeat(4301)}}`, contentType: 'application/json' });
  // methods: HEAD / OPTIONS / unknown, on API and static routes
  R('GET /api/v1/systems', { id: 'sec.method.head-api', method: 'HEAD', path: `${API}/systems` });
  R('GET /api/v1/systems', { id: 'sec.method.options-api', method: 'OPTIONS', path: `${API}/systems` });
  R('GET /api/v1/systems', { id: 'sec.method.delete-api', method: 'DELETE', path: `${API}/systems` });
  R('GET /api/v1/systems', { id: 'sec.method.post-on-get-route', method: 'POST', path: `${API}/systems`, json: {} });
  R('PUT /api/v1/settings', { id: 'sec.method.patch-settings', method: 'PATCH', path: `${API}/settings`, json: {} });
  R('GET /api/v1/systems', { id: 'sec.method.unknown', method: 'PURGE', path: `${API}/systems` });
  R('GET /api/v1/health', { id: 'sec.method.head-health', method: 'HEAD', path: `${API}/health`, auth: 'none' });
  R('GET /{path:path}', { id: 'sec.method.head-static', method: 'HEAD', path: '/', auth: 'none' });
  R('GET /{path:path}', { id: 'sec.method.options-static', method: 'OPTIONS', path: '/index.html', auth: 'none' });
  R('GET /{path:path}', { id: 'sec.method.post-static', method: 'POST', path: '/index.html', auth: 'none', json: {} });
  R('GET /metrics', { id: 'sec.method.post-metrics', method: 'POST', path: '/metrics', json: {} });
  // trailing slashes and unknown paths under the guarded prefixes
  R('GET /api/v1/systems', { id: 'sec.slash.systems', path: `${API}/systems/` });
  R('GET /api/v1/health', { id: 'sec.slash.health', path: `${API}/health/`, auth: 'none' });
  R('GET /api/v1/systems/{system_id}/status', { id: 'sec.slash.status', path: `${API}/systems/switchyard/status/` });
  R('PUT /api/v1/settings', { id: 'sec.slash.settings-put', method: 'PUT', path: `${API}/settings/`, json: {} });
  R('GET /metrics', { id: 'sec.slash.metrics', path: '/metrics/' });
  R('GET /{path:path}', { id: 'sec.unknown.api', path: `${API}/nope` });
  R('GET /{path:path}', { id: 'sec.unknown.api.no-auth', path: `${API}/nope`, auth: 'none' });
  R('GET /{path:path}', { id: 'sec.unknown.api-root', path: '/api' });
  R('GET /{path:path}', { id: 'sec.unknown.api-v2', path: '/api/v2/systems' });
  R('GET /{path:path}', { id: 'sec.unknown.post-api', method: 'POST', path: `${API}/nope`, json: {} });
  R('GET /{path:path}', { id: 'sec.docs', path: '/docs', auth: 'none' });
  R('GET /{path:path}', { id: 'sec.redoc', path: '/redoc', auth: 'none' });
  R('GET /{path:path}', { id: 'sec.openapi-root', path: '/openapi.json', auth: 'none' });
  return c;
}

/** The static frontend: allowlist, traversal, ETag / 304. */
export function staticCases() {
  const c = [];
  const S = (o) => c.push({ route: 'GET /{path:path}', auth: 'none', ...o });
  S({ id: 'static.root', path: '/', capture: { etagIndex: { header: 'etag' } } });
  S({ id: 'static.index', path: '/index.html' });
  S({ id: 'static.favicon-svg', path: '/favicon.svg' });
  S({ id: 'static.favicon-ico-missing', path: '/favicon.ico' });
  S({ id: 'static.js-app', path: '/js/app.js', capture: { etagApp: { header: 'etag' } } });
  S({ id: 'static.js-nested', path: '/js/views/overview.js' });
  S({ id: 'static.css', path: '/css/base.css' });
  S({ id: 'static.i18n', path: '/i18n/en.json' });
  S({ id: 'static.with-bearer', path: '/index.html', auth: 'admin' });
  S({ id: 'static.query', path: '/index.html?v=1' });
  S({ id: 'static.js-dir', path: '/js/' });
  S({ id: 'static.js-bare', path: '/js' });
  S({ id: 'static.dev-dir', path: '/dev/mock-server.mjs' });
  S({ id: 'static.tests-dir', path: '/tests/api.test.mjs' });
  S({ id: 'static.package-json', path: '/package.json' });
  S({ id: 'static.unknown', path: '/nope.html' });
  S({ id: 'static.spa-route', path: '/systems/switchyard' });
  S({ id: 'static.traversal.dotdot', path: '/js/../index.html' });
  S({ id: 'static.traversal.dotdot-root', path: '/../backend/pyproject.toml' });
  S({ id: 'static.traversal.encoded', path: '/js/%2e%2e/index.html' });
  S({ id: 'static.traversal.encoded-slash', path: '/js/..%2findex.html' });
  S({ id: 'static.traversal.double-encoded', path: '/js/%252e%252e/index.html' });
  S({ id: 'static.traversal.backslash', path: '/js/..%5cindex.html' });
  S({ id: 'static.traversal.double-slash', path: '/js//app.js' });
  S({ id: 'static.traversal.dot-segment', path: '/js/./app.js' });
  S({ id: 'static.traversal.nul', path: '/js/app.js%00.css' });
  S({ id: 'static.traversal.into-backend', path: '/js/../../backend/src/nemo_console/config.py' });
  S({ id: 'static.etag.match', path: '/', headers: { 'If-None-Match': '{{etagIndex}}' } });
  S({ id: 'static.etag.weak-match', path: '/', headers: { 'If-None-Match': 'W/{{etagIndex}}' } });
  S({ id: 'static.etag.list-match', path: '/js/app.js', headers: { 'If-None-Match': '"nope", {{etagApp}}' } });
  S({ id: 'static.etag.no-match', path: '/', headers: { 'If-None-Match': '"0000"' } });
  S({ id: 'static.etag.star', path: '/', headers: { 'If-None-Match': '*' } });
  S({ id: 'static.etag.other-file', path: '/index.html', headers: { 'If-None-Match': '{{etagApp}}' } });
  S({ id: 'static.if-modified-since', path: '/', headers: { 'If-Modified-Since': 'Wed, 21 Oct 2099 07:28:00 GMT' } });
  S({ id: 'static.range', path: '/js/app.js', headers: { Range: 'bytes=0-9' } });
  return c;
}

/** System-independent API routes (read side). */
export function globalReadCases() {
  const c = [];
  const R = (route, o) => c.push({ route, ...o });
  R('GET /api/v1/overview', { id: 'global.overview', path: `${API}/overview` });
  R('GET /api/v1/overview', { id: 'global.overview.viewer', path: `${API}/overview`, auth: 'viewer' });
  R('GET /api/v1/topology', { id: 'global.topology', path: `${API}/topology` });
  R('GET /api/v1/topology', { id: 'global.topology.viewer', path: `${API}/topology`, auth: 'viewer' });
  R('GET /api/v1/systems', { id: 'global.systems', path: `${API}/systems` });
  R('GET /api/v1/health', { id: 'global.health.after', path: `${API}/health`, auth: 'none' });
  R('GET /api/v1/settings', { id: 'global.settings.admin', path: `${API}/settings` });
  R('GET /api/v1/settings', { id: 'global.settings.viewer', path: `${API}/settings`, auth: 'viewer' });
  for (const w of ['15m', '1h', '24h', '7d']) R('GET /api/v1/routing/summary', { id: `global.routing.${w}`, path: `${API}/routing/summary?window=${w}` });
  R('GET /api/v1/routing/summary', { id: 'global.routing.default', path: `${API}/routing/summary`, auth: 'viewer' });
  R('GET /api/v1/routing/summary', { id: 'global.routing.bad-window', path: `${API}/routing/summary?window=2h` });
  R('GET /api/v1/routing/summary', { id: 'global.routing.empty-window', path: `${API}/routing/summary?window=` });
  R('GET /api/v1/routing/summary', { id: 'global.routing.repeated', path: `${API}/routing/summary?window=1h&window=bad` });
  R('GET /api/v1/audit', { id: 'global.audit.empty-or-not', path: `${API}/audit?limit=5` });
  R('GET /api/v1/openapi.json', { id: 'global.openapi.admin', path: `${API}/openapi.json` });
  R('GET /api/v1/openapi.json', { id: 'global.openapi.viewer', path: `${API}/openapi.json`, auth: 'viewer' });
  R('HEAD /api/v1/openapi.json', { id: 'global.openapi.head', method: 'HEAD', path: `${API}/openapi.json` });
  return c;
}

/** Audit list: limits, system filter, validation (run after the mutations so it has rows). */
export function auditCases(tag) {
  const c = [];
  const R = (o) => c.push({ route: 'GET /api/v1/audit', ...o });
  R({ id: `audit.${tag}.default`, path: `${API}/audit` });
  R({ id: `audit.${tag}.limit-1`, path: `${API}/audit?limit=1` });
  R({ id: `audit.${tag}.limit-500`, path: `${API}/audit?limit=500` });
  R({ id: `audit.${tag}.system`, path: `${API}/audit?system=switchyard` });
  R({ id: `audit.${tag}.system-dynamo`, path: `${API}/audit?system=dynamo&limit=3` });
  R({ id: `audit.${tag}.limit-0`, path: `${API}/audit?limit=0` });
  R({ id: `audit.${tag}.limit-501`, path: `${API}/audit?limit=501` });
  R({ id: `audit.${tag}.limit-abc`, path: `${API}/audit?limit=abc` });
  R({ id: `audit.${tag}.limit-float`, path: `${API}/audit?limit=2.5` });
  R({ id: `audit.${tag}.system-bogus`, path: `${API}/audit?system=bogus` });
  R({ id: `audit.${tag}.system-empty`, path: `${API}/audit?system=` });
  return c;
}

/** Settings: view, valid and invalid updates (422 shapes), viewer refused. */
export function settingsCases() {
  const c = [];
  const P = (o) => c.push({ route: 'PUT /api/v1/settings', method: 'PUT', path: `${API}/settings`, ...o });
  P({ id: 'settings.put.empty', json: {} });
  P({ id: 'settings.put.poll', json: { poll_interval_s: 5 } });
  P({ id: 'settings.put.poll-float', json: { poll_interval_s: 2.5 } });
  P({ id: 'settings.put.poll-low', json: { poll_interval_s: 0.1 } });
  P({ id: 'settings.put.poll-high', json: { poll_interval_s: 61 } });
  P({ id: 'settings.put.poll-string', json: { poll_interval_s: 'fast' } });
  P({ id: 'settings.put.poll-numeric-string', json: { poll_interval_s: '3' } });
  P({ id: 'settings.put.poll-bool', json: { poll_interval_s: true } });
  P({ id: 'settings.put.poll-null', json: { poll_interval_s: null } });
  P({ id: 'settings.put.url-loopback', json: { urls: { litellm: 'http://127.0.0.1:4999/' } } });
  P({ id: 'settings.put.url-localhost-path', json: { urls: { relay: 'http://localhost:4041/base' } } });
  P({ id: 'settings.put.url-remote', json: { urls: { litellm: 'http://10.0.0.1:4000' } } });
  P({ id: 'settings.put.url-https', json: { urls: { litellm: 'https://127.0.0.1:4000' } } });
  P({ id: 'settings.put.url-userinfo', json: { urls: { litellm: 'http://user:pw@127.0.0.1:4000' } } });
  P({ id: 'settings.put.url-query', json: { urls: { litellm: 'http://127.0.0.1:4000/?a=1' } } });
  P({ id: 'settings.put.url-bad-port', json: { urls: { litellm: 'http://127.0.0.1:99999' } } });
  P({ id: 'settings.put.url-unknown-name', json: { urls: { nope: 'http://127.0.0.1:4000' } } });
  P({ id: 'settings.put.url-not-string', json: { urls: { litellm: 4000 } } });
  P({ id: 'settings.put.urls-list', json: { urls: ['http://127.0.0.1:4000'] } });
  P({ id: 'settings.put.key-env-ok', json: { playground_key_env: { relay: 'NEMO_CONSOLE_PLAYGROUND_RELAY' } } });
  P({ id: 'settings.put.key-env-api-key', json: { playground_key_env: { dynamo: 'DYNAMO_API_KEY' } } });
  P({ id: 'settings.put.key-env-value', json: { playground_key_env: { relay: 'sk-literal-secret-value-123' } } });
  P({ id: 'settings.put.key-env-disallowed', json: { playground_key_env: { relay: 'AWS_SECRET_ACCESS_KEY' } } });
  P({ id: 'settings.put.key-env-bad-name', json: { playground_key_env: { 'Bad-Name': 'LITELLM_API_KEY' } } });
  P({ id: 'settings.put.extra-field', json: { trust_loopback: true } });
  P({ id: 'settings.put.array-body', json: [] });
  P({ id: 'settings.put.malformed', body: '{"poll_interval_s": ', contentType: 'application/json' });
  P({ id: 'settings.put.text-plain', body: '{"poll_interval_s": 3}', contentType: 'text/plain' });
  P({ id: 'settings.put.no-content-type', body: '{"poll_interval_s": 3}' });
  P({ id: 'settings.put.empty-body', body: '', contentType: 'application/json' });
  P({ id: 'settings.put.viewer', json: { poll_interval_s: 3 }, auth: 'viewer' });
  P({ id: 'settings.put.no-auth', json: { poll_interval_s: 3 }, auth: 'none' });
  c.push({ id: 'settings.get.after', route: 'GET /api/v1/settings', path: `${API}/settings` });
  c.push({ id: 'settings.get.after.viewer', route: 'GET /api/v1/settings', path: `${API}/settings`, auth: 'viewer' });
  return c;
}

/** The console's own /metrics (run last: it counts every request of the run). */
export function metricsCases() {
  return [
    { id: 'metrics.viewer', route: 'GET /metrics', path: '/metrics', auth: 'viewer' },
    { id: 'metrics.admin', route: 'GET /metrics', path: '/metrics' },
  ];
}

/** The CLI entry point: mint-key (the launcher scripts parse its output and exit code). */
export function cliCases() {
  return [
    { id: 'cli.mint-key.admin', route: 'CLI mint-key', kind: 'cli', args: ['mint-key', 'admin'] },
    { id: 'cli.mint-key.default', route: 'CLI mint-key', kind: 'cli', args: ['mint-key'] },
    { id: 'cli.mint-key.bogus', route: 'CLI mint-key', kind: 'cli', args: ['mint-key', 'root'] },
  ];
}

/**
 * Role sweep (audit finding 1): every guarded route of the Python route table, called without a key and, for admin
 * routes, as a viewer, with a valid `{}` body where the route takes one. Both backends must refuse with the same
 * status and body (401 / 403 after the system and id checks), never run the handler (no 200 / 422).
 */
export function roleSweepCases() {
  const table = JSON.parse(readFileSync(new URL('../routes.python.json', import.meta.url), 'utf8')).routes;
  const fill = { system_id: 'switchyard', file_id: 'routes', action: 'dry_run_config', run_id: 'r1' };
  const c = [];
  for (const r of table) {
    if (r.auth !== 'viewer' && r.auth !== 'admin') continue;
    const path = r.path.replace(/\{([a-z_]+)\}/g, (_, k) => fill[k]);
    const route = `${r.method} ${r.path}`;
    const base = { route, method: r.method, path, ...(r.method === 'GET' || r.method === 'HEAD' ? {} : { json: {} }) };
    if (path.endsWith('/events/stream')) base.readMs = 500;
    const slug = `${r.method.toLowerCase()}.${r.path.replace(/^\/api\/v1\//, '').replace(/[{}]/g, '').replace(/\//g, '.')}`;
    c.push({ ...base, id: `roles.none.${slug}`, auth: 'none' });
    if (r.auth === 'admin') c.push({ ...base, id: `roles.viewer.${slug}`, auth: 'viewer' });
  }
  return c;
}
