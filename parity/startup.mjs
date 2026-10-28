#!/usr/bin/env node
// Startup failures across implementations (backend/PORTING.md section 4 and D-1):
//
//   node parity/startup.mjs [--rust-bin PATH]
//
// Each case starts `<backend>` (serve, or a CLI subcommand) with one bad setting in the environment or in the user env
// file, on a hermetic setup, and compares the exit code and the error text. Python dies with a traceback ending in a
// pydantic `ValidationError` / pydantic-settings `SettingsError`; the comparison takes that final exception message,
// drops the class path, pydantic's `input_value=... input_type=...` echo and the docs URL line (deviation D-1), and
// must equal what Rust prints. A port that is already taken must end both with exit 3 and the same `uvicorn.error`
// line, and must leave the same state behind (the bootstrap key: the lifespan runs before the bind).
import { spawn } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { backendCwd, DEFAULT_RUST_BIN, freePort, pythonCli } from './lib/servers.mjs';

const argv = process.argv.slice(2);
const rustBin = argv.includes('--rust-bin') ? argv[argv.indexOf('--rust-bin') + 1] : DEFAULT_RUST_BIN;
// the Python side is the reference (parity/ref.mjs): --python python@<tag or commit>, default its DEFAULT_REF
const pySpec = argv.includes('--python') ? argv[argv.indexOf('--python') + 1] : 'python';
const CLI = { python: pythonCli(pySpec), rust: [rustBin] };
const CWD = { python: backendCwd(pySpec), rust: backendCwd('rust') };
const root = mkdtempSync(join(tmpdir(), 'nc-startup-'));
const failures = [];

const CASES = [
  ['port-not-int', { NEMO_CONSOLE_PORT: 'abc' }],
  ['port-empty', { NEMO_CONSOLE_PORT: '' }],
  ['bool-2', { NEMO_CONSOLE_TRUST_LOOPBACK: '2' }],
  ['grace-0', { NEMO_CONSOLE_SHUTDOWN_GRACE_S: '0' }],
  ['grace-61', { NEMO_CONSOLE_SHUTDOWN_GRACE_S: '61' }],
  ['poll-not-float', { NEMO_CONSOLE_POLL_INTERVAL_S: 'fast' }],
  ['list-not-json', { NEMO_CONSOLE_ALLOWED_HOSTS: 'a,b' }],
  ['dict-empty', { NEMO_CONSOLE_URLS: '' }],
  ['layout-unknown-key', { NEMO_CONSOLE_LAYOUT__NOPE: 'x' }],
  ['layout-escape', { NEMO_CONSOLE_LAYOUT__SWITCHYARD: '../x' }],
  ['container-option', { NEMO_CONSOLE_DCGM_EXPORTER_CONTAINER: '-v' }],
  ['image-option', { NEMO_CONSOLE_DCGM_DIAG_IMAGE: '--privileged' }],
  ['openshell-list', { NEMO_CONSOLE_OPENSHELL_CONTAINERS: '["ok", "-x"]' }],
  ['dynamo-role', { NEMO_CONSOLE_DYNAMO_SERVICES__BOGUS: 'x' }],
  ['dynamo-dup', { NEMO_CONSOLE_DYNAMO_CONTAINERS: '{"worker": "ai-lab-dynamo-dynamo-frontend-1"}' }],
  ['unit', { NEMO_CONSOLE_RELAY_UNIT: 'relay' }],
  ['log-path', { NEMO_CONSOLE_SWITCHYARD_LOG_PATH: '/var/../etc' }],
  ['probe-interval', { NEMO_CONSOLE_CAPABLE_PROBE_INTERVAL_S: '10' }],
  ['probe-key-env', { NEMO_CONSOLE_CAPABLE_PROBE_KEY_ENV: 'HOME' }],
  ['topology-model', { NEMO_CONSOLE_TOPOLOGY_ENGINES: '[{"id": "BAD ID", "label": "", "container": "c"}]' }],
  ['two-errors', { NEMO_CONSOLE_PORT: 'abc', NEMO_CONSOLE_TRUST_LOOPBACK: 'maybe' }],
  ['env-file-bad-port', {}, { file: 'NEMO_CONSOLE_PORT=xyz\n' }],
  ['env-file-bad-json', {}, { file: 'NEMO_CONSOLE_URLS={nope\n' }],
  ['mint-key-bad-env', { NEMO_CONSOLE_PORT: 'abc' }, { args: ['mint-key', 'viewer'] }],
  ['open-bad-env', { NEMO_CONSOLE_LAYOUT__NOPE: 'x' }, { args: ['open'] }],
  ['stop-bad-env', { NEMO_CONSOLE_SHUTDOWN_GRACE_S: '99' }, { args: ['stop'] }],
];

function runOnce(kind, env, args) {
  return new Promise((resolve) => {
    const [cmd, ...pre] = CLI[kind];
    const p = spawn(cmd, [...pre, ...args], { cwd: CWD[kind], env, stdio: ['ignore', 'pipe', 'pipe'] });
    let out = ''; let err = '';
    p.stdout.on('data', (d) => { out += d; });
    p.stderr.on('data', (d) => { err += d; });
    const t = setTimeout(() => p.kill('SIGKILL'), 60_000);
    p.on('exit', (code) => { clearTimeout(t); resolve({ code, out, err }); });
  });
}

/** Python's final exception message (or Rust's whole stderr), normalised per D-1. */
function message(kind, err) {
  let text = err;
  if (kind === 'python') {
    const lines = err.split('\n');
    let i = -1;
    for (let k = lines.length - 1; k >= 0; k--) if (/^[A-Za-z_][\w.]*(Error|Exception): /.test(lines[k])) { i = k; break; }
    text = i < 0 ? err : [lines[i].replace(/^[A-Za-z_][\w.]*: /, ''), ...lines.slice(i + 1)].join('\n');
  }
  return text.split('\n').filter((l) => !/^\s+For further information visit /.test(l))
    .map((l) => l.replace(/, input_value=.*?, input_type=\w+\]$/, ']')).join('\n').trim();
}

function setup(name, kind, extraEnv, file) {
  const dir = join(root, `${name}-${kind}`);
  const home = join(dir, 'home');
  mkdirSync(home, { recursive: true });
  const env = {
    HOME: home, PATH: '/usr/bin:/bin', LANG: 'C.UTF-8', NEMO_CONSOLE_STATE_DIR: join(dir, 'state'),
    NEMO_CONSOLE_REPO_ROOT: join(dir, 'repo'), NEMO_CONSOLE_BACKGROUND_TASKS: 'false',
    NEMO_CONSOLE_URLS: '{"litellm": "http://127.0.0.1:9", "switchyard": "http://127.0.0.1:9", "relay": "http://127.0.0.1:9", "dynamo": "http://127.0.0.1:9", "dcgm": "http://127.0.0.1:9", "prometheus": "http://127.0.0.1:9", "guardrails": "http://127.0.0.1:9", "dynamo_worker_metrics": "http://127.0.0.1:9"}',
    ...extraEnv,
  };
  if (file !== undefined) {
    mkdirSync(join(home, '.config', 'nemo-console'), { recursive: true });
    writeFileSync(join(home, '.config', 'nemo-console', 'env'), file);
  } else {
    env.NEMO_CONSOLE_NO_USER_ENV = '1';
  }
  if (extraEnv.NEMO_CONSOLE_URLS === '') env.NEMO_CONSOLE_URLS = '';
  return { dir, env };
}

try {
  for (const [name, extraEnv, opt = {}] of CASES) {
    const res = {};
    for (const kind of ['python', 'rust']) {
      const { env } = setup(name, kind, extraEnv, opt.file);
      const r = await runOnce(kind, env, opt.args || []);
      res[kind] = { code: r.code, msg: message(kind, r.err) };
    }
    const eq = res.python.code === res.rust.code && res.python.msg === res.rust.msg;
    console.log(`${eq ? 'equal' : 'DIFF '} ${name}: exit ${res.python.code}${eq ? '' : `/${res.rust.code}`}  ${res.python.msg.split('\n').slice(-2).join(' | ').slice(0, 120)}`);
    if (!eq) {
      failures.push(name);
      console.log(`   python: ${JSON.stringify(res.python)}\n   rust:   ${JSON.stringify(res.rust)}`);
    }
  }
  // the port is taken: exit 3, the uvicorn.error line, and the lifespan's state (bootstrap key) left behind
  const res = {};
  for (const kind of ['python', 'rust']) {
    const port = await freePort();
    const holder = createServer();
    await new Promise((r) => holder.listen(port, '127.0.0.1', r));
    const { dir, env } = setup('port-taken', kind, { NEMO_CONSOLE_PORT: String(port), NEMO_CONSOLE_NO_USER_ENV: '1' });
    const r = await runOnce(kind, env, []);
    holder.close();
    const line = (r.err.split('\n').find((l) => l.includes('"level": "ERROR"')) || '').replace(/"ts": "[^"]*", /, '')
      .replaceAll(String(port), '<port>');
    res[kind] = { code: r.code, line, bootstrapKey: existsSync(join(dir, 'state', 'bootstrap-admin.key')) };
  }
  const eq = JSON.stringify(res.python) === JSON.stringify(res.rust);
  console.log(`${eq ? 'equal' : 'DIFF '} port-taken: exit ${res.python.code}, key left: ${res.python.bootstrapKey}, ${res.python.line.slice(0, 160)}`);
  if (!eq) { failures.push('port-taken'); console.log(`   python: ${JSON.stringify(res.python)}\n   rust:   ${JSON.stringify(res.rust)}`); }
} catch (e) {
  failures.push(`harness: ${e.stack || e.message}`);
} finally {
  if (!argv.includes('--keep')) rmSync(root, { recursive: true, force: true });
}
if (failures.length) {
  console.error(`startup: FAILED (${failures.length}): ${failures.join(', ')}`);
  process.exit(1);
}
console.log('startup: PASS (same exit codes and messages for every bad setting)');
