// Hermetic backends. Each backend (python or rust) gets its own temp dir with its own HOME / XDG dirs / state dir /
// seeded repo copy, its own fake upstreams and its own fake-CLI bin dir, and the SAME settings otherwise. The frontend
// dir is the worktree's frontend/ for both (so static ETags match). PATH holds ONLY the fake bin dir: the real docker
// in /usr/bin must never be reachable. Nothing is read from or written to the real HOME.
import { spawn } from 'node:child_process';
import { chmodSync, existsSync, readdirSync, readFileSync, writeFileSync } from 'node:fs';
import { createServer, connect } from 'node:net';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { makeTree, FIXTURES, PARITY } from './seed.mjs';
import { startUpstreams, UPSTREAM_NAMES } from '../fakes/upstreams.mjs';
import { initialWorld } from '../fakes/cli-table.mjs';
import { pythonRef, refTag, rustVersion } from '../ref.mjs';

export const WORKTREE = resolve(PARITY, '..');
/** The Rust backend's Cargo workspace (the only backend in the checkout since 0.4.0). */
export const BACKEND = join(WORKTREE, 'backend');
export const FRONTEND = join(WORKTREE, 'frontend');
export const DEFAULT_RUST_BIN = join(BACKEND, 'target', 'debug', 'nemo-console');

/** A backend kind is `rust` or a Python reference: `python` (= python@DEFAULT_REF) / `python@<tag or commit>`. */
export const isPython = (kind) => kind === 'python' || String(kind).startsWith('python@');

/** The Python reference of a kind (prepared on first use, see ref.mjs). */
export const refOf = (kind) => pythonRef(refTag(kind));

/** Working directory a backend runs in: the same for every kind (the fake CLIs record the cwd their caller passed on,
 *  and `-I` keeps the cwd off Python's import path, so the reference runs from here as well as from its own dir). */
export function backendCwd(_kind) {
  return BACKEND;
}

/** `<python> -I -m nemo_console.main` of a reference: how the Python console is started and found by `stop`. */
export function pythonCli(kind) {
  return [refOf(kind).python, '-I', '-m', 'nemo_console.main'];
}

/** The version string a backend reports (health, settings, openapi), for the `<version>` normalisation. */
export function backendVersion(kind) {
  return isPython(kind) ? refOf(kind).version : rustVersion();
}
const CLI_JS = join(dirname(fileURLToPath(import.meta.url)), '..', 'fakes', 'cli.mjs');
// services/runner.py ALLOWED_BINARIES: every one gets a fake
export const FAKE_BINARIES = ['docker', 'systemctl', 'nemo-relay', 'skillevaluator', 'skillevaluator-local',
  'data-designer-local', 'data-designer-export', 'openshell', 'aiperf-local', 'guardrails-local', 'evaluator-local'];
export const DUMMY_LITELLM_KEY = 'sk-parity-dummy-000000000000';
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// Ports handed out in this harness process: never twice (a closed port can come back from bind(0), and the "empty"
// scenario's refused upstream ports must not become another backend's console port).
const USED_PORTS = new Set();
const bindZero = () => new Promise((res, rej) => {
  const s = createServer();
  s.once('error', rej);
  s.listen(0, '127.0.0.1', () => { const { port } = s.address(); s.close(() => res(port)); });
});
export async function freePort() {
  for (let i = 0; i < 50; i++) {
    const p = await bindZero();
    if (!USED_PORTS.has(p)) { USED_PORTS.add(p); return p; }
  }
  throw new Error('no unused ephemeral port after 50 tries');
}
export const notePortsUsed = (ports) => { for (const p of ports) USED_PORTS.add(p); };

const tcpOpen = (port) => new Promise((res) => {
  const sock = connect({ host: '127.0.0.1', port });
  sock.once('connect', () => { sock.destroy(); res(true); });
  sock.once('error', () => res(false));
});

/** argv prefix that runs the backend binary of `kind`. */
export function backendCommand(kind, rustBin) {
  if (isPython(kind)) {
    const { python } = refOf(kind);
    return [python, '-I', '-B', '-m', 'nemo_console.main'];     // -B: never write .pyc files into the reference
  }
  if (kind === 'rust') {
    const bin = rustBin || DEFAULT_RUST_BIN;
    if (!existsSync(bin)) {
      throw new Error(`Rust backend binary not found at ${bin}: build it (cd backend && cargo build --locked) or pass --rust-bin PATH`);
    }
    return [bin];
  }
  throw new Error(`unknown backend kind ${kind} (rust | python | python@<tag or commit>)`);
}

function writeShims(binDir, sideDir) {
  for (const name of FAKE_BINARIES) {
    const p = join(binDir, name);
    writeFileSync(p, `#!/bin/sh\nexec ${JSON.stringify(process.execPath)} ${JSON.stringify(CLI_JS)} ${JSON.stringify(sideDir)} ${JSON.stringify(name)} "$@"\n`);
    chmodSync(p, 0o755);
  }
}

/** Run `<backend> <args>` once to completion with the backend's env (mint-key and other CLI subcommands). */
export function runCli(b, args, { timeoutMs = 60_000 } = {}) {
  const [cmd, ...pre] = b.command;
  return new Promise((res) => {
    const p = spawn(cmd, [...pre, ...args], { cwd: backendCwd(b.kind), env: b.env, stdio: ['ignore', 'pipe', 'pipe'] });
    let out = ''; let err = '';
    p.stdout.on('data', (d) => { out += d; });
    p.stderr.on('data', (d) => { err += d; });
    const t = setTimeout(() => p.kill('SIGKILL'), timeoutMs);
    p.on('close', (code, signal) => { clearTimeout(t); res({ code, signal, stdout: out, stderr: err }); });
  });
}

/**
 * Start one hermetic backend (kind: rust | python | python@<tag or commit>).
 * @param {{kind:string, rustBin?:string, sideDir:string, label:string, scenario:{name:string, seed:boolean, fakes:boolean, upstreams:boolean, env?:object}}} o
 */
export async function startBackend(o) {
  const { kind, sideDir, scenario, label } = o;
  const command = backendCommand(kind, o.rustBin);
  const p = makeTree(sideDir, { seed: scenario.seed });
  const cliLog = join(sideDir, 'cli.jsonl');
  writeFileSync(cliLog, '');
  writeFileSync(join(sideDir, 'cli.json'), JSON.stringify({
    scenario: scenario.name, fixtures: FIXTURES, repo: p.repo, home: p.home, side: sideDir, state: p.state, log: cliLog }));
  const worldFile = join(sideDir, 'fake-world.json');
  if (scenario.fakes) {
    writeShims(p.bin, sideDir);
    writeFileSync(worldFile, JSON.stringify(initialWorld()));
  }

  let upstreams = null;
  let urls;
  if (scenario.upstreams) {
    upstreams = await startUpstreams({ worldFile: scenario.fakes ? worldFile : undefined });
    notePortsUsed(Object.values(upstreams.ports));
    urls = upstreams.urls;
  } else {
    // nothing listens on these ports: every upstream is "down" (connection refused)
    urls = {};
    for (const n of UPSTREAM_NAMES) urls[n] = `http://127.0.0.1:${await freePort()}`;
  }
  const port = await freePort();
  const env = {
    PATH: p.bin, HOME: p.home, LANG: 'C.UTF-8',
    XDG_CONFIG_HOME: join(p.home, '.config'), XDG_DATA_HOME: join(p.home, '.local/share'),
    XDG_CACHE_HOME: join(p.home, '.cache'), XDG_STATE_HOME: join(p.home, '.local/state'), XDG_RUNTIME_DIR: p.xdgRuntime,
    NEMO_CONSOLE_NO_USER_ENV: '1', NEMO_CONSOLE_PORT: String(port), NEMO_CONSOLE_STATE_DIR: p.state,
    NEMO_CONSOLE_REPO_ROOT: p.repo, NEMO_CONSOLE_FRONTEND_DIR: FRONTEND,
    NEMO_CONSOLE_BACKGROUND_TASKS: 'false', NEMO_CONSOLE_TRUST_LOOPBACK: 'false',
    // no status cache: whether a request is served from the 2 s cache would otherwise depend on timing, and a cached
    // status can lag a change the fake machine just made (same setting for both backends)
    NEMO_CONSOLE_STATUS_TTL_S: '0',
    NEMO_CONSOLE_URLS: JSON.stringify(urls),
    LITELLM_API_KEY: DUMMY_LITELLM_KEY,
    ...(scenario.env || {}),
  };
  const b = { kind, label, command, env, port, paths: p, upstreams, urls, cliLog, sideDir, scenario: scenario.name, log: '',
    version: backendVersion(kind) };

  const [cmd, ...pre] = command;
  const proc = spawn(cmd, pre, { cwd: backendCwd(kind), env, stdio: ['ignore', 'pipe', 'pipe'] });
  b.proc = proc;
  proc.stdout.on('data', (d) => { b.log += d; });
  proc.stderr.on('data', (d) => { b.log += d; });
  // Readiness by TCP connect only: an HTTP probe would land in the console's own request counters (/metrics), and the
  // number of probes would differ between a slow and a fast backend. Both bind after their startup work is done.
  const keyFile = join(p.state, 'bootstrap-admin.key');
  let up = false;
  for (let i = 0; i < 300; i++) {
    if (proc.exitCode !== null) throw new Error(`${label} (${kind}) exited early (${proc.exitCode}):\n${b.log.slice(-3000)}`);
    if (await tcpOpen(port)) { up = true; break; }
    await sleep(100);
  }
  if (!up) { await stopBackend(b); throw new Error(`${label} (${kind}) never listened on ${port}:\n${b.log.slice(-3000)}`); }
  for (let i = 0; i < 50 && !existsSync(keyFile); i++) await sleep(100);
  if (!existsSync(keyFile)) { await stopBackend(b); throw new Error(`${label}: no ${keyFile} after start`); }
  b.adminKey = readFileSync(keyFile, 'utf8').trim();
  // a viewer key minted the way an operator does it, with the same env (after the start: minting first would make
  // the server skip its bootstrap key)
  const minted = await runCli(b, ['mint-key', 'viewer']);
  if (minted.code !== 0) { await stopBackend(b); throw new Error(`${label}: mint-key viewer failed (${minted.code}): ${minted.stderr.slice(-1000)}`); }
  const vf = readdirSync(p.state).find((f) => /^minted-viewer-.*\.key$/.test(f));
  if (!vf) { await stopBackend(b); throw new Error(`${label}: mint-key viewer wrote no minted-viewer-*.key`); }
  b.viewerKey = readFileSync(join(p.state, vf), 'utf8').trim();
  b.mintOutput = minted;
  return b;
}

/** Stop a backend we started (SIGTERM, then SIGKILL after 8 s) and its fake upstreams. Only our own child. */
export async function stopBackend(b) {
  if (b.proc && b.proc.exitCode === null && b.proc.signalCode === null) {
    const exited = new Promise((r) => b.proc.once('exit', r));
    b.proc.kill('SIGTERM');
    const t = setTimeout(() => { try { b.proc.kill('SIGKILL'); } catch { /* gone */ } }, 8000);
    await exited;
    clearTimeout(t);
  }
  if (b.upstreams) await b.upstreams.close();
}

/** CLI log lines appended since `from` (a line count). */
export function readCliLog(b) {
  const text = readFileSync(b.cliLog, 'utf8');
  return text.split('\n').filter(Boolean).map((l) => { try { return JSON.parse(l); } catch { return { argv: ['<unparseable log line>'], raw: l }; } });
}
