#!/usr/bin/env node
// Browser end-to-end check of Nemo Console: the REAL backend, a real headless Chrome, the same auth mode as a real
// install (loopback NOT trusted; an admin bearer in sessionStorage, exactly where the UI keeps it). Zero
// dependencies: Node 22's built-in WebSocket speaks the DevTools protocol (ADR 0013: no npm).
//
//   cd frontend && node e2e/run.mjs          (or `make e2e` from the repo root)
//
// The backend is the Rust binary: E2E_BIN overrides its path (default ../backend/target/debug/nemo-console; `make e2e`
// builds it first with `cargo build --locked`).
//
// Hermetic by default (backend/PORTING.md A-5): every upstream URL points at a CLOSED port
// (127.0.0.1:9) and PATH is an empty directory, so the run never reaches a live service (router :4000, Switchyard
// :4010, Relay :4040, Dynamo :8020/:8081, DCGM :9400, Prometheus :9090, Guardrails :8806) and never runs the host's
// docker / systemctl / nemo-relay; the run checks the effective URLs before it opens a page. E2E_LIVE=1 restores the
// old behaviour (the default URLs and the caller's PATH) for a deliberate run against a live lab.
//
// Needs: the built backend binary (make setup / make e2e) and a Chrome — $CHROME_BIN (`make e2e` also reads `git config nemo-console.chrome`),
// else google-chrome / chromium / chrome-headless-shell on PATH. Every page of the nav and every tab of every system
// page is opened; the run FAILS on:
//   * the error state ("Something went wrong", class `state error`) or the not-found state anywhere in the page,
//   * an uncaught exception, a console error, an API answer of 5xx, or a frontend file that does not load,
//   * a frontend file (html, js, css, json) served without `Cache-Control: no-cache` — a browser that reuses a stale
//     module next to a new one breaks the new pages (2026-10-24: five new views failed for a user that way).
// Systems that are not running are FINE: a down system must still render its page. The backend runs with a
// throwaway HOME, state dir and an EMPTY repo root, so nothing of the developer's config, keys or lab is read and a
// run in a deployment checkout sees what CI and a fresh clone see (no system deployed). An API 4xx is judged by
// what the page shows, not by the browser's network log line.
import { spawn } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, existsSync, mkdirSync, chmodSync, cpSync } from 'node:fs';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { visitRelayTraces } from './relay-traces.mjs';
import { visitVizPages, checkRealSeriesRoute } from './viz-pages.mjs';

const FRONTEND = dirname(dirname(fileURLToPath(import.meta.url)));
const BACKEND = join(FRONTEND, '..', 'backend');
const BIN = process.env.E2E_BIN || join(BACKEND, 'target', 'debug', 'nemo-console');
const LIVE = process.env.E2E_LIVE === '1';
const CLOSED_URL = 'http://127.0.0.1:9'; // discard port: nothing listens, a connection is refused at once
// Every key of the backend's DEFAULT_URLS (backend/crates/nemo-console/src/config.rs). A key missing here keeps its live
// default; the effective-URL check in startBackend catches that.
const URL_KEYS = ['switchyard', 'relay', 'dynamo', 'dynamo_worker_metrics', 'litellm', 'dcgm', 'prometheus', 'guardrails'];
const SETTLE_MS = 400; // the page must stay free of skeletons for this long to count as rendered
const PAGE_TIMEOUT_MS = 20000;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
// Synthetic Relay captures (backend/crates/nemo-console/tests/fixtures/relay_traces): ATOF event files and ATIF trajectories.
const RELAY_FIXTURES = join(FRONTEND, '..', 'backend', 'crates', 'nemo-console', 'tests', 'fixtures', 'relay_traces');

function findChrome() {
  if (process.env.CHROME_BIN) return process.env.CHROME_BIN;
  for (const dir of (process.env.PATH || '').split(':')) {
    for (const name of ['google-chrome', 'google-chrome-stable', 'chromium', 'chromium-browser', 'chrome-headless-shell']) {
      if (dir && existsSync(join(dir, name))) return join(dir, name);
    }
  }
  throw new Error('no Chrome found: set CHROME_BIN, or `git config nemo-console.chrome <path>`, or put google-chrome / chromium / chrome-headless-shell on PATH');
}

const freePort = () =>
  new Promise((resolve, reject) => {
    const s = createServer();
    s.once('error', reject);
    s.listen(0, '127.0.0.1', () => {
      const { port } = s.address();
      s.close(() => resolve(port));
    });
  });

/** Minimal DevTools client: one browser WebSocket, flat sessions. */
export class Cdp {
  constructor(ws) {
    this.ws = ws;
    this.id = 0;
    this.waiting = new Map();
    this.handlers = [];
  }
  static async connect(url) {
    const ws = new WebSocket(url);
    await new Promise((res, rej) => {
      ws.onopen = res;
      ws.onerror = () => rej(new Error(`cannot connect to ${url}`));
    });
    const c = new Cdp(ws);
    ws.onmessage = (m) => {
      const msg = JSON.parse(m.data);
      if (msg.id && c.waiting.has(msg.id)) {
        const { resolve, reject } = c.waiting.get(msg.id);
        c.waiting.delete(msg.id);
        msg.error ? reject(new Error(`${msg.error.message} (${msg.error.code})`)) : resolve(msg.result);
      } else if (msg.method) {
        for (const h of c.handlers) h(msg);
      }
    };
    return c;
  }
  send(method, params = {}, sessionId) {
    const id = ++this.id;
    this.ws.send(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) }));
    return new Promise((resolve, reject) => this.waiting.set(id, { resolve, reject }));
  }
  on(h) {
    this.handlers.push(h);
  }
  close() {
    try {
      this.ws.close();
    } catch {
      /* ignore */
    }
  }
}

/** Copy the synthetic Relay captures into a throwaway home, where the backend looks for them by default. */
export function seedRelayFixtures(home) {
  const share = join(home, '.local', 'share', 'nemo-relay');
  for (const dir of ['atof', 'atif']) {
    mkdirSync(join(share, dir), { recursive: true });
    cpSync(join(RELAY_FIXTURES, dir), join(share, dir), { recursive: true });
  }
}

/** Wait until the trace index has read the seeded files: state ready with at least one trace (null on timeout). */
async function waitIndexed({ base, key }, ms = 90000) {
  const t0 = Date.now();
  while (Date.now() - t0 < ms) {
    try {
      const r = await fetch(`${base}/api/v1/systems/relay/traces/index`, { headers: { Authorization: `Bearer ${key}` } });
      if (r.ok) {
        const ix = await r.json();
        if (ix.state === 'ready' && ix.traces > 0) return ix;
      }
    } catch {
      /* not answering yet */
    }
    await sleep(500);
  }
  return null;
}

// `seed(home)` runs on the throwaway HOME before the backend starts (see seedRelayFixtures).
export async function startBackend(tmp, { port, frontendDir, seed } = {}) {
  if (!existsSync(BIN)) throw new Error(`${BIN} missing: build it (make setup, or cd backend && cargo build --locked)`);
  port ||= await freePort();
  const home = join(tmp, 'home');
  const state = join(tmp, 'state');
  const repo = join(tmp, 'repo');
  const bin = join(tmp, 'bin'); // the hermetic PATH: empty, so every allowlisted CLI is "not found"
  for (const d of [home, state, repo, bin]) mkdirSync(d, { recursive: true });
  seed?.(home);
  chmodSync(state, 0o700);
  const env = {
    PATH: LIVE ? process.env.PATH || '/usr/bin:/bin' : bin,
    HOME: home,
    LANG: 'C.UTF-8',
    NEMO_CONSOLE_PORT: String(port),
    NEMO_CONSOLE_STATE_DIR: state,
    NEMO_CONSOLE_NO_USER_ENV: '1',
    NEMO_CONSOLE_TRUST_LOOPBACK: 'false',
    NEMO_CONSOLE_REPO_ROOT: repo,
    ...(LIVE ? {} : { NEMO_CONSOLE_URLS: JSON.stringify(Object.fromEntries(URL_KEYS.map((k) => [k, CLOSED_URL]))) }),
    ...(frontendDir ? { NEMO_CONSOLE_FRONTEND_DIR: frontendDir } : {}),
  };
  const proc = spawn(BIN, [], { cwd: BACKEND, env, stdio: ['ignore', 'pipe', 'pipe'] });
  let log = '';
  proc.stdout.on('data', (d) => {
    log += d;
  });
  proc.stderr.on('data', (d) => {
    log += d;
  });
  const base = `http://127.0.0.1:${port}`;
  for (let i = 0; i < 150; i++) {
    if (proc.exitCode !== null) throw new Error(`backend exited early (${proc.exitCode}):\n${log.slice(-2000)}`);
    try {
      if ((await fetch(`${base}/api/v1/health`)).ok) break;
    } catch {
      /* not up yet */
    }
    await sleep(200);
  }
  const keyFile = join(state, 'bootstrap-admin.key');
  for (let i = 0; i < 50 && !existsSync(keyFile); i++) await sleep(100);
  const key = readFileSync(keyFile, 'utf8').trim();
  if (!LIVE) {
    // Prove the run is hermetic before any page polls an upstream: every effective URL must be the closed port.
    let urls = null;
    try {
      const r = await fetch(`${base}/api/v1/settings`, { headers: { Authorization: `Bearer ${key}` } });
      urls = r.ok ? (await r.json()).urls : null;
    } catch {
      /* reported below */
    }
    const live = urls && typeof urls === 'object' ? Object.entries(urls).filter(([, u]) => u !== CLOSED_URL) : null;
    if (!live || live.length) {
      proc.kill('SIGKILL');
      throw new Error(
        `not hermetic: ${live ? live.map(([k, u]) => `${k}=${u}`).join(', ') : 'GET /api/v1/settings failed'}` + ' (add the key to URL_KEYS, or set E2E_LIVE=1 for a deliberate live run)',
      );
    }
  }
  return { proc, base, key, log: () => log };
}

export async function startChrome(profile) {
  const port = await freePort();
  const proc = spawn(
    findChrome(),
    [
      '--headless',
      `--remote-debugging-port=${port}`,
      `--user-data-dir=${profile}`,
      '--no-sandbox',
      '--disable-gpu',
      '--disable-dev-shm-usage',
      '--disable-background-networking',
      '--disable-sync',
      '--no-first-run',
      '--no-default-browser-check',
      '--window-size=1440,1000',
      'about:blank',
    ],
    { stdio: ['ignore', 'ignore', 'pipe'] },
  );
  let err = '';
  proc.stderr.on('data', (d) => {
    err += d;
  });
  for (let i = 0; i < 100; i++) {
    try {
      const v = await (await fetch(`http://127.0.0.1:${port}/json/version`)).json();
      if (v.webSocketDebuggerUrl) return { proc, wsUrl: v.webSocketDebuggerUrl, version: v.Browser };
    } catch {
      /* not listening yet */
    }
    await sleep(150);
  }
  proc.kill('SIGKILL');
  throw new Error(`Chrome never exposed DevTools:\n${err.slice(-1500)}`);
}

/** The frontend mock server (dependency-free, deterministic data: metrics, series, runs) on a free loopback port. */
async function startMock() {
  const port = await freePort();
  const proc = spawn(process.execPath, [join(FRONTEND, 'dev', 'mock-server.mjs'), '--port', String(port)], { env: { PATH: process.env.PATH || '' }, stdio: ['ignore', 'ignore', 'pipe'] });
  let err = '';
  proc.stderr.on('data', (d) => {
    err += d;
  });
  const base = `http://127.0.0.1:${port}`;
  for (let i = 0; i < 100; i++) {
    if (proc.exitCode !== null) throw new Error(`the mock server exited early (${proc.exitCode}): ${err.slice(-500)}`);
    try {
      if ((await fetch(`${base}/api/v1/health`)).ok) return { proc, base };
    } catch {
      /* not up yet */
    }
    await sleep(100);
  }
  proc.kill('SIGKILL');
  throw new Error('the mock server never answered');
}

export async function main(argv = process.argv.slice(2)) {
  const keepProfile = argv.includes('--keep-profile');
  const tmp = mkdtempSync(join(tmpdir(), 'nc-e2e-'));
  const failures = [];
  let backend;
  let seeded = null;
  let mock = null; // the frontend mock server (dev/mock-server.mjs): the wave-1 chart and run-history checks need metrics and runs
  let chrome;
  let cdp;
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
    const problems = (msg) => {
      if (route !== '(sign-in)') failures.push(`${route}: ${msg}`);
    };
    const seenHeaders = new Map();
    cdp.on((m) => {
      if (m.sessionId !== sessionId) return;
      if (m.method === 'Runtime.exceptionThrown') {
        const d = m.params.exceptionDetails;
        problems(`uncaught exception: ${d.exception?.description || d.text}`);
      } else if (m.method === 'Runtime.consoleAPICalled' && m.params.type === 'error') {
        problems(
          `console.error: ${m.params.args
            .map((a) => a.value ?? a.description ?? '')
            .join(' ')
            .slice(0, 300)}`,
        );
      } else if (m.method === 'Log.entryAdded' && m.params.entry.level === 'error' && m.params.entry.source !== 'network') {
        problems(`browser error: ${m.params.entry.text.slice(0, 300)} ${m.params.entry.url || ''}`); // network: below
      } else if (m.method === 'Network.responseReceived') {
        const r = m.params.response;
        const u = new URL(r.url);
        if (u.origin !== backend.base && u.origin !== seeded?.base && u.origin !== mock?.base) return;
        if (u.pathname.startsWith('/api/')) {
          if (r.status >= 500) problems(`HTTP ${r.status} from ${u.pathname}`);
          return;
        }
        if (r.status >= 400) problems(`frontend file ${u.pathname} answered ${r.status}`);
        if (u.origin === mock?.base) return; // the mock serves its files `no-store`: the revalidation check is about the real backend
        seenHeaders.set(u.pathname, Object.entries(r.headers).find(([k]) => k.toLowerCase() === 'cache-control')?.[1] || '');
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
      const t0 = Date.now();
      let quietSince = 0;
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
        if (!st.busy && st.hasContent) {
          quietSince ||= Date.now();
          if (Date.now() - quietSince >= SETTLE_MS) return st;
        } else quietSince = 0;
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
      let st = await settle();
      pages++;
      if (st.timeout) problems('did not finish rendering');
      if (st.notFound) problems('shows the not-found state');
      for (const e of st.errs) problems(`error state: ${e}`);
      const tabs = await evaluate(`[...document.querySelectorAll('#main [role="tab"]')].map((b) => b.id)`);
      for (const id of tabs || []) {
        route = `${path} › ${id}`;
        await evaluate(`document.getElementById(${JSON.stringify(id)})?.click(); true`);
        await sleep(150);
        st = await settle();
        pages++;
        if (st.timeout) problems('did not finish rendering');
        for (const e of st.errs) problems(`error state: ${e}`);
      }
    }
    // The same browser, a second backend whose home holds the synthetic Relay captures: the five trace explorer tabs
    // and one detail view of each, against data (e2e/relay-traces.mjs). The first pass above already opened every one of
    // those tabs on an EMPTY home, which is the other state a fresh install is in.
    route = 'relay traces (sign-in)';
    seeded = await startBackend(join(tmp, 'seeded'), { seed: seedRelayFixtures });
    await s('Page.navigate', { url: `${seeded.base}/` });
    await sleep(500);
    await evaluate(`sessionStorage.setItem('nc.bearer', ${JSON.stringify(seeded.key)}); true`);
    await s('Page.reload', { ignoreCache: false });
    await sleep(800);
    route = 'relay traces (index)';
    if (!(await waitIndexed(seeded))) problems('the trace index never became ready with a trace from the synthetic captures');
    else await visitRelayTraces({ evaluate, settle, sleep, fail: (msg) => problems(msg), route: (name) => (route = name) });
    // The real backend's series route (addendum 9.13), then the same browser against the frontend mock server: the charts (rates,
    // 7d, show max, the line picker) and the run history of the eight run-based pages, which need metrics and runs the hermetic
    // home does not have (e2e/viz-pages.mjs).
    route = 'series route (real backend)';
    await checkRealSeriesRoute(backend, (msg) => problems(msg));
    route = 'viz pages (mock sign-in)';
    mock = await startMock();
    await s('Page.navigate', { url: `${mock.base}/` });
    await sleep(800);
    await visitVizPages({ base: mock.base, evaluate, settle, sleep, fail: (msg) => problems(msg), route: (name) => (route = name) });
    route = '(static files)';
    for (const [p, cc] of seenHeaders) if (!/\bno-cache\b/.test(cc)) problems(`${p} served without Cache-Control: no-cache (got '${cc}')`);
    if (!seenHeaders.size) problems('no frontend file was observed (the page never loaded?)');

    console.log(
      `e2e: backend ${BIN.replace(/^.*\/backend\//, 'backend/')} (${LIVE ? 'LIVE upstreams and PATH' : 'hermetic'}); ${chrome.version}; ${paths.length} routes, ${pages} pages/tabs rendered, ${seenHeaders.size} static files checked`,
    );
  } catch (e) {
    failures.push(`harness: ${e.message}`);
  } finally {
    cdp?.close();
    chrome?.proc.kill('SIGKILL');
    if (mock) {
      mock.proc.kill('SIGTERM');
      for (let i = 0; i < 20 && mock.proc.exitCode === null; i++) await sleep(100);
      if (mock.proc.exitCode === null) mock.proc.kill('SIGKILL');
    }
    for (const b of [backend, seeded]) {
      if (!b) continue;
      b.proc.kill('SIGTERM');
      for (let i = 0; i < 50 && b.proc.exitCode === null; i++) await sleep(100);
      if (b.proc.exitCode === null) b.proc.kill('SIGKILL');
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
