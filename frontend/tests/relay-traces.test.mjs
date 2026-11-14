// The Relay trace explorer's non-geometry logic: API client routes, the pure helpers, the message catalogue and the
// rules every explorer file must obey (text only, no markup from data, no inline style).
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { createClient } from '../js/api.js';
import { setMessages } from '../js/i18n.js';
import { normalizeFacet, buildProgress, coverage, contentProblem, trajectoryProblem, prettyArgs, describeLevel, traceEndpoints } from '../js/rt/pure.js';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const en = JSON.parse(fs.readFileSync(path.join(root, 'i18n/en.json'), 'utf8'));
setMessages(en);
const walk = (d) => fs.readdirSync(d, { withFileTypes: true }).flatMap((e) => (e.isDirectory() ? walk(path.join(d, e.name)) : e.name.endsWith('.js') ? [path.join(d, e.name)] : []));
const files = [...walk(path.join(root, 'js/rt')), ...walk(path.join(root, 'js/viz'))];
const res = (status, body) => ({ ok: status < 400, status, headers: { get: () => 'application/json' }, json: async () => body, text: async () => '' });

test('api: every 9.12 route is one GET under /systems/relay, ids are encoded, the signal is passed, no CSRF header on GETs', async () => {
  const calls = [];
  const api = createClient({
    fetchImpl: async (u, o) => {
      calls.push({ u, o });
      return res(200, {});
    },
  });
  const sig = new AbortController().signal;
  await api.traceIndex({ signal: sig });
  await api.traces({ window: '7d', q: 'abc', limit: 50, offset: '', min_ms: null });
  await api.trace('11111111-2222-7333-8444-555555555555');
  await api.spanContent('t-1', 's-1', { max_chars: 200000, messages: 'head' });
  await api.sessions({ sort: 'last' });
  await api.session('ses/with slash');
  await api.analytics({ window: 'all', anchor: 'now' });
  await api.trajectories({ source: 'gateway' });
  await api.trajectories({ trace: '11111111-2222-7333-8444-555555555555', limit: 1 });
  await api.trajectory('tj-1', { source: 'example-agent' });
  await api.stepContent('tj-1', 7, { source: 'example-agent' });
  await api.capture();
  assert.deepEqual(
    calls.map((c) => c.u),
    [
      '/api/v1/systems/relay/traces/index',
      '/api/v1/systems/relay/traces?window=7d&q=abc&limit=50',
      '/api/v1/systems/relay/traces/11111111-2222-7333-8444-555555555555',
      '/api/v1/systems/relay/traces/t-1/spans/s-1/content?max_chars=200000&messages=head',
      '/api/v1/systems/relay/sessions?sort=last',
      '/api/v1/systems/relay/sessions/ses%2Fwith%20slash',
      '/api/v1/systems/relay/analytics?window=all&anchor=now',
      '/api/v1/systems/relay/trajectories?source=gateway',
      '/api/v1/systems/relay/trajectories?trace=11111111-2222-7333-8444-555555555555&limit=1',
      '/api/v1/systems/relay/trajectories/tj-1?source=example-agent',
      '/api/v1/systems/relay/trajectories/tj-1/steps/7/content?source=example-agent',
      '/api/v1/systems/relay/capture',
    ],
  );
  assert.ok(calls.every((c) => c.o.method === 'GET' && c.o.headers['X-Console'] === undefined));
  assert.equal(calls[0].o.signal, sig);
});

test('facets are read from lists of objects, plain lists and count maps', () => {
  assert.deepEqual(normalizeFacet([{ value: 'a', count: 3 }, { name: 'b' }, 'c']), [
    { value: 'a', count: 3 },
    { value: 'b', count: null },
    { value: 'c', count: null },
  ]);
  assert.deepEqual(normalizeFacet({ x: 2, y: 'n/a' }), [
    { value: 'x', count: 2 },
    { value: 'y', count: null },
  ]);
  assert.deepEqual(normalizeFacet([{ value: null, count: 4 }]), [{ value: null, count: 4 }], 'a null value (not set) is kept');
  assert.deepEqual(normalizeFacet(null), []);
  assert.deepEqual(normalizeFacet(7), []);
});

test('index progress is bytes read over bytes present; capture coverage counts levels', () => {
  assert.equal(buildProgress({ files: [] }), null);
  assert.equal(buildProgress(undefined), null);
  assert.equal(
    buildProgress({
      files: [
        { bytes: 100, scanned_bytes: 100 },
        { bytes: 300, scanned_bytes: 0 },
      ],
    }),
    0.25,
  );
  assert.equal(buildProgress({ files: [{ bytes: 10, scanned_bytes: 99 }] }), 1, 'never above 1');
  const c = coverage([
    { enabled: true, state: 'active' },
    { enabled: true, state: 'unknown' },
    { enabled: false, state: 'off' },
  ]);
  assert.deepEqual(c, { on: 2, total: 3, by: { active: 1, unknown: 1, off: 1 } });
});

test('a failed content request is explained in plain words, by status and code', () => {
  const e = (status, code, message = 'x') => Object.assign(new Error(message), { status, code });
  assert.match(contentProblem(e(403, 'content_disabled')).text, /NEMO_CONSOLE_RELAY_TRACE_CONTENT=off.*NEMO_CONSOLE_RELAY_TRACE_CONTENT=admin/);
  assert.match(contentProblem(e(403, 'forbidden')).title, /admin key/);
  assert.match(contentProblem(e(409, 'content_unavailable')).title, /gone/);
  assert.match(contentProblem(e(404, 'not_found')).text, /does not know/);
  assert.equal(contentProblem(e(403, 'forbidden')).kind, 'warn');
  assert.equal(contentProblem(e(503, 'unavailable')).kind, 'bad');
  assert.match(contentProblem(e(0, 'network')).text, /Cannot reach/);
  assert.equal(contentProblem(null).kind, 'bad');
});

test('trajectoryProblem: 409 says the file changed, not that the id is bad', () => {
  const e = (status, code) => ({ status, code, message: 'x' });
  assert.match(trajectoryProblem(e(409, 'content_unavailable')).title, /changed/);
  assert.match(trajectoryProblem(e(409, 'content_unavailable')).text, /few seconds/);
  assert.doesNotMatch(trajectoryProblem(e(409, 'content_unavailable')).title, /not a trajectory id/);
  assert.match(trajectoryProblem(e(404, 'not_found')).title, /not found/);
  assert.match(trajectoryProblem(e(422, 'invalid_request')).title, /not a trajectory id/);
  assert.equal(trajectoryProblem(e(503, 'unavailable')), null);
  assert.equal(trajectoryProblem(null), null);
});

test('tool arguments are pretty-printed only when they are JSON', () => {
  assert.equal(prettyArgs('{"a":1,"b":[2]}'), '{\n  "a": 1,\n  "b": [\n    2\n  ]\n}');
  assert.equal(prettyArgs('{not json'), '{not json');
  assert.equal(prettyArgs('plain words'), 'plain words');
  assert.equal(prettyArgs(null), '');
});

test('every rt.* message key used in the explorer exists, and so do the dynamic families', () => {
  const used = new Set();
  for (const f of files) for (const m of fs.readFileSync(f, 'utf8').matchAll(/['"`](rt\.[a-z_0-9.]+)['"`]/g)) used.add(m[1]);
  const missing = [...used].filter((k) => !(k in en) && !(`${k}_one` in en) && !(`${k}_other` in en));
  assert.deepEqual(missing, []);
  const fam = {
    'tab.': ['traces', 'sessions', 'analytics', 'trajectories', 'capture'],
    'rt.window.': ['1h', '6h', '24h', '7d', '30d', 'all'],
    'rt.anchor.': ['latest', 'now', 'latest_tip', 'now_tip'],
    'rt.status.': ['ok', 'error', 'open'],
    'rt.cat.': ['agent', 'turn', 'llm', 'tool', 'other'],
    'rt.index.state_': ['ready', 'building', 'empty', 'disabled'],
    'rt.index.fstate.': ['indexed', 'partial', 'skipped', 'error'],
    'rt.view.': ['waterfall', 'icicle', 'table'],
    'rt.cap.state_': ['active', 'idle', 'stale', 'off', 'unknown', 'active_tip', 'idle_tip', 'stale_tip', 'off_tip', 'unknown_tip'],
    'rt.cap.proj_': ['gen_ai', 'openinference', 'full'],
    'rt.cap.g_': ['files', 'otel', 'payload', 'enrich', 'other'],
    'rt.cap.k_': ['sinks', 'endpoints', 'mode', 'agent_name', 'minimum_severity', 'export_interval_ms', 'interval_ms', 'polling', 'detectors', 'last_ts', 'events', 'bytes', 'files', 'trajectories'],
    'rt.cap.mode_': ['full', 'turn_filtered'],
    'rt.traj.src_': ['user', 'agent', 'system'],
    'rt.ctx.mode_': ['time', 'index'],
    'rt.an.e_': ['type', 'status', 'reason'],
  };
  for (const [p, ks] of Object.entries(fam)) for (const k of ks) assert.ok(`${p}${k}` in en, `${p}${k}`);
  for (const v of Object.values(en)) assert.ok(typeof v === 'string' && v.length);
});

test('explorer files obey the CSP and the text-only rule: no innerHTML, no inline style, no eval, no external origin', () => {
  assert.ok(files.length >= 25);
  for (const f of files) {
    const src = fs.readFileSync(f, 'utf8');
    const rel = path.relative(root, f);
    assert.ok(!/\.innerHTML|insertAdjacentHTML|outerHTML|setTrustedHtml|document\.write|\beval\s*\(|new Function/.test(src), `${rel}: markup from a string`);
    assert.ok(!/setAttribute\(\s*['"]style['"]/.test(src) && !/<style/i.test(src), `${rel}: inline style`);
    assert.ok(!/https?:\/\/(?!www\.w3\.org\/2000\/svg)/.test(src), `${rel}: an origin`);
    assert.ok(!/localStorage|sessionStorage|document\.cookie/.test(src), `${rel}: browser storage`);
    assert.ok(!/\bfetch\s*\(/.test(src), `${rel}: calls the network directly instead of through api.js`);
  }
});

test('captured text is only ever set through text nodes: h() children are strings, never markup', () => {
  // content.js and capture-tab.js render capture-derived strings; they must not use any API that parses markup.
  for (const f of ['js/rt/content.js', 'js/rt/capture-tab.js', 'js/rt/span-details.js', 'js/rt/trajectories-tab.js']) {
    const src = fs.readFileSync(path.join(root, f), 'utf8');
    assert.ok(!/createContextualFragment|DOMParser|\.srcdoc|setAttribute\(\s*['"]on/.test(src), f);
  }
});

test('the settings are named exactly as the environment variables, in the disabled-state, content and help texts', () => {
  const help = fs.readFileSync(path.join(root, 'i18n/help.en.json'), 'utf8');
  assert.match(en['rt.index.disabled_hint'], /NEMO_CONSOLE_RELAY_TRACE_INDEX=off/);
  assert.match(en['rt.index.disabled_how'], /~\/\.config\/nemo-console\/env/);
  assert.match(en['rt.content.err_disabled'], /NEMO_CONSOLE_RELAY_TRACE_CONTENT=off/);
  assert.match(en['rt.index.content_off_tip'], /NEMO_CONSOLE_RELAY_TRACE_CONTENT=off/);
  assert.match(en['rt.index.truncated_hint'], /NEMO_CONSOLE_RELAY_TRACE_MAX_SPANS/);
  assert.match(help, /NEMO_CONSOLE_RELAY_TRACE_INDEX=off/);
  assert.match(help, /NEMO_CONSOLE_RELAY_TRACE_CONTENT=off/);
  for (const text of [...Object.values(en), help]) assert.ok(!/relay_trace_(index|content) = /.test(text), 'the old "setting = value" spelling is gone');
  const src = files.map((f) => fs.readFileSync(f, 'utf8')).join('\n');
  assert.match(src, /fixBlock\('NEMO_CONSOLE_RELAY_TRACE_INDEX=on'/, 'the disabled state offers the exact line to copy');
});

const L = (id, enabled, detail, observed = {}, state = 'unknown') => ({ id, label: id, enabled, state, detail, observed });
const TRACE3 = L('otel_traces', true, {
  endpoints: [
    { projection: 'gen_ai', target: 'a:1', transport: 'http', service_name: 's', reachable: null },
    { projection: 'full', target: 'b:2', transport: 'grpc', service_name: 's', reachable: false },
    { projection: 'openinference', target: 'c:3', transport: 'http', service_name: 's', reachable: true },
  ],
});
test('capture levels are typed from their pinned keys; unknown keys fall back to generic; null is skipped', () => {
  const d = describeLevel(L('atof', true, { sinks: [{ type: 'file', file: 'relay-events.jsonl', mode: 'append' }] }, { last_ts: 1700000000, events: 5, bytes: 10, files: 2 }, 'active'));
  assert.deepEqual(
    d.configured.map((f) => f.type),
    ['sinks'],
  );
  assert.deepEqual(
    d.observed.map((f) => [f.key, f.type]),
    [
      ['last_ts', 'ts'],
      ['events', 'count'],
      ['bytes', 'bytes'],
      ['files', 'count'],
    ],
  );
  const mode = (m) => describeLevel(L('payloads', true, { mode: m })).configured[0];
  assert.deepEqual(mode('full'), { type: 'mode', value: 'full' });
  assert.deepEqual(mode('turn_filtered'), { type: 'mode', value: 'turn_filtered' });
  const r = describeLevel(L('resource_metrics', true, { polling: true, interval_ms: 15000 }));
  assert.deepEqual(
    r.configured.map((f) => [f.type, f.key]),
    [
      ['bool', 'polling'],
      ['ms', 'interval_ms'],
    ],
  );
  assert.deepEqual(describeLevel(L('pricing', false, {})).configured, []);
  assert.deepEqual(describeLevel(L('redaction', false, { detectors: [] })).configured, [], 'an empty list is nothing to show');
  assert.deepEqual(describeLevel(L('atif', true, { agent_name: null })).configured, [], 'null is skipped');
  const odd = describeLevel(L('atof', true, { sinks: [{ type: 'file' }], novel_key: { a: 1 } }, { weird: 3 }));
  assert.deepEqual(odd.configured.at(-1), { type: 'generic', key: 'novel_key', value: { a: 1 } });
  assert.deepEqual(odd.observed, [{ type: 'generic', key: 'weird', value: 3 }]);
  const unknownLevel = describeLevel(L('future_level', true, { a: 1 }, { b: 2 }));
  assert.deepEqual(
    [...unknownLevel.configured, ...unknownLevel.observed].map((f) => f.type),
    ['generic', 'generic'],
  );
  assert.deepEqual(describeLevel(null), { configured: [], observed: [], warnings: [] });
});

test('logs and metrics without their own endpoints are a duplicate export when there are several trace endpoints', () => {
  assert.equal(traceEndpoints([TRACE3]).length, 3);
  const levels = [TRACE3, L('otel_logs', true, { minimum_severity: 'info', derived: true, endpoints: [] }), L('otel_metrics', true, { export_interval_ms: 60000, derived: true, endpoints: [] })];
  for (const id of ['otel_logs', 'otel_metrics']) {
    const d = describeLevel(
      levels.find((l) => l.id === id),
      levels,
    );
    assert.deepEqual(d.warnings, [{ code: 'dup_export', n: 3 }]);
    assert.ok(d.configured.some((f) => f.type === 'derived' && f.endpoints === 3));
  }
  const one = [{ ...TRACE3, detail: { endpoints: [TRACE3.detail.endpoints[0]] } }, levels[1]];
  assert.deepEqual(describeLevel(one[1], one).warnings, [], 'one trace endpoint: no duplicate');
  const explicit = L('otel_logs', true, { minimum_severity: 'warn', derived: false, endpoints: [{ target: 'x:1', transport: 'http', reachable: true }] });
  assert.deepEqual(describeLevel(explicit, levels).warnings, [], 'explicit endpoints are not derived');
  assert.equal(describeLevel(explicit, levels).configured.at(-1).type, 'endpoints');
  assert.deepEqual(traceEndpoints(null), []);
});
