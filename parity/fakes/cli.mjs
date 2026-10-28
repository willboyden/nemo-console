// Fake CLI dispatcher. Every allowlisted binary of the console's runner (services/runner.py ALLOWED_BINARIES) is a
// tiny `#!/bin/sh` shim in the backend's own bin dir:
//     exec <node> <this file> <sideDir> <name> "$@"
// The side dir is baked into the shim (the runner passes the child only PATH, HOME, XDG_RUNTIME_DIR, LANG, ... so a
// custom env var would never arrive). This file appends ONE JSON line per invocation to <sideDir>/cli.jsonl
// ({argv, cwd, stdin, env}) and answers from fakes/cli-table.mjs: stdout, stderr, exit code, optional delay and files
// to write. An argv the table does not know exits 1 with a clear stderr, and the log line says `unknown: true`.
import { appendFileSync, readFileSync, writeFileSync, mkdirSync, copyFileSync, renameSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { isatty } from 'node:tty';
import { respond, initialWorld } from './cli-table.mjs';

const ENV_SHOWN = ['PATH', 'HOME', 'XDG_RUNTIME_DIR', 'DBUS_SESSION_BUS_ADDRESS', 'DOCKER_HOST', 'DOCKER_CONTEXT',
  'DOCKER_CONFIG', 'LANG', 'LC_ALL'];
const ENV_IGNORED = new Set(['PWD', 'OLDPWD', 'SHLVL', '_']);       // added by /bin/sh itself, not by the console

async function main() {
  const [sideDir, name, ...args] = process.argv.slice(2);
  const conf = JSON.parse(readFileSync(join(sideDir, 'cli.json'), 'utf8'));
  // stdin read to EOF asynchronously (a synchronous read of a non-blocking pipe can throw EAGAIN and lose the data);
  // the runner gives either a pipe it closes after writing, or /dev/null
  let stdin = '';
  if (!isatty(0)) {
    const chunks = [];
    for await (const c of process.stdin) chunks.push(c);
    stdin = Buffer.concat(chunks).toString('utf8');
  }
  const envKeys = Object.keys(process.env).filter((k) => !ENV_IGNORED.has(k)).sort();
  const env = Object.fromEntries(ENV_SHOWN.filter((k) => k in process.env).map((k) => [k, process.env[k]]));
  // the fake machine's state (containers, sandboxes, ...): created on first use, changed by mutating commands
  const worldFile = join(sideDir, 'fake-world.json');
  let world;
  try { world = JSON.parse(readFileSync(worldFile, 'utf8')); } catch { world = initialWorld(); writeAtomic(worldFile, JSON.stringify(world)); }
  const ctx = { ...conf, name, args, argv: [name, ...args], stdin, cwd: process.cwd(), world, worldChanged: false };
  let ans;
  try {
    ans = respond(ctx);
  } catch (e) {
    ans = { code: 70, stderr: `parity fake: table error for ${JSON.stringify(ctx.argv)}: ${e.message}\n` };
  }
  const unknown = !ans;
  if (unknown) ans = { code: 1, stderr: `parity fake ${name}: no scripted answer for ${JSON.stringify(ctx.argv)}\n` };
  const rec = { argv: ctx.argv, cwd: ctx.cwd, envKeys, env, ...(stdin ? { stdin } : {}), ...(unknown ? { unknown: true } : {}) };
  if (ctx.worldChanged) writeAtomic(worldFile, JSON.stringify(world));
  appendFileSync(conf.log, `${JSON.stringify(rec)}\n`);
  for (const f of ans.files || []) {
    mkdirSync(dirname(f.path), { recursive: true });
    if (f.copyFrom) copyFileSync(f.copyFrom, f.path);
    else writeFileSync(f.path, f.content ?? '');
  }
  if (ans.delayMs) await new Promise((r) => setTimeout(r, ans.delayMs));
  if (ans.stdout) process.stdout.write(ans.stdout);
  if (ans.stderr) process.stderr.write(ans.stderr);
  // exit only after the pipes drained
  const code = ans.code ?? 0;
  const flush = (s) => new Promise((r) => (s.writableLength ? s.once('drain', r) : r()));
  await Promise.all([flush(process.stdout), flush(process.stderr)]);
  process.exitCode = code;
}

function writeAtomic(path, text) {
  const tmp = `${path}.${process.pid}.tmp`;
  writeFileSync(tmp, text);
  renameSync(tmp, path);
}

main();
