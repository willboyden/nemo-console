// Fake upstream HTTP services: one small node:http server per external system the adapters call (config.py
// DEFAULT_URLS), on ephemeral 127.0.0.1 ports, answering with data from the test fixtures. Every backend gets its
// OWN set, so the requests each backend sent can be compared case by case. Deterministic answers only.
//
// Every request is logged as {server, method, path, query, headers (a fixed subset), body}. A path no handler knows
// answers 404 and is logged with `unknown: true`.
import http from 'node:http';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { FIXTURES } from '../lib/seed.mjs';

const fx = (...p) => readFileSync(join(FIXTURES, ...p), 'utf8');
// request headers worth comparing (not user-agent / accept-encoding / connection: HTTP-client details, not behaviour)
const LOGGED_HEADERS = ['content-type', 'authorization', 'x-api-key', 'anthropic-version', 'accept'];

const json = (res, status, doc, headers = {}) => {
  const body = typeof doc === 'string' ? doc : JSON.stringify(doc);
  res.writeHead(status, { 'content-type': 'application/json', 'content-length': Buffer.byteLength(body), ...headers });
  res.end(body);
};
const text = (res, status, body, type = 'text/plain; charset=utf-8', headers = {}) => {
  res.writeHead(status, { 'content-type': type, 'content-length': Buffer.byteLength(body), ...headers });
  res.end(body);
};

/** An OpenAI-shaped chat answer (non-stream JSON or an SSE stream), the same bytes every time. */
function chat(res, req, body, extraHeaders = {}) {
  let doc = {};
  try { doc = JSON.parse(body || '{}'); } catch { return json(res, 400, { error: { message: 'bad json' } }); }
  const model = typeof doc.model === 'string' ? doc.model : 'unknown';
  if (model === 'parity-upstream-error') return json(res, 500, { error: { message: 'upstream exploded (parity fake)' } });
  if (doc.stream) {
    res.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache', ...extraHeaders });
    const chunk = (delta, finish = null) => JSON.stringify({
      id: 'chatcmpl-parity', object: 'chat.completion.chunk', created: 1790000000, model,
      choices: [{ index: 0, delta, finish_reason: finish }] });
    res.write(`data: ${chunk({ role: 'assistant', content: '' })}\n\n`);
    res.write(`data: ${chunk({ content: 'po' })}\n\n`);
    res.write(`data: ${chunk({ content: 'ng' })}\n\n`);
    res.write(`data: ${chunk({}, 'stop')}\n\n`);
    res.end('data: [DONE]\n\n');
    return undefined;
  }
  return json(res, 200, {
    id: 'chatcmpl-parity', object: 'chat.completion', created: 1790000000, model,
    choices: [{ index: 0, message: { role: 'assistant', content: 'pong' }, finish_reason: 'stop' }],
    usage: { prompt_tokens: 7, completion_tokens: 1, total_tokens: 8 } }, extraHeaders);
}

// DCGM: Prometheus answers chosen by the query text (dcgm tests' QUERY_FIXTURES)
const PROM_QUERIES = [['up{', 'prom_up.json'], ['count by', 'prom_count.json'], ['max by', 'prom_tempmax.json'],
  ['sum by', 'prom_power.json']];

/** Handlers per server: (req, res, url, body) -> true when handled. */
const HANDLERS = {
  litellm(req, res, url, body) {
    if (req.method === 'GET' && url.pathname === '/health/liveliness') return json(res, 200, '"I\'m alive!"'), true;
    if (req.method === 'POST' && url.pathname === '/v1/chat/completions') return chat(res, req, body), true;
    return false;
  },
  switchyard(req, res, url, body) {
    if (req.method === 'GET' && url.pathname === '/health') return json(res, 200, fx('switchyard', 'health.json')), true;
    if (req.method === 'GET' && url.pathname === '/v1/models') return json(res, 200, fx('switchyard', 'models.json')), true;
    if (req.method === 'GET' && url.pathname === '/v1/stats') return json(res, 200, fx('switchyard', 'stats.json')), true;
    if (req.method === 'GET' && url.pathname === '/metrics') return text(res, 200, fx('switchyard', 'metrics.txt'), 'text/plain; version=0.0.4'), true;
    if (req.method === 'POST' && url.pathname === '/v1/stats/reset') return json(res, 200, { status: 'reset' }), true;
    if (req.method === 'POST' && url.pathname === '/v1/chat/completions') {
      let model = 'unknown';
      try { model = JSON.parse(body).model || model; } catch { /* chat() answers 400 */ }
      return chat(res, req, body, { 'x-switchyard-route': String(model), 'x-switchyard-tier': 'efficient' }), true;
    }
    return false;
  },
  relay(req, res, url, body) {
    // the gateway has no /health (404, as captured) and /v1/models wants a key (relay tests' conftest)
    if (req.method === 'GET' && url.pathname === '/health') return text(res, 404, 'Not Found'), true;
    if (req.method === 'GET' && url.pathname === '/v1/models') return json(res, 401, { error: { message: 'Authentication Error, No api key passed in.' } }), true;
    if (req.method === 'POST' && url.pathname === '/v1/chat/completions') return chat(res, req, body, { 'x-route': 'relay-default' }), true;
    return false;
  },
  dynamo(req, res, url, body, world) {
    // the frontend answers while its container runs (fake docker world); models only while a worker / mocker runs
    const up = (n) => world()?.containers?.[n]?.status === 'running';
    const fe = !world() || up('ai-lab-dynamo-dynamo-frontend-1');
    const wk = !world() || up('ai-lab-dynamo-dynamo-worker-1') || up('ai-lab-dynamo-dynamo-mocker-1');
    if (req.method === 'GET' && url.pathname === '/health') return (fe ? json(res, 200, fx('dynamo', 'frontend_health.json')) : json(res, 503, { status: 'unavailable' })), true;
    if (req.method === 'GET' && url.pathname === '/v1/models') return json(res, 200, wk ? fx('dynamo', 'frontend_models.json') : '{"object":"list","data":[]}'), true;
    if (req.method === 'GET' && url.pathname === '/metrics') return text(res, 200, fx('dynamo', 'frontend_metrics.prom'), 'text/plain; version=0.0.4'), true;
    if (req.method === 'POST' && url.pathname === '/v1/chat/completions') return chat(res, req, body), true;
    return false;
  },
  dynamo_worker_metrics(req, res, url) {
    if (req.method === 'GET' && url.pathname === '/metrics') return text(res, 200, fx('dynamo', 'worker_metrics.prom'), 'text/plain; version=0.0.4'), true;
    if (req.method === 'GET' && url.pathname === '/health') return json(res, 200, fx('dynamo', 'worker_health.json')), true;
    return false;
  },
  dcgm(req, res, url) {
    if (req.method === 'GET' && url.pathname === '/metrics') return text(res, 200, fx('dcgm', 'metrics.txt'), 'text/plain; version=0.0.4'), true;
    if (req.method === 'GET' && url.pathname === '/debug/pprof/') return text(res, 404, '404 page not found\n'), true;
    return false;
  },
  prometheus(req, res, url) {
    if (req.method === 'GET' && url.pathname === '/api/v1/query') {
      const q = url.searchParams.get('query') || '';
      const hit = PROM_QUERIES.find(([needle]) => q.includes(needle));
      if (hit) return json(res, 200, fx('dcgm', hit[1])), true;
      return json(res, 400, { status: 'error', errorType: 'bad_data', error: 'unknown query (parity fake)' }), true;
    }
    return false;
  },
  guardrails(req, res, url, body) {
    if (req.method === 'GET' && url.pathname === '/v1/health') return json(res, 200, fx('guardrails', 'health.json')), true;
    if (req.method === 'GET' && url.pathname === '/v1/rails/configs') return json(res, 200, fx('guardrails', 'configs.json')), true;
    if (req.method === 'POST' && url.pathname === '/v1/checks') {
      let prompt = '';
      try { prompt = JSON.stringify(JSON.parse(body).messages || []); } catch { /* empty */ }
      if (prompt.includes('capital of France')) return json(res, 200, { status: 'passed', content: 'What is the capital of France?' }), true;
      return json(res, 200, { status: 'blocked', rail: 'self check input', content: "I'm sorry, I can't respond to that." }), true;
    }
    if (req.method === 'POST' && url.pathname === '/v1/chat/completions') return chat(res, req, body), true;
    return false;
  },
};

export const UPSTREAM_NAMES = Object.keys(HANDLERS);

function startOne(name, log, world) {
  const server = http.createServer((req, res) => {
    const chunks = [];
    req.on('data', (c) => chunks.push(c));
    req.on('end', () => {
      const body = Buffer.concat(chunks).toString('utf8');
      const url = new URL(req.url, 'http://fake');
      // `accept` only where the console sets it itself (the playground POST); elsewhere it is the HTTP client's default
      const headers = Object.fromEntries(LOGGED_HEADERS.filter((h) => req.headers[h] !== undefined && (h !== 'accept' || req.method === 'POST'))
        .map((h) => [h, req.headers[h]]));
      const rec = { server: name, method: req.method, path: url.pathname, query: url.search, headers, ...(body ? { body } : {}) };
      let handled = false;
      try { handled = HANDLERS[name](req, res, url, body, world); } catch (e) { rec.error = e.message; }
      if (!handled) {
        rec.unknown = true;
        if (!res.headersSent) json(res, 404, { detail: `parity fake ${name}: no route ${req.method} ${url.pathname}` });
      }
      log.push(rec);
    });
  });
  server.keepAliveTimeout = 1000;
  return new Promise((resolve) => server.listen(0, '127.0.0.1', () => resolve(server)));
}

/** Start one fake of every upstream for one backend. `worldFile`: the fake machine state the fake CLIs keep (the
 *  Dynamo frontend follows its containers). Returns {urls, ports, log, close()}. */
export async function startUpstreams({ worldFile } = {}) {
  const log = [];
  const servers = {};
  const world = () => { if (!worldFile) return null; try { return JSON.parse(readFileSync(worldFile, 'utf8')); } catch { return null; } };
  for (const name of UPSTREAM_NAMES) servers[name] = await startOne(name, log, world);
  const urls = Object.fromEntries(Object.entries(servers).map(([n, s]) => [n, `http://127.0.0.1:${s.address().port}`]));
  return {
    urls,
    ports: Object.fromEntries(Object.entries(servers).map(([n, s]) => [n, s.address().port])),
    log,
    close: () => Promise.all(Object.values(servers).map((s) => new Promise((r) => { s.closeAllConnections?.(); s.close(() => r()); }))),
  };
}
