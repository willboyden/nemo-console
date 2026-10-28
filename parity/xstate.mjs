#!/usr/bin/env node
// Cross-implementation persisted state (backend/PORTING.md section 5): what one backend wrote, the other reads the same.
//
//   node parity/xstate.mjs [--rust-bin PATH]
//
// 1. Services level, both directions:
//    - Rust reads a console.db the Python services wrote: `cargo test --test core_xstate` (committed fixture
//      backend/crates/nemo-console/tests/fixtures/xstate, made by parity/gen/gen_xstate_fixture.py write);
//    - Python reads a console.db the Rust services wrote: the same cargo test leaves its state in NC_XSTATE_OUT, then
//      `gen_xstate_fixture.py check` opens it with the Python services and compares every read.
//    Covered: key hashes (bearer auth), audit rows (incl. the truncation marker, an unparseable row, non-ASCII),
//    config revisions (bytes + history order), metric rollups (1h/24h history), settings overrides, saved params.
// 2. HTTP level, both orders on ONE state dir: backend X starts, takes mutations (settings, audited 404/405/422
//    calls, a CLI-minted admin key), answers a set of reads; it stops; backend Y starts on the same dir and must answer
//    the same reads, with X's keys. Zero npm dependencies; hermetic (temp HOME/state, empty PATH, closed upstreams).
import { spawn } from 'node:child_process';
import { existsSync, mkdtempSync, readdirSync, readFileSync, rmSync } from 'node:fs';
import { homedir, tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { send } from './lib/http.mjs';
import { fromPlain, parse } from './lib/json.mjs';
import { compareTrees } from './lib/compare.mjs';
import { BACKEND, DEFAULT_RUST_BIN, refOf, runCli, startBackend, stopBackend } from './lib/servers.mjs';

const HERE = dirname(fileURLToPath(import.meta.url));
const argv = process.argv.slice(2);
const rustBin = argv.includes('--rust-bin') ? argv[argv.indexOf('--rust-bin') + 1] : DEFAULT_RUST_BIN;
// the Python side is the reference (parity/ref.mjs): --python python@<tag or commit>, default its DEFAULT_REF
const pySpec = argv.includes('--python') ? argv[argv.indexOf('--python') + 1] : 'python';
const SECRETS = ['LITELLM_API_KEY', 'OPENAI_API_KEY', 'ANTHROPIC_API_KEY', 'HF_TOKEN'];
const failures = [];

function run(cmd, args, { cwd, env }) {
  return new Promise((resolve) => {
    const p = spawn(cmd, args, { cwd, env, stdio: ['ignore', 'pipe', 'pipe'] });
    p.on('error', (e) => resolve({ code: -1, out: `${cmd}: ${e.message}` }));
    let out = '';
    p.stdout.on('data', (d) => { out += d; });
    p.stderr.on('data', (d) => { out += d; });
    p.on('exit', (code) => resolve({ code, out }));
  });
}

async function servicesLevel(tmp) {
  const out = join(tmp, 'rust-written');
  const env = { ...process.env, NC_XSTATE_OUT: out, CARGO_BUILD_JOBS: process.env.CARGO_BUILD_JOBS || '8' };
  for (const k of SECRETS) delete env[k];
  const cargoBin = existsSync(join(homedir(), '.cargo', 'bin', 'cargo')) ? join(homedir(), '.cargo', 'bin', 'cargo') : 'cargo';
  const cargo = await run(cargoBin, ['test', '--quiet', '--test', 'core_xstate'], { cwd: BACKEND, env });
  const ok = /test result: ok\. 2 passed/.test(cargo.out);
  console.log(`services: rust reads python state + rust writes state: ${ok ? 'ok' : 'FAILED'}`);
  if (!ok) { failures.push(`cargo test core_xstate:\n${cargo.out.slice(-2000)}`); return; }
  const ref = refOf(pySpec);
  const py = await run(ref.python, ['-I', join(HERE, 'gen', 'gen_xstate_fixture.py'), 'check', out], {
    cwd: ref.backendDir,
    env: { HOME: join(tmp, 'pyhome'), PATH: '/usr/bin:/bin', LANG: 'C.UTF-8', NEMO_CONSOLE_NO_USER_ENV: '1', TMPDIR: tmp },
  });
  console.log(`services: ${py.out.trim().split('\n').pop()}`);
  if (py.code !== 0) failures.push(`python check of the rust-written state:\n${py.out.slice(-3000)}`);
}

const json = (r) => { try { return parse(r.body.toString('utf8')); } catch { return fromPlain({ _text: r.body.toString('utf8') }); } };
const drop = (t, key) => { if (t.k === 'obj') t.v.delete(key); return t; };

async function req(b, method, path, { key, body, headers = {} } = {}) {
  const h = { Host: `127.0.0.1:${b.port}`, 'X-Console': '1', ...headers };
  if (key) h.Authorization = `Bearer ${key}`;
  let payload;
  if (body !== undefined) { payload = JSON.stringify(body); h['Content-Type'] = 'application/json'; }
  return send(b.port, { method, path, headers: h, body: payload, timeoutMs: 30_000 });
}

/** Mutations every backend serves from the core alone (no adapter needed). */
async function mutate(b, tag) {
  const k = b.adminKey;
  await req(b, 'PUT', '/api/v1/settings', { key: k, body: { poll_interval_s: tag === 'x' ? 6 : 9.5, urls: { dynamo: 'http://localhost:8021' } } });
  await req(b, 'PUT', '/api/v1/settings', { key: k, body: { playground_key_env: { relay: `NEMO_CONSOLE_PLAYGROUND_${tag.toUpperCase()}` } } });
  await req(b, 'PUT', '/api/v1/settings', { key: k, body: { poll_interval_s: 0.01 } });                  // 422, audited
  await req(b, 'POST', '/api/v1/systems', { key: k, body: { note: `héllo ${tag}`, token: 'zzz' } });       // 405, audited
  await req(b, 'POST', '/api/v1/systems/nope/actions/x', { key: k, body: {} });                            // 404, audited
  await req(b, 'PUT', '/api/v1/settings', { key: b.viewerKey, body: {} });                                 // 403: a denial, not a row
  const minted = await runCli(b, ['mint-key', 'admin']);
  if (minted.code !== 0) throw new Error(`${b.label}: mint-key admin failed: ${minted.stderr}`);
  const f = readdirSync(b.paths.state).filter((n) => /^minted-admin-.*\.key$/.test(n)).sort();
  return readFileSync(join(b.paths.state, f[f.length - 1]), 'utf8').trim();
}

/** The reads compared across the restart (restart_required is per process: dropped). */
async function reads(b, keys) {
  const out = new Map();
  out.set('audit', json(await req(b, 'GET', '/api/v1/audit?limit=500', { key: keys.admin })));
  // `version` is each implementation's own release (the reference is older): not state
  out.set('settings', drop(drop(json(await req(b, 'GET', '/api/v1/settings', { key: keys.admin })), 'restart_required'), 'version'));
  out.set('settings_viewer', drop(drop(json(await req(b, 'GET', '/api/v1/settings', { key: keys.viewer })), 'restart_required'), 'version'));
  for (const [label, key] of Object.entries(keys)) {
    const h = json(await req(b, 'GET', '/api/v1/health', { key }));
    out.set(`role:${label}`, h.k === 'obj' ? h.v.get('role') : h);
    out.set(`systems:${label}`, fromPlain((await req(b, 'GET', '/api/v1/systems', { key })).status));
  }
  out.set('status:wrong-key', fromPlain((await req(b, 'GET', '/api/v1/systems', { key: 'nck_wrong' })).status));
  return { k: 'obj', v: out };
}

async function httpLevel(tmp, first, second) {
  const side = join(tmp, `${first}-then-${second}`);
  // the same closed-port URLs for both starts (they are process config, not state)
  const closed = Object.fromEntries(['switchyard', 'relay', 'dynamo', 'dynamo_worker_metrics', 'litellm', 'dcgm', 'prometheus',
    'guardrails'].map((n) => [n, 'http://127.0.0.1:9']));
  const scenario = { name: 'xstate', seed: false, fakes: false, upstreams: false, env: { NEMO_CONSOLE_URLS: JSON.stringify(closed) } };
  const kindOf = (k) => (k === 'python' ? pySpec : k);
  const a = await startBackend({ kind: kindOf(first), rustBin, sideDir: side, label: first, scenario });
  let keys;
  let before;
  try {
    const mintedAdmin = await mutate(a, 'x');
    keys = { admin: a.adminKey, viewer: a.viewerKey, minted: mintedAdmin };
    before = await reads(a, keys);
  } finally { await stopBackend(a); }
  const b = await startBackend({ kind: kindOf(second), rustBin, sideDir: side, label: second, scenario });
  try {
    const after = await reads(b, keys);
    const d = compareTrees(before, after);
    console.log(`http: ${first} wrote, ${second} reads: ${d.length ? `${d.length} DIFFS` : 'equal'} (${before.v.get('audit').v.length} audit rows, keys ${Object.keys(keys).join('/')})`);
    for (const x of d.slice(0, 20)) console.log(`   ${JSON.stringify(x).slice(0, 300)}`);
    if (d.length) failures.push(`${first} -> ${second}: ${d.length} diffs`);
    // the second backend keeps writing on top, and its own reads stay consistent with the rows the first one wrote
    await mutate(b, 'y');
    const more = await reads(b, keys);
    const n0 = before.v.get('audit').v.length;
    const n1 = more.v.get('audit').v.length;
    if (n1 !== n0 + 5) failures.push(`${first} -> ${second}: ${n1} audit rows after the second backend's writes (want ${n0 + 5})`);
  } finally { await stopBackend(b); }
}

const tmp = mkdtempSync(join(tmpdir(), 'nc-xstate-'));
try {
  await servicesLevel(tmp);
  await httpLevel(tmp, 'python', 'rust');
  await httpLevel(tmp, 'rust', 'python');
} catch (e) {
  failures.push(`harness: ${e.stack || e.message}`);
} finally {
  if (!argv.includes('--keep')) rmSync(tmp, { recursive: true, force: true });
}
if (failures.length) {
  console.error(`xstate: FAILED (${failures.length})`);
  for (const f of failures) console.error(`  - ${f}`);
  process.exit(1);
}
console.log('xstate: PASS (each backend reads what the other wrote)');
void HERE;
