#!/usr/bin/env node
// The desktop launcher CLI (`nemo-console open` / `stop`, backend/PORTING.md D-4) across implementations:
//
//   node parity/launcher.mjs [--rust-bin PATH]
//
// Every case runs once with the Python CLI and once with the Rust CLI on identical hermetic setups (temp HOME and
// state dir, PATH = a dir holding only a fake `xdg-open` that records the URL, a free port), and the exit code,
// stdout and stderr must be equal after normalising pids, temp paths and the port. The cross cases start a console
// with one implementation and stop it with the other (both directions). Every console a case started is stopped
// (by its pid file, after checking the pid's command line) before the next case. Zero npm dependencies.
import { spawn } from 'node:child_process';
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { backendCwd, DEFAULT_RUST_BIN, freePort, pythonCli } from './lib/servers.mjs';

const argv = process.argv.slice(2);
const rustBin = argv.includes('--rust-bin') ? argv[argv.indexOf('--rust-bin') + 1] : DEFAULT_RUST_BIN;
// the Python side is the reference (parity/ref.mjs): --python python@<tag or commit>, default its DEFAULT_REF
const pySpec = argv.includes('--python') ? argv[argv.indexOf('--python') + 1] : 'python';
const CLI = { python: pythonCli(pySpec), rust: [rustBin] };
const CWD = { python: backendCwd(pySpec), rust: backendCwd('rust') };
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const failures = [];
const root = mkdtempSync(join(tmpdir(), 'nc-launcher-'));

function setup(name) {
  const dir = join(root, name);
  const home = join(dir, 'home');
  const bin = join(dir, 'bin');
  const state = join(dir, 'state');
  for (const d of [home, bin]) mkdirSync(d, { recursive: true });
  const opened = join(dir, 'opened.txt');
  writeFileSync(join(bin, 'xdg-open'), `#!/bin/sh\necho "$1" >> '${opened}'\n`);
  chmodSync(join(bin, 'xdg-open'), 0o755);
  return { dir, home, bin, state, opened };
}

function envFor(s, port, extra = {}) {
  return {
    HOME: s.home, PATH: s.bin, LANG: 'C.UTF-8', NEMO_CONSOLE_NO_USER_ENV: '1', NEMO_CONSOLE_PORT: String(port),
    NEMO_CONSOLE_STATE_DIR: s.state, NEMO_CONSOLE_REPO_ROOT: join(s.dir, 'repo'), NEMO_CONSOLE_BACKGROUND_TASKS: 'false',
    NEMO_CONSOLE_URLS: JSON.stringify({ litellm: 'http://127.0.0.1:9', switchyard: 'http://127.0.0.1:9', relay: 'http://127.0.0.1:9',
      dynamo: 'http://127.0.0.1:9', dcgm: 'http://127.0.0.1:9', prometheus: 'http://127.0.0.1:9', guardrails: 'http://127.0.0.1:9',
      dynamo_worker_metrics: 'http://127.0.0.1:9' }),
    ...extra,
  };
}

function cli(kind, args, env) {
  return new Promise((resolve) => {
    const [cmd, ...pre] = CLI[kind];
    const p = spawn(cmd, [...pre, ...args], { cwd: CWD[kind], env, stdio: ['ignore', 'pipe', 'pipe'] });
    let stdout = ''; let stderr = '';
    p.stdout.on('data', (d) => { stdout += d; });
    p.stderr.on('data', (d) => { stderr += d; });
    const t = setTimeout(() => p.kill('SIGKILL'), 60_000);
    p.on('exit', (code) => { clearTimeout(t); resolve({ code, stdout, stderr }); });
  });
}

const norm = (text, s, port) => text
  .replaceAll(s.dir, '<DIR>').replaceAll(String(port), '<port>')
  .replace(/pid \d+/g, 'pid <pid>');

/** Stop whatever console the pid file names, if its command line is a console's (only processes this run started). */
async function cleanup(s) {
  const f = join(s.state, 'console.pid');
  if (!existsSync(f)) return;
  const pid = Number(readFileSync(f, 'utf8').trim());
  if (!pid) return;
  let cmdline = '';
  try { cmdline = readFileSync(`/proc/${pid}/cmdline`, 'utf8'); } catch { return; }
  if (!cmdline.includes('nemo_console.main')) return;
  try { process.kill(pid, 'SIGTERM'); } catch { return; }
  for (let i = 0; i < 100 && existsSync(`/proc/${pid}`); i++) await sleep(100);
  try { process.kill(pid, 'SIGKILL'); } catch { /* gone */ }
}

/** A foreign HTTP server on the port (answers `{}` to /api/v1/health). */
function foreign(port) {
  return new Promise((resolve) => {
    const srv = createServer((_q, r) => { r.writeHead(200, { 'content-type': 'application/json' }); r.end('{}'); });
    srv.listen(port, '127.0.0.1', () => resolve(srv));
  });
}

/** One case: steps of [kind-or-role, args, extraEnv?]; roles 'A'/'B' are the two implementations in this order. */
async function runCase(name, steps, { before } = {}) {
  const results = {};
  for (const order of [['python', 'rust'], ['rust', 'python']]) {
    const s = setup(`${name}-${order[0]}-${order[1]}`);
    const port = await freePort();
    const ctx = { s, port };
    const outs = [];
    let srv;
    try {
      if (before) srv = await before(ctx);
      for (const [role, args, extra] of steps) {
        const kind = role === 'A' ? order[0] : order[1];
        const r = await cli(kind, args, envFor(s, port, extra));
        outs.push({ step: `${role} ${args.join(' ')}`, code: r.code, stdout: norm(r.stdout, s, port), stderr: norm(r.stderr, s, port) });
      }
      const opened = existsSync(s.opened) ? norm(readFileSync(s.opened, 'utf8'), s, port) : '';
      results[order.join('>')] = { outs, opened };
    } finally {
      if (srv) srv.close();
      await cleanup(s);
    }
  }
  return results;
}

/** Same-implementation cases: the Python run and the Rust run must print and exit alike. */
async function same(name, steps, opts) {
  // a single-implementation case is expressed with role A only; run it under both orders = python then rust
  const r = await runCase(name, steps, opts);
  const [p, q] = [r['python>rust'], r['rust>python']];
  const eq = JSON.stringify(p) === JSON.stringify(q);
  console.log(`${eq ? 'equal ' : 'DIFF  '} ${name}: ${p.outs.map((o) => `${o.step} -> ${o.code}`).join('; ')}`);
  if (!eq) {
    failures.push(name);
    console.log(`   python: ${JSON.stringify(p).slice(0, 900)}\n   rust:   ${JSON.stringify(q).slice(0, 900)}`);
  }
}

/** Cross cases: A opens, B stops; both orders must give the same transcript. */
async function cross(name, steps, want) {
  const r = await runCase(name, steps);
  const [p, q] = [r['python>rust'], r['rust>python']];
  const eq = JSON.stringify(p) === JSON.stringify(q);
  const ok = eq && want(p);
  console.log(`${ok ? 'equal ' : 'DIFF  '} ${name} (python>rust vs rust>python): ${p.outs.map((o) => `${o.step} -> ${o.code}`).join('; ')}`);
  if (!ok) {
    failures.push(name);
    console.log(`   python>rust: ${JSON.stringify(p).slice(0, 900)}\n   rust>python: ${JSON.stringify(q).slice(0, 900)}`);
  }
}

try {
  await same('stop-nothing-recorded', [['A', ['stop']]]);
  await same('stop-garbage-pid', [['A', ['stop']]], { before: ({ s }) => { mkdirSync(s.state, { recursive: true }); writeFileSync(join(s.state, 'console.pid'), 'abc\n'); } });
  await same('stop-dead-pid', [['A', ['stop']]], { before: ({ s }) => { mkdirSync(s.state, { recursive: true }); writeFileSync(join(s.state, 'console.pid'), '4194300\n'); } });
  await same('stop-stranger-pid', [['A', ['stop']]], { before: ({ s }) => { mkdirSync(s.state, { recursive: true }); writeFileSync(join(s.state, 'console.pid'), `${process.pid}\n`); } });
  await same('open-usage', [['A', ['open', 'extra']]]);
  await same('stop-usage', [['A', ['stop', 'now']]]);
  await same('open-non-loopback-host', [['A', ['open'], { NEMO_CONSOLE_HOST: '8.8.8.8' }]]);
  await same('open-foreign-listener', [['A', ['open']]], { before: ({ port }) => foreign(port) });
  await same('open-then-stop', [['A', ['open']], ['A', ['open']], ['A', ['stop']], ['A', ['stop']]]);
  await cross('cross-open-then-stop', [['A', ['open']], ['B', ['open']], ['B', ['stop']], ['A', ['stop']]],
    (p) => p.outs[0].code === 0 && /started Nemo Console/.test(p.outs[0].stdout) && p.outs[1].code === 0 && !p.outs[1].stdout
      && p.outs[2].code === 0 && /stopped Nemo Console/.test(p.outs[2].stdout) && p.outs[3].code === 1);
} catch (e) {
  failures.push(`harness: ${e.stack || e.message}`);
} finally {
  if (!argv.includes('--keep')) rmSync(root, { recursive: true, force: true });
}
if (failures.length) {
  console.error(`launcher: FAILED (${failures.length}): ${failures.join(', ')}`);
  process.exit(1);
}
console.log('launcher: PASS (open/stop print and exit alike; each implementation stops a console the other started)');
