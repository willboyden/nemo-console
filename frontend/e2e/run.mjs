#!/usr/bin/env node
// Browser end-to-end check of Nemo Console: the REAL backend, a real headless Chrome, the same auth mode as a real
// install (loopback NOT trusted; an admin bearer in sessionStorage, exactly where the UI keeps it). Zero
// dependencies: Node 22's built-in WebSocket speaks the DevTools protocol (ADR 0013: no npm).
//
//   cd frontend && node e2e/run.mjs          (or `make e2e` from the repo root)
//
// Needs: ../backend/.venv (make setup) and a Chrome — $CHROME_BIN (`make e2e` also reads `git config nemo-console.chrome`),
// else google-chrome / chromium / chrome-headless-shell on PATH. Every page of the nav and every tab of every system
// page is opened; the run FAILS on:
//   * the error state ("Something went wrong", class `state error`) or the not-found state anywhere in the page,
//   * an uncaught exception or a console error,
//   * a frontend file (html, js, css, json) served without `Cache-Control: no-cache` — a browser that reuses a stale
//     module next to a new one breaks the new pages (2026-10-24: five new views failed for a user that way).
// Systems that are not running are FINE: a down system must still render its page. The backend runs with a
// throwaway HOME and state dir, so nothing of the developer's config or keys is read.
import { spawn } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, existsSync, mkdirSync, chmodSync } from 'node:fs';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const FRONTEND = dirname(dirname(fileURLToPath(import.meta.url)));
const BACKEND = join(FRONTEND, '..', 'backend');
const PY = join(BACKEND, '.venv', 'bin', 'python');
const SETTLE_MS = 400;          // the page must stay free of skeletons for this long to count as rendered
const PAGE_TIMEOUT_MS = 20000;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function findChrome() {
  if (process.env.CHROME_BIN) return process.env.CHROME_BIN;
  for (const dir of (process.env.PATH || '').split(':')) {
    for (const name of ['google-chrome', 'google-chrome-stable', 'chromium', 'chromium-browser', 'chrome-headless-shell']) {
      if (dir && existsSync(join(dir, name))) return join(dir, name);
    }
  }
  throw new Error('no Chrome found: set CHROME_BIN, or `git config nemo-console.chrome <path>`, or put google-chrome / chromium / chrome-headless-shell on PATH');
}

const freePort = () => new Promise((resolve, reject) => {
  const s = createServer();
  s.once('error', reject);
  s.listen(0, '127.0.0.1', () => { const { port } = s.address(); s.close(() => resolve(port)); });
});

/** Minimal DevTools client: one browser WebSocket, flat sessions. */
export class Cdp {
  constructor(ws) { this.ws = ws; this.id = 0; this.waiting = new Map(); this.handlers = []; }
  static async connect(url) {
    const ws = new WebSocket(url);
    await new Promise((res, rej) => { ws.onopen = res; ws.onerror = () => rej(new Error(`cannot connect to ${url}`)); });
    const c = new Cdp(ws);
    ws.onmessage = (m) => {
      const msg = JSON.parse(m.data);
      if (msg.id && c.waiting.has(msg.id)) {
        const { resolve, reject } = c.waiting.get(msg.id); c.waiting.delete(msg.id);
        msg.error ? reject(new Error(`${msg.error.message} (${msg.error.code})`)) : resolve(msg.result);
      } else if (msg.method) { for (const h of c.handlers) h(msg); }
    };
    return c;
  }
  send(method, params = {}, sessionId) {
    const id = ++this.id;
    this.ws.send(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) }));
    return new Promise((resolve, reject) => this.waiting.set(id, { resolve, reject }));
  }
  on(h) { this.handlers.push(h); }
  close() { try { this.ws.close(); } catch { /* ignore */ } }
}

export async function startBackend(tmp, { port, frontendDir } = {}) {
  if (!existsSync(PY)) throw new Error(`${PY} missing: run \`make setup\` first`);
  port ||= await freePort();
  const home = join(tmp, 'home'); const state = join(tmp, 'state');
  mkdirSync(home, { recursive: true }); mkdirSync(state, { recursive: true }); chmodSync(state, 0o700);
  const env = {
    PATH: process.env.PATH || '/usr/bin:/bin', HOME: home, LANG: 'C.UTF-8',
    NEMO_CONSOLE_PORT: String(port), NEMO_CONSOLE_STATE_DIR: state, NEMO_CONSOLE_NO_USER_ENV: '1',
    NEMO_CONSOLE_TRUST_LOOPBACK: 'false',
    ...(frontendDir ? { NEMO_CONSOLE_FRONTEND_DIR: frontendDir } : {}),
  };
  const proc = spawn(PY, ['-I', '-m', 'nemo_console.main'], { cwd: BACKEND, env, stdio: ['ignore', 'pipe', 'pipe'] });
  let log = ''; proc.stdout.on('data', (d) => { log += d; }); proc.stderr.on('data', (d) => { log += d; });
  const base = `http://127.0.0.1:${port}`;
  for (let i = 0; i < 150; i++) {
    if (proc.exitCode !== null) throw new Error(`backend exited early (${proc.exitCode}):\n${log.slice(-2000)}`);
    try { if ((await fetch(`${base}/api/v1/health`)).ok) break; } catch { /* not up yet */ }
    await sleep(200);
  }
  const keyFile = join(state, 'bootstrap-admin.key');
  for (let i = 0; i < 50 && !existsSync(keyFile); i++) await sleep(100);
  const key = readFileSync(keyFile, 'utf8').trim();
  return { proc, base, key, log: () => log };
}

export async function startChrome(profile) {
  const port = await freePort();
  const proc = spawn(findChrome(), [
    '--headless', `--remote-debugging-port=${port}`, `--user-data-dir=${profile}`, '--no-sandbox', '--disable-gpu',
    '--disable-dev-shm-usage', '--disable-background-networking', '--disable-sync', '--no-first-run',
    '--no-default-browser-check', '--window-size=1440,1000', 'about:blank'], { stdio: ['ignore', 'ignore', 'pipe'] });
  let err = ''; proc.stderr.on('data', (d) => { err += d; });
  for (let i = 0; i < 100; i++) {
    try {
      const v = await (await fetch(`http://127.0.0.1:${port}/json/version`)).json();
      if (v.webSocketDebuggerUrl) return { proc, wsUrl: v.webSocketDebuggerUrl, version: v.Browser };
    } catch { /* not listening yet */ }
    await sleep(150);
  }
  proc.kill('SIGKILL');
  throw new Error(`Chrome never exposed DevTools:\n${err.slice(-1500)}`);
}

export async function main(argv = process.argv.slice(2)) {
  const keepProfile = argv.includes('--keep-profile');
  const tmp = mkdtempSync(join(tmpdir(), 'nc-e2e-'));
  const failures = [];
  let backend; let chrome; let cdp;
  try {
    backend = await startBackend(tmp);
    chrome = await startChrome(join(tmp, 'profile'));
    cdp = await Cdp.connect(chrome.wsUrl);
    const { targetId } = await cdp.send('Target.createTarget', { url: 'about:blank' });
    const { sessionId } = await cdp.send('Target.attachToTarget', { targetId, flatten: true });
    const s = (m, p) => cdp.send(m, p, sessionId);
    await Promise.all([s('Page.enable'), s('Runtime.enable'), s('Log.enable'), s('Network.enable')]);

    let route = '(sign-in)';
    // Before the key is in sessionStorage the app's first calls answer 401 by design: not a failure.
    const problems = (msg) => { if (route !== '(sign-in)') failures.push(`${route}: ${msg}`); };
    const seenHeaders = new Map();
    cdp.on((m) => {
      if (m.sessionId !== sessionId) return;
      if (m.method === 'Runtime.exceptionThrown') {
        const d = m.params.exceptionDetails; problems(`uncaught exception: ${d.exception?.description || d.text}`);
      } else if (m.method === 'Runtime.consoleAPICalled' && m.params.type === 'error') {
        problems(`console.error: ${m.params.args.map((a) => a.value ?? a.description ?? '').join(' ').slice(0, 300)}`);
      } else if (m.method === 'Log.entryAdded' && m.params.entry.level === 'error') {
        problems(`browser error: ${m.params.entry.text.slice(0, 300)} ${m.params.entry.url || ''}`);
      } else if (m.method === 'Network.responseReceived') {
        const r = m.params.response; const u = new URL(r.url);
        if (u.origin === backend.base && !u.pathname.startsWith('/api/')) {
          const cc = Object.entries(r.headers).find(([k]) => k.toLowerCase() === 'cache-control')?.[1] || '';
          seenHeaders.set(u.pathname, cc);
        }
      }
    });

    const evaluate = async (expr) => (await s('Runtime.evaluate', { expression: expr, returnByValue: true, awaitPromise: true })).result.value;
    // Sign in the way a person does: the key goes into sessionStorage, then the app boots with it.
    await s('Page.navigate', { url: `${backend.base}/` });
    await sleep(500);
    await evaluate(`sessionStorage.setItem('nc.bearer', ${JSON.stringify(backend.key)}); true`);
    await s('Page.reload', { ignoreCache: false });
    await sleep(800);

    /** Wait until the page has rendered (no skeleton for SETTLE_MS) and report the visible error states. */
    const settle = async () => {
      const t0 = Date.now(); let quietSince = 0;
      while (Date.now() - t0 < PAGE_TIMEOUT_MS) {
        const st = await evaluate(`(() => {
          const main = document.querySelector('#main');
          const busy = !!document.querySelector('#main .skeleton, #main [aria-busy="true"]');
          const errs = [...document.querySelectorAll('#main .state.error')].filter((e) => e.offsetParent !== null)
            .map((e) => e.innerText.replace(/\\s+/g, ' ').slice(0, 240));
          const notFound = !!document.querySelector('#main > .state.empty');   // the router's own not-found state
          return { busy, errs, notFound, hasContent: !!main && main.children.length > 0 };
        })()`);
        if (st.errs.length || st.notFound) return st;
        if (!st.busy && st.hasContent) { quietSince ||= Date.now(); if (Date.now() - quietSince >= SETTLE_MS) return st; }
        else quietSince = 0;
        await sleep(100);
      }
      return { timeout: true, errs: [], notFound: false };
    };

    const paths = [...readFileSync(join(FRONTEND, 'js', 'routes.js'), 'utf8').matchAll(/path: '(\/[a-z]+)'/g)].map((m) => m[1]);
    if (paths.length < 10) throw new Error(`only ${paths.length} routes parsed from js/routes.js`);
    let pages = 0;
    for (const path of paths) {
      route = path;
      await evaluate(`location.hash = '#${path}'; true`);
      await sleep(150);
      let st = await settle(); pages++;
      if (st.timeout) problems('did not finish rendering');
      if (st.notFound) problems('shows the not-found state');
      for (const e of st.errs) problems(`error state: ${e}`);
      const tabs = await evaluate(`[...document.querySelectorAll('#main [role="tab"]')].map((b) => b.id)`);
      for (const id of tabs || []) {
        route = `${path} › ${id}`;
        await evaluate(`document.getElementById(${JSON.stringify(id)})?.click(); true`);
        await sleep(150);
        st = await settle(); pages++;
        if (st.timeout) problems('did not finish rendering');
        for (const e of st.errs) problems(`error state: ${e}`);
      }
    }
    route = '(static files)';
    for (const [p, cc] of seenHeaders) if (!/\bno-cache\b/.test(cc)) problems(`${p} served without Cache-Control: no-cache (got '${cc}')`);
    if (!seenHeaders.size) problems('no frontend file was observed (the page never loaded?)');

    console.log(`e2e: ${chrome.version}; ${paths.length} routes, ${pages} pages/tabs rendered, ${seenHeaders.size} static files checked`);
  } catch (e) {
    failures.push(`harness: ${e.message}`);
  } finally {
    cdp?.close();
    chrome?.proc.kill('SIGKILL');
    if (backend) {
      backend.proc.kill('SIGTERM');
      for (let i = 0; i < 50 && backend.proc.exitCode === null; i++) await sleep(100);
      if (backend.proc.exitCode === null) backend.proc.kill('SIGKILL');
    }
    if (!keepProfile) rmSync(tmp, { recursive: true, force: true });
  }
  if (failures.length) {
    console.error(`e2e: FAILED (${failures.length})`);
    for (const f of failures) console.error(`  - ${f}`);
    if (backend) console.error(`backend log tail:\n${backend.log().slice(-1500)}`);
    return 1;
  }
  console.log('e2e: all pages rendered with no error state, no console error, and revalidating static files');
  return 0;
}

if (import.meta.url === `file://${process.argv[1]}`) process.exit(await main());
