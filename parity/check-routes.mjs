#!/usr/bin/env node
// Route coverage gate.
//   1. Every (method, path) row of routes.python.json (the Python app's route table, tools/dump_routes.py) must have
//      at least one corpus case (a case names its row in `route: "METHOD /template"`). Case counts per route are
//      printed; routes with a single case are flagged.
//   2. A corpus case may only name a route that exists (typo guard), or a pseudo-route ("CLI ...").
//   3. When parity/routes.rust.json exists (written by the Rust test suite, same row shape: at least method, path,
//      auth), the Rust route set must equal Python's on (method, path, auth). Rows marked `rust_only: "D-<n>"` are
//      routes added after the port (a deviation recorded in backend/PORTING.md): they are listed, must NOT exist in
//      the Python table, and are left out of the comparison. Rows carrying `auth_deviation: {python, deviation:
//      "D-<n>"}` are routes whose role changed after the port: Python must have the row with exactly that old role,
//      the Rust role must be stricter (admin > viewer > public), and the row is compared with the Python role. Every
//      deviation named (rust_only or auth_deviation) must have its bullet in backend/PORTING.md ("- **D-<n> ...").
// Exit 0 when all pass, 1 otherwise.   node parity/check-routes.mjs [--verbose]
import { existsSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { ALL_SCENARIOS, casesFor } from './corpus/index.mjs';

const HERE = dirname(fileURLToPath(import.meta.url));
const verbose = process.argv.includes('--verbose');
const py = JSON.parse(readFileSync(join(HERE, 'routes.python.json'), 'utf8')).routes;
const key = (r) => `${r.method} ${r.path}`;
const pyKeys = new Set(py.map(key));
let failed = false;

// 1 + 2: corpus coverage
const counts = new Map(py.map((r) => [key(r), { total: 0, byScenario: {} }]));
const unknown = new Map();
for (const s of ALL_SCENARIOS) {
  for (const c of casesFor(s)) {
    if (c.route.startsWith('CLI ')) continue;
    if (!pyKeys.has(c.route)) {
      unknown.set(c.route, [...(unknown.get(c.route) || []), `${s}:${c.id}`]);
      continue;
    }
    const e = counts.get(c.route);
    e.total++;
    e.byScenario[s] = (e.byScenario[s] || 0) + 1;
  }
}
console.log('corpus cases per route (all scenarios):');
for (const [route, e] of [...counts].sort()) {
  const flag = e.total === 0 ? '  <- MISSING' : e.total === 1 ? '  <- only one case' : '';
  console.log(
    `  ${String(e.total).padStart(4)}  ${route}  (${
      Object.entries(e.byScenario)
        .map(([s, n]) => `${s} ${n}`)
        .join(', ') || '-'
    })${flag}`,
  );
}
const missing = [...counts].filter(([, e]) => e.total === 0).map(([r]) => r);
if (missing.length) {
  failed = true;
  console.log(`\nFAIL: ${missing.length} Python route(s) without a corpus case:\n  ${missing.join('\n  ')}`);
}
if (unknown.size) {
  failed = true;
  console.log(`\nFAIL: corpus cases name routes that are not in routes.python.json:`);
  for (const [r, ids] of unknown) console.log(`  ${r}: ${ids.slice(0, 5).join(', ')}${ids.length > 5 ? ' ...' : ''}`);
}

// 3: Rust route table
const rustFile = join(HERE, 'routes.rust.json');
if (existsSync(rustFile)) {
  let rust;
  try {
    const doc = JSON.parse(readFileSync(rustFile, 'utf8'));
    rust = Array.isArray(doc) ? doc : doc.routes;
    if (!Array.isArray(rust)) throw new Error('expected an array of rows or {routes: [...]}');
  } catch (e) {
    failed = true;
    console.log(`\nFAIL: routes.rust.json is not readable: ${e.message}`);
  }
  if (rust) {
    const sig = (r) => `${r.method} ${r.path} auth=${r.auth ?? 'null'}`;
    const RANK = { public: 0, viewer: 1, admin: 2 };
    const porting = readFileSync(join(HERE, '..', 'backend', 'PORTING.md'), 'utf8');
    // a deviation is documented when PORTING.md has its bullet: "- **D-<n> ..."
    const deviationDocumented = (id) => new RegExp(`^- \\*\\*${id} `, 'm').test(porting);
    const extra = rust.filter((r) => r.rust_only);
    for (const r of extra) {
      if (/^D-\d+$/.test(String(r.rust_only)) && !deviationDocumented(r.rust_only)) {
        failed = true;
        console.log(`\nFAIL: ${key(r)}: rust_only ${r.rust_only} is not a deviation in backend/PORTING.md`);
      }
      if (!/^D-\d+$/.test(String(r.rust_only))) {
        failed = true;
        console.log(`\nFAIL: ${key(r)}: rust_only must name a PORTING.md deviation (D-<n>), not ${JSON.stringify(r.rust_only)}`);
      }
      if (pyKeys.has(key(r))) {
        failed = true;
        console.log(`\nFAIL: ${key(r)} is marked rust_only (${r.rust_only}) but the Python table has it`);
      }
    }
    if (extra.length) {
      console.log(`\nRust-only routes (added after the port, not compared): ${extra.length}`);
      for (const r of extra) console.log(`  ${r.rust_only}  ${sig(r)}`);
    }
    const changed = rust.filter((r) => !r.rust_only && r.auth_deviation);
    for (const r of changed) {
      const d = r.auth_deviation;
      if (!d || typeof d !== 'object' || !/^D-\d+$/.test(String(d.deviation)) || typeof d.python !== 'string') {
        failed = true;
        console.log(`\nFAIL: ${key(r)}: auth_deviation must be {python: <role>, deviation: "D-<n>"}, not ${JSON.stringify(d)}`);
        continue;
      }
      // a role may only get stricter after the port (admin > viewer > public): a loosening is never a parity deviation
      if (!(RANK[r.auth] > RANK[d.python])) {
        failed = true;
        console.log(`\nFAIL: ${key(r)}: auth_deviation ${d.deviation}: the Rust role (${r.auth}) must be stricter than Python's (${d.python})`);
      }
      if (!deviationDocumented(d.deviation)) {
        failed = true;
        console.log(`\nFAIL: ${key(r)}: auth_deviation ${d.deviation} is not a deviation in backend/PORTING.md`);
      }
      if (!py.some((x) => key(x) === key(r) && x.auth === d.python)) {
        failed = true;
        console.log(`\nFAIL: ${key(r)}: auth_deviation ${d.deviation} says Python has auth=${d.python}, the Python table does not`);
      }
    }
    if (changed.length) {
      console.log(`\nRoutes whose role changed after the port (compared with the Python role): ${changed.length}`);
      for (const r of changed) console.log(`  ${r.auth_deviation?.deviation}  ${key(r)} auth=${r.auth_deviation?.python} -> ${r.auth}`);
    }
    const asPython = (r) => (r.auth_deviation?.python ? { ...r, auth: r.auth_deviation.python } : r);
    const ps = new Set(py.map(sig));
    const rs = new Set(rust.filter((r) => !r.rust_only).map((r) => sig(asPython(r))));
    const onlyPy = [...ps].filter((x) => !rs.has(x)).sort();
    const onlyRust = [...rs].filter((x) => !ps.has(x)).sort();
    if (onlyPy.length || onlyRust.length) {
      failed = true;
      console.log('\nFAIL: the Rust route table differs from Python on (method, path, auth):');
      for (const x of onlyPy) console.log(`  only Python: ${x}`);
      for (const x of onlyRust) console.log(`  only Rust:   ${x}`);
    } else {
      console.log(`\nRust route table: ${rust.length - extra.length} rows (+ ${extra.length} Rust-only), equal to Python on (method, path, auth).`);
    }
  }
} else {
  console.log('\nroutes.rust.json not present: the Rust route-set check is skipped (the Rust test suite writes it).');
}

if (verbose) console.log(`\n${py.length} Python routes, ${ALL_SCENARIOS.map((s) => `${s}: ${casesFor(s).length} cases`).join(', ')}`);
console.log(failed ? '\ncheck-routes: FAIL' : `\ncheck-routes: PASS (${py.length} routes covered)`);
process.exitCode = failed ? 1 : 0;
