#!/usr/bin/env node
// Differential parity harness: the last Python backend (the reference, parity/ref.mjs) vs the Rust backend (or a
// backend against itself), side by side, with IDENTICAL hermetic setups. Node 22, no npm dependencies.
//
//   node parity/run.mjs                                   (= --a python --b rust: the reference vs this checkout)
//   node parity/run.mjs --a python@<tag or commit> --b rust [--rust-bin PATH] --allow-diff-routes parity/allow-diffs.txt
//   node parity/run.mjs --a rust --b rust                 (determinism of the Rust backend)
//   kinds: rust | python (= python@DEFAULT_REF of ref.mjs) | python@<tag or commit> (extracted + venv on first use)
//   options: --scenario full|empty (repeatable; default all)   --filter <substring of case id or route>
//            --only <regex on case id or route> (repeatable; ANDed with --filter)
//            --allow-diff-routes <file>   (lines "METHOD /template", a case id, or "stub:<system_id>" = every case whose
//                                          request path names only that system: diffs there do not fail the run)
//            --keep (keep the temp dirs)  --tmp <dir> (parent of the temp dirs; default os.tmpdir())  --list
//            --dump (also write out/dump-<scenario>.json: A's normalised answer, argv and upstream calls per case)
//
// Every case of parity/corpus runs on both backends in the same fixed order (each case is sent to both at the
// same moment, so the TTL caches of both age alike). Compared: status, chosen headers, the body (JSON as values with
// number kinds kept, SSE as a list of events, Prometheus text as normalised lines, other text exactly), the argv of
// every fake CLI the backend ran during the case and the requests it sent to the fake upstreams. The normalisation
// rules are all in corpus/normalise.mjs. Writes parity/out/report.json; exit 0 only when nothing differs.
import { mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { startBackend, stopBackend, readCliLog, runCli } from './lib/servers.mjs';
import { send, parseSse } from './lib/http.mjs';
import { parse, pointer, toPlain, stringify } from './lib/json.mjs';
import { compareTrees, comparePlain, compareSets } from './lib/compare.mjs';
import { PARITY } from './lib/seed.mjs';
import { makeNormaliser, isCachedArgv, isCachedHttp } from './corpus/normalise.mjs';
import { SCENARIOS, casesFor } from './corpus/index.mjs';

const CHOSEN_HEADERS = ['content-type', 'cache-control', 'etag', 'last-modified', 'content-security-policy',
  'x-content-type-options', 'referrer-policy', 'x-frame-options', 'permissions-policy', 'cross-origin-opener-policy',
  'cross-origin-resource-policy', 'x-served-by-route', 'x-served-by-tier', 'x-accel-buffering', 'allow',
  'content-disposition', 'location', 'www-authenticate'];
const BAD_KEY = 'nck_parityNotAKey0000000000000000000000000000';

function parseArgs(argv) {
  const o = { a: 'python', b: 'rust', rustBin: undefined, scenarios: [], filter: [], only: [], allowFile: undefined, keep: false, tmp: tmpdir(), list: false };
  for (let i = 0; i < argv.length; i++) {
    const k = argv[i];
    const v = () => { if (i + 1 >= argv.length) throw new Error(`${k} needs a value`); return argv[++i]; };
    if (k === '--a') o.a = v();
    else if (k === '--b') o.b = v();
    else if (k === '--rust-bin') o.rustBin = v();
    else if (k === '--scenario') o.scenarios.push(v());
    else if (k === '--filter') o.filter.push(v());
    else if (k === '--only') o.only.push(new RegExp(v()));
    else if (k === '--allow-diff-routes') o.allowFile = v();
    else if (k === '--keep') o.keep = true;
    else if (k === '--tmp') o.tmp = v();
    else if (k === '--list') o.list = true;
    else if (k === '--dump') o.dump = true;
    else if (k === '-h' || k === '--help') { console.log(readFileSync(new URL(import.meta.url), 'utf8').split('\n').slice(1, 20).join('\n')); process.exit(0); }
    else throw new Error(`unknown option ${k}`);
  }
  for (const s of o.scenarios) if (!SCENARIOS[s]) throw new Error(`unknown scenario ${s} (${Object.keys(SCENARIOS).join(', ')})`);
  if (!o.scenarios.length) o.scenarios = Object.keys(SCENARIOS);
  return o;
}

function readAllowList(file) {
  const lines = new Set(file ? readFileSync(file, 'utf8').split('\n').map((l) => l.replace(/#.*/, '').trim()).filter(Boolean)
    .map((l) => l.replace(/\s+/g, ' ')) : []);
  const stubs = new Set([...lines].filter((l) => l.startsWith('stub:')).map((l) => l.slice(5)));
  return {
    has: (x) => lines.has(x),
    /** A case on a per-system route of a system that is still a stub: every request path of the case names only
     *  systems listed as `stub:<id>` (the id decoded as the router decodes it; unknown or mis-cased ids never match). */
    stubCase: (c) => {
      const paths = c.concurrent ? c.concurrent.map((sub) => sub.path ?? c.path) : [c.path];
      const ids = paths.map((p) => {
        const m = typeof p === 'string' ? /^\/api\/v1\/systems\/([^/?#]+)/.exec(p) : null;
        if (!m) return null;
        try { return decodeURIComponent(m[1]); } catch { return null; }
      });
      return ids.length > 0 && ids.every((id) => id !== null && stubs.has(id));
    },
  };
}

class MissingVar extends Error {}

/** Replace {{name}} in strings with this side's values (captured vars + built-ins). A var this side never captured
 *  (its earlier answer differed) falls back to the OTHER side's value, so the case still runs on both and the
 *  difference shows where it matters; the fallback is reported. Missing on both sides -> MissingVar (case skipped). */
function substitute(value, side) {
  if (typeof value === 'string') {
    return value.replace(/\{\{(\w+)\}\}/g, (m, k) => {
      if (k in side.vars) return String(side.vars[k]);
      if (side.other && k in side.other.vars) { side.fallbacks.add(k); return String(side.other.vars[k]); }
      throw new MissingVar(k);
    });
  }
  if (Array.isArray(value)) return value.map((x) => substitute(x, side));
  if (value && typeof value === 'object' && !Buffer.isBuffer(value)) {
    return Object.fromEntries(Object.entries(value).map(([k, v]) => [substitute(k, side), substitute(v, side)]));
  }
  return value;
}

/** Wait until the wall clock is between 50 and 700 ms into a second (see the action note in main). */
async function clearOfSecondBoundary() {
  const ms = Date.now() % 1000;
  if (ms < 50 || ms > 700) await new Promise((r) => setTimeout(r, (1050 - ms) % 1000));
}

function buildRequest(c, side) {
  const method = c.method || 'GET';
  const headers = {};
  if (c.host !== null) headers.Host = c.host ? substitute(c.host, side) : `127.0.0.1:${side.port}`;
  const auth = c.auth ?? 'admin';
  if (auth === 'admin') headers.Authorization = `Bearer ${side.adminKey}`;
  else if (auth === 'viewer') headers.Authorization = `Bearer ${side.viewerKey}`;
  else if (auth === 'bad') headers.Authorization = `Bearer ${BAD_KEY}`;
  else if (auth.startsWith('raw:')) headers.Authorization = auth.slice(4);
  else if (auth !== 'none') throw new Error(`case ${c.id}: bad auth ${auth}`);
  if (!['GET', 'HEAD', 'OPTIONS'].includes(method) && c.csrf !== false) headers['X-Console'] = '1';
  Object.assign(headers, substitute(c.headers || {}, side));
  let body;
  if (c.json !== undefined) {
    body = JSON.stringify(substitute(c.json, side));
    if (!Object.keys(headers).some((h) => h.toLowerCase() === 'content-type')) headers['Content-Type'] = 'application/json';
  } else if (c.body !== undefined) {
    body = typeof c.body === 'function' ? c.body(side) : substitute(c.body, side);
    if (c.contentType) headers['Content-Type'] = c.contentType;
  }
  return { method, path: substitute(c.path, side), headers, body, chunked: !!c.chunked, readMs: c.readMs, timeoutMs: c.timeoutMs };
}

/** Per-case shaping of a normalised view (corpus flags; each is a documented normalisation rule):
 *  collapseKeepAlive: consecutive `: keep-alive` comments count as one (their number depends on scheduling jitter);
 *  historyShape: a metrics-history answer becomes {nonEmpty, values: distinct values} (the number of poller ticks and
 *  their timestamps depend on wall-clock timing; WHAT was recorded must not). */
function shape(c, view) {
  const b = view.body;
  if (c.collapseKeepAlive && b.kind === 'sse') {
    b.events = b.events.filter((e, i, all) => !(e.comment === 'keep-alive' && i > 0 && all[i - 1].comment === 'keep-alive'));
  }
  if (c.historyShape && b.kind === 'json' && b.tree.k === 'obj' && b.tree.v.get('points')?.k === 'arr') {
    const pts = b.tree.v.get('points').v;
    const seen = new Map();
    for (const p of pts) if (p.k === 'arr' && p.v[1]) seen.set(stringify(p.v[1]), p.v[1]);
    const values = [...seen.entries()].sort(([x], [y]) => (x < y ? -1 : x > y ? 1 : 0)).map(([, n]) => n);
    b.tree.v.set('points', { k: 'obj', v: new Map([['nonEmpty', { k: 'bool', v: pts.length > 0 }], ['values', { k: 'arr', v: values }]]) });
  }
  return view;
}

/** Normalised view of one response: {status, headers, body:{kind, ...}} */
function analyse(res, norm) {
  const out = { status: res.status, error: res.error ? res.error.replace(/\d{2,5}/g, '<n>') : undefined, headers: {} };
  for (const h of CHOSEN_HEADERS) if (res.headers[h] !== undefined) out.headers[h] = h === 'etag' || h === 'last-modified' ? res.headers[h] : norm.str(res.headers[h]);
  const ct = (res.headers['content-type'] || '').toLowerCase();
  const text = res.body.toString('utf8');
  if (!res.body.length) out.body = { kind: 'empty' };
  else if (ct.startsWith('text/event-stream')) {
    out.body = { kind: 'sse', events: parseSse(text).map((ev) => {
      const e = { ...ev };
      if (e.data !== undefined) {
        try { e.data = norm.json(parse(e.data)); e.json = true; } catch { e.data = norm.str(e.data); }
      }
      return e;
    }) };
  } else if (ct.includes('json')) {
    try { out.body = { kind: 'json', tree: norm.json(parse(text)) }; } catch (e) { out.body = { kind: 'text', text: norm.str(text), note: `invalid JSON: ${e.message}` }; }
  } else if (ct.startsWith('text/plain') && /version=0\.0\.4|^# (HELP|TYPE) /m.test(ct + '\n' + text)) {
    out.body = { kind: 'prometheus', lines: norm.prometheus(text) };
  } else if (/^(text\/html|text\/css|text\/javascript|application\/javascript|image\/)/.test(ct)) {
    out.body = { kind: 'bytes', length: res.body.length, text };                 // static files: byte-exact
  } else {
    out.body = { kind: 'text', text: norm.str(text) };
  }
  return out;
}

function compareResponses(ra, rb) {
  const diffs = [];
  comparePlain(ra.status, rb.status, 'status', diffs);
  if (ra.error || rb.error) comparePlain(ra.error, rb.error, 'transport-error', diffs);
  for (const h of CHOSEN_HEADERS) comparePlain(ra.headers[h], rb.headers[h], `header:${h}`, diffs);
  const ba = ra.body; const bb = rb.body;
  if (ba.kind !== bb.kind) { diffs.push({ path: 'body-kind', a: ba.kind, b: bb.kind }); return diffs; }
  if (ba.kind === 'json') {
    for (const d of compareTrees(ba.tree, bb.tree)) diffs.push({ ...d, path: `body${d.path}` });
  } else if (ba.kind === 'sse') {
    const n = Math.max(ba.events.length, bb.events.length);
    for (let i = 0; i < n; i++) {
      const ea = ba.events[i]; const eb = bb.events[i];
      if (!ea || !eb) { diffs.push({ path: `sse/${i}`, a: ea ? JSON.stringify(ea) : '(absent)', b: eb ? JSON.stringify(eb) : '(absent)' }); continue; }
      for (const f of new Set([...Object.keys(ea), ...Object.keys(eb)])) {
        if (f === 'data' && ea.json && eb.json) for (const d of compareTrees(ea.data, eb.data)) diffs.push({ ...d, path: `sse/${i}/data${d.path}` });
        else comparePlain(ea[f], eb[f], `sse/${i}/${f}`, diffs);
      }
    }
  } else if (ba.kind === 'prometheus') {
    compareSets(ba.lines, bb.lines, 'body:prometheus-lines', diffs);
  } else if (ba.kind === 'bytes') {
    if (ba.text !== bb.text) {
      const la = ba.text.split('\n'); const lb = bb.text.split('\n');
      const i = la.findIndex((l, k) => l !== lb[k]);
      diffs.push({ path: `body:bytes (length ${ba.length} vs ${bb.length}), first differing line ${i + 1}`, a: (la[i] ?? '(eof)').slice(0, 200), b: (lb[i] ?? '(eof)').slice(0, 200) });
    }
  } else if (ba.kind === 'text') {
    comparePlain(ba.text, bb.text, 'body:text', diffs);
  }
  return diffs;
}

/** A response view for --dump: plain JSON (number kinds shown by canonical text) instead of tagged trees. */
function dumpView(r) {
  const b = r.body;
  let body;
  if (b.kind === 'json') body = JSON.parse(stringify(b.tree));
  else if (b.kind === 'sse') body = b.events.map((e) => (e.json ? { ...e, data: JSON.parse(stringify(e.data)) } : e));
  else if (b.kind === 'prometheus') body = b.lines;
  else if (b.kind === 'bytes') body = `<${b.length} bytes>`;
  else body = b.text;
  return { status: r.status, headers: r.headers, body };
}

function plainCapture(res, spec) {
  if (typeof spec === 'object' && spec.header) return res.headers[spec.header];
  const ct = (res.headers['content-type'] || '').toLowerCase();
  if (!ct.includes('json')) return undefined;
  try {
    const node = pointer(parse(res.body.toString('utf8')), spec);
    if (node === undefined) return undefined;
    const v = toPlain(node);
    return typeof v === 'object' ? JSON.stringify(v) : v;
  } catch { return undefined; }
}

/** Build the request (or CLI args) for one side first: a missing {{var}} throws MissingVar before anything is sent. */
function prepare(c, side) {
  if (c.kind === 'cli') return { args: substitute(c.args, side) };
  if (c.concurrent) return { subs: c.concurrent.map((sub) => buildRequest({ ...c, concurrent: undefined, ...sub }, side)) };
  return buildRequest(c, side);
}

async function runCase(c, side, req) {
  const t0 = Date.now();
  const cli0 = readCliLog(side.b).length;
  const up0 = side.b.upstreams ? side.b.upstreams.log.length : 0;
  let res;
  let cliOut;
  if (c.kind === 'cli') {
    cliOut = await runCli(side.b, req.args);
  } else if (req.subs) {
    // several requests in flight at once (lock / connection-cap behaviour); sub i starts after its delayMs
    res = await Promise.all(req.subs.map(async (r, i) => {
      await new Promise((ok) => setTimeout(ok, c.concurrent[i].delayMs || 0));
      return send(side.port, r);
    }));
  } else {
    const during = c.during ? new Promise((r) => setTimeout(() => { try { c.during.run(side); } catch (e) { side.notes.push(`${c.id}: during hook failed: ${e.message}`); } r(); }, c.during.afterMs || 0)) : null;
    res = await send(side.port, req);
    if (during) await during;
  }
  if (c.settleMs) await new Promise((r) => setTimeout(r, c.settleMs));
  let exit;
  if (c.waitExit) {
    const proc = side.b.proc;
    const gone = () => proc.exitCode !== null || proc.signalCode !== null;
    if (!gone()) await Promise.race([new Promise((r) => proc.once('exit', r)), new Promise((r) => setTimeout(r, 20_000))]);
    exit = gone() ? (proc.exitCode ?? `signal ${proc.signalCode}`) : 'still running after 20 s';
  }
  if (c.after) await c.after(side);
  const cli = readCliLog(side.b).slice(cli0);
  const http = side.b.upstreams ? side.b.upstreams.log.slice(up0) : [];
  return { res, cliOut, cli, http, exit, ms: Date.now() - t0 };
}

function sideCtx(b, root) {
  let realRoot;
  try { realRoot = realpathSync(root); } catch { realRoot = root; }
  const upstreamPorts = b.upstreams ? b.upstreams.ports : Object.fromEntries(Object.entries(b.urls).map(([n, u]) => [n, Number(new URL(u).port)]));
  const norm = makeNormaliser({ root, realRoot, port: b.port, upstreamPorts, version: b.version });
  return { b, port: b.port, adminKey: b.adminKey, viewerKey: b.viewerKey, norm, notes: [], root,
    vars: { port: b.port, origin: `http://127.0.0.1:${b.port}`, home: b.paths.home, repo: b.paths.repo, state: b.paths.state, adminKeyRaw: b.adminKey },
    allCli: new Set(), allHttp: new Set(), unknownCli: [], unknownHttp: [], fallbacks: new Set() };
}

async function runScenario(name, opts, allow, report) {
  const scenario = SCENARIOS[name];
  const cases = casesFor(name).filter((c) => !opts.filter.length || opts.filter.some((f) => c.id.includes(f) || c.route.includes(f)))
    .filter((c) => !opts.only.length || opts.only.some((re) => re.test(c.id) || re.test(c.route)));
  mkdirSync(opts.tmp, { recursive: true });
  const tmp = mkdtempSync(join(opts.tmp, `nc-parity-${name}-`));
  const out = { scenario: name, a: opts.a, b: opts.b, cases: 0, equal: 0, skipped: [], diffs: [], timings: {} };
  const dump = [];
  let A; let B;
  try {
    // one after the other: a fake upstream of one side binding port 0 must not grab the other side's console port
    A = await startBackend({ kind: opts.a, rustBin: opts.rustBin, sideDir: join(tmp, 'a'), scenario, label: `A(${opts.a})` });
    B = await startBackend({ kind: opts.b, rustBin: opts.rustBin, sideDir: join(tmp, 'b'), scenario, label: `B(${opts.b})` });
    const sa = sideCtx(A, join(tmp, 'a'));
    const sb = sideCtx(B, join(tmp, 'b'));
    sa.other = sb; sb.other = sa;
    // the mint-key run of the setup is itself a CLI parity check
    const mintDiffs = [];
    comparePlain(A.mintOutput.code, B.mintOutput.code, 'mint-key viewer: exit code', mintDiffs);
    comparePlain(sa.norm.str(A.mintOutput.stdout), sb.norm.str(B.mintOutput.stdout), 'mint-key viewer: stdout', mintDiffs);
    if (mintDiffs.length) out.diffs.push({ id: 'setup:mint-key-viewer', route: 'CLI mint-key', diffs: mintDiffs });
    const t0 = Date.now();
    for (const c of cases) {
      out.cases++;
      let qa; let qb;
      sa.fallbacks.clear(); sb.fallbacks.clear();
      try {
        qa = prepare(c, sa);
        qb = prepare(c, sb);
      } catch (e) {
        if (!(e instanceof MissingVar)) throw e;
        out.skipped.push({ id: c.id, why: `needs {{${e.message}}} from an earlier case (filtered out?)` });
        continue;
      }
      const fallbackNote = [...[...sa.fallbacks].map((k) => `A used B's {{${k}}}`), ...[...sb.fallbacks].map((k) => `B used A's {{${k}}}`)];
      if (c.pauseMs) await new Promise((r) => setTimeout(r, c.pauseMs));      // let wall-clock TTL caches expire on both
      // An action may stamp a run id. Python's stamp reads the clock twice (seconds, then milliseconds), so near a second
      // boundary its id can fall up to a second behind and sort out of order; Rust reads it once (PORTING D-30). Send
      // actions only well inside a second, so both stamps land in the same second as the request.
      if (/\/actions\//.test(c.path || '')) await clearOfSecondBoundary();
      const [pa, pb] = await Promise.all([runCase(c, sa, qa), runCase(c, sb, qb)]);
      out.timings[c.id] = [pa.ms, pb.ms];
      const diffs = [];
      if (c.kind === 'cli') {
        comparePlain(pa.cliOut.code, pb.cliOut.code, 'exit code', diffs);
        comparePlain(sa.norm.str(pa.cliOut.stdout), sb.norm.str(pb.cliOut.stdout), 'stdout', diffs);
        // stderr only when the command failed (its message is then what the operator sees); on success it carries
        // only log lines, whose wording is not part of the CLI contract
        if (pa.cliOut.code !== 0 || pb.cliOut.code !== 0) comparePlain(sa.norm.str(pa.cliOut.stderr), sb.norm.str(pb.cliOut.stderr), 'stderr', diffs);
      } else if (c.concurrent) {
        pa.res.forEach((r, i) => {
          for (const d of compareResponses(analyse(r, sa.norm), analyse(pb.res[i], sb.norm))) diffs.push({ ...d, path: `[${i}] ${d.path}` });
        });
      } else {
        const ra = shape(c, analyse(pa.res, sa.norm));
        const rb = shape(c, analyse(pb.res, sb.norm));
        diffs.push(...compareResponses(ra, rb));
        if (c.waitExit) comparePlain(pa.exit, pb.exit, 'process exit after the case', diffs);
        if (c.capture) {
          for (const [v, spec] of Object.entries(c.capture)) {
            const va = plainCapture(pa.res, spec); const vb = plainCapture(pb.res, spec);
            if (va !== undefined) sa.vars[v] = va;
            if (vb !== undefined) sb.vars[v] = vb;
          }
        }
      }
      // fake CLI argv and upstream requests made during the case
      const cliA = pa.cli.map((r) => sa.norm.cliRecord(r)); const cliB = pb.cli.map((r) => sb.norm.cliRecord(r));
      for (const x of cliA) sa.allCli.add(x);
      for (const x of cliB) sb.allCli.add(x);
      for (const r of pa.cli) if (r.unknown) sa.unknownCli.push({ case: c.id, argv: sa.norm.str(r.argv.join(' ')) });
      for (const r of pb.cli) if (r.unknown) sb.unknownCli.push({ case: c.id, argv: sb.norm.str(r.argv.join(' ')) });
      const argvKey = (s) => JSON.parse(s).argv.join(' ');
      // a scenario with background tasks (poller, probe) makes calls between cases: only the whole-run sets compare
      if (!scenario.wholeRunCallsOnly) compareSets([...new Set(cliA.filter((x) => !isCachedArgv(argvKey(x))))], [...new Set(cliB.filter((x) => !isCachedArgv(argvKey(x))))], 'cli-argv (set, cached probes excluded)', diffs);
      const httpA = pa.http.map((r) => sa.norm.httpRecord(r)); const httpB = pb.http.map((r) => sb.norm.httpRecord(r));
      for (const x of httpA) sa.allHttp.add(x);
      for (const x of httpB) sb.allHttp.add(x);
      for (const r of pa.http) if (r.unknown) sa.unknownHttp.push({ case: c.id, req: `${r.server} ${r.method} ${r.path}` });
      for (const r of pb.http) if (r.unknown) sb.unknownHttp.push({ case: c.id, req: `${r.server} ${r.method} ${r.path}` });
      const httpKey = (s) => { const o = JSON.parse(s); return `${o.server} ${o.method} ${o.path}`; };
      if (!scenario.wholeRunCallsOnly) compareSets([...new Set(httpA.filter((x) => !isCachedHttp(httpKey(x))))], [...new Set(httpB.filter((x) => !isCachedHttp(httpKey(x))))], 'upstream-requests (set, cached excluded)', diffs);
      if (opts.dump) {
        const view = c.kind === 'cli' ? { code: pa.cliOut.code, stdout: sa.norm.str(pa.cliOut.stdout), stderr: sa.norm.str(pa.cliOut.stderr) }
          : c.concurrent ? { status: pa.res.map((r) => r.status), body: pa.res.map((r) => dumpView(analyse(r, sa.norm)).body) } : dumpView(analyse(pa.res, sa.norm));
        dump.push({ id: c.id, route: c.route, ...view, ...(c.waitExit ? { exit: pa.exit } : {}), argv: cliA.map((x) => JSON.parse(x).argv.join(' ')), upstream: httpA.map((x) => { const o = JSON.parse(x); return `${o.server} ${o.method} ${o.path}`; }) });
      }
      if (fallbackNote.length) diffs.push({ path: 'captured variable (an earlier answer differed)', a: fallbackNote.join('; '), b: '' });
      if (diffs.length) out.diffs.push({ id: c.id, route: c.route, allowed: allow.has(c.route) || allow.has(c.id) || allow.stubCase(c), diffs });
      else out.equal++;
      if (process.env.PARITY_PROGRESS) process.stderr.write(`${diffs.length ? 'x' : '.'}`);
    }
    // over the whole run: the distinct argv / upstream requests (cached probes included) must be the same. Built from
    // the COMPLETE logs, so calls made between cases (background tasks: poller, capable probe) are counted too.
    for (const [side, b] of [[sa, A], [sb, B]]) {
      for (const r of readCliLog(b)) side.allCli.add(side.norm.cliRecord(r));
      if (b.upstreams) for (const r of b.upstreams.log) side.allHttp.add(side.norm.httpRecord(r));
    }
    const gd = [];
    // with background tasks, adapter-internal caches decide which CLI calls a poller tick makes: HTTP set only
    if (!scenario.wholeRunCallsOnly) compareSets([...sa.allCli], [...sb.allCli], 'cli-argv over the whole run (distinct)', gd);
    compareSets([...sa.allHttp], [...sb.allHttp], 'upstream-requests over the whole run (distinct)', gd);
    if (gd.length) {
      const route = 'GLOBAL argv/upstream sets';
      out.diffs.push({ id: `global:${name}`, route, allowed: allow.has(route) || allow.has(`global:${name}`), diffs: gd });
    }
    out.unknownCli = { a: sa.unknownCli, b: sb.unknownCli };
    out.unknownHttp = { a: sa.unknownHttp, b: sb.unknownHttp };
    out.notes = [...sa.notes, ...sb.notes];
    out.seconds = Math.round((Date.now() - t0) / 1000);
    if (opts.dump) writeFileSync(join(PARITY, 'out', `dump-${name}.json`), JSON.stringify(dump, null, 1));
  } finally {
    await Promise.all([A && stopBackend(A), B && stopBackend(B)].filter(Boolean));
    if (A && B && process.env.PARITY_SERVER_LOGS) {
      writeFileSync(join(PARITY, 'out', `server-${name}-a.log`), A.log);
      writeFileSync(join(PARITY, 'out', `server-${name}-b.log`), B.log);
    }
    if (!opts.keep) rmSync(tmp, { recursive: true, force: true });
    else out.tmp = tmp;
  }
  report.scenarios.push(out);
  return out;
}

async function main() {
  const opts = parseArgs(process.argv.slice(2));
  if (opts.list) {
    for (const s of opts.scenarios) for (const c of casesFor(s)) console.log(`${s}\t${c.id}\t${c.route}`);
    return 0;
  }
  const allow = readAllowList(opts.allowFile);
  mkdirSync(join(PARITY, 'out'), { recursive: true });
  const report = { generated_at: new Date().toISOString(), a: opts.a, b: opts.b, rust_bin: opts.rustBin, filter: opts.filter, scenarios: [] };
  let failed = false;
  for (const s of opts.scenarios) {
    const r = await runScenario(s, opts, allow, report);
    const hard = r.diffs.filter((d) => !d.allowed);
    console.log(`\nscenario ${s}: ${r.cases} cases, ${r.equal} equal, ${r.diffs.length} with diffs` +
      `${r.diffs.length - hard.length ? ` (${r.diffs.length - hard.length} on allowed routes)` : ''}, ${r.skipped.length} skipped, ${r.seconds}s`);
    const byRoute = new Map();
    for (const d of r.diffs) byRoute.set(d.route, [...(byRoute.get(d.route) || []), d]);
    for (const [route, ds] of [...byRoute].sort()) {
      console.log(`  ${ds[0].allowed ? '[allowed] ' : ''}${route}: ${ds.length} case(s)`);
      for (const d of ds.slice(0, 4)) {
        console.log(`    - ${d.id}`);
        for (const x of d.diffs.slice(0, 3)) console.log(`        ${x.path}: A=${String(typeof x.a === 'string' ? x.a : JSON.stringify(x.a)).slice(0, 160)} | B=${String(typeof x.b === 'string' ? x.b : JSON.stringify(x.b)).slice(0, 160)}`);
      }
    }
    for (const sk of r.skipped) console.log(`  skipped ${sk.id}: ${sk.why}`);
    const unk = [...r.unknownCli.a, ...r.unknownCli.b];
    if (unk.length) console.log(`  note: ${unk.length} fake-CLI call(s) had no scripted answer (see report.json unknownCli)`);
    const unkH = [...r.unknownHttp.a, ...r.unknownHttp.b];
    if (unkH.length) console.log(`  note: ${unkH.length} upstream request(s) hit no fake route (see report.json unknownHttp)`);
    if (hard.length) failed = true;
  }
  const tot = report.scenarios.reduce((acc, r) => ({ cases: acc.cases + r.cases, equal: acc.equal + r.equal, diffs: acc.diffs + r.diffs.length,
    hard: acc.hard + r.diffs.filter((d) => !d.allowed).length }), { cases: 0, equal: 0, diffs: 0, hard: 0 });
  report.summary = tot;
  writeFileSync(join(PARITY, 'out', 'report.json'), JSON.stringify(report, null, 1));
  console.log(`\nTOTAL ${opts.a} vs ${opts.b}: ${tot.cases} cases, ${tot.equal} equal, ${tot.diffs} with diffs (${tot.diffs - tot.hard} allowed) -> ${failed ? 'FAIL' : 'PASS'}`);
  console.log(`report: ${join(PARITY, 'out', 'report.json')}`);
  return failed ? 1 : 0;
}

main().then((code) => { process.exitCode = code; }, (e) => { console.error(`parity harness error: ${e.stack || e.message}`); process.exitCode = 2; });
