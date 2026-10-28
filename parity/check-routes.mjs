#!/usr/bin/env node
// Route coverage gate.
//   1. Every (method, path) row of routes.python.json (the Python app's route table, tools/dump_routes.py) must have
//      at least one corpus case (a case names its row in `route: "METHOD /template"`). Case counts per route are
//      printed; routes with a single case are flagged.
//   2. A corpus case may only name a route that exists (typo guard), or a pseudo-route ("CLI ...").
//   3. When parity/routes.rust.json exists (written by the Rust test suite, same row shape: at least method, path,
//      auth), the Rust route set must equal Python's on (method, path, auth).
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
    if (!pyKeys.has(c.route)) { unknown.set(c.route, [...(unknown.get(c.route) || []), `${s}:${c.id}`]); continue; }
    const e = counts.get(c.route);
    e.total++;
    e.byScenario[s] = (e.byScenario[s] || 0) + 1;
  }
}
console.log('corpus cases per route (all scenarios):');
for (const [route, e] of [...counts].sort()) {
  const flag = e.total === 0 ? '  <- MISSING' : e.total === 1 ? '  <- only one case' : '';
  console.log(`  ${String(e.total).padStart(4)}  ${route}  (${Object.entries(e.byScenario).map(([s, n]) => `${s} ${n}`).join(', ') || '-'})${flag}`);
}
const missing = [...counts].filter(([, e]) => e.total === 0).map(([r]) => r);
if (missing.length) { failed = true; console.log(`\nFAIL: ${missing.length} Python route(s) without a corpus case:\n  ${missing.join('\n  ')}`); }
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
    const ps = new Set(py.map(sig));
    const rs = new Set(rust.map(sig));
    const onlyPy = [...ps].filter((x) => !rs.has(x)).sort();
    const onlyRust = [...rs].filter((x) => !ps.has(x)).sort();
    if (onlyPy.length || onlyRust.length) {
      failed = true;
      console.log('\nFAIL: the Rust route table differs from Python on (method, path, auth):');
      for (const x of onlyPy) console.log(`  only Python: ${x}`);
      for (const x of onlyRust) console.log(`  only Rust:   ${x}`);
    } else {
      console.log(`\nRust route table: ${rust.length} rows, equal to Python on (method, path, auth).`);
    }
  }
} else {
  console.log('\nroutes.rust.json not present: the Rust route-set check is skipped (the Rust test suite writes it).');
}

if (verbose) console.log(`\n${py.length} Python routes, ${ALL_SCENARIOS.map((s) => `${s}: ${casesFor(s).length} cases`).join(', ')}`);
console.log(failed ? '\ncheck-routes: FAIL' : `\ncheck-routes: PASS (${py.length} routes covered)`);
process.exitCode = failed ? 1 : 0;
