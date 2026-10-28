#!/usr/bin/env node
// TEST-MAP gate: every pytest node id (parity/pytest-ids.txt) needs a row in backend/TEST-MAP.md that names the
// Rust test(s) covering it, or says why it does not apply.
//
// TEST-MAP.md is a markdown table; each data row starts with the pytest node id in backticks:
//   | pytest id | owner | Rust test(s) | status |
//   | `tests/core/test_security.py::test_bad_host_is_421` | core | `api::security::test_bad_host_is_421` | done |
//   | `tests/core/test_desktop.py::test_x` | core | N/A: Python launcher only | n/a |
// A `|` inside an id is written `\|`; an id that contains a backtick is fenced with double backticks (`` a`b ``).
// A row is MAPPED when its Rust-test cell names at least one test (`path::fn` or `fn`, in backticks or bare,
// separated by commas, spaces or <br>) and every named fn exists somewhere under backend/crates (`fn <name>`).
// A row whose Rust-test cell starts with `N/A:` (followed by a reason) counts as N/A. Anything else is UNMAPPED.
//
//   node parity/check-test-map.mjs [--allow-unmapped] [--map PATH] [--list]
// Exit 1 when anything is unmapped (rows without tests, tests that do not exist, ids missing from the map) unless
// --allow-unmapped; rows for ids that are not in pytest-ids.txt and duplicate rows are reported and also fail.
import { existsSync, readdirSync, readFileSync, statSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const RUST = resolve(HERE, '..', 'backend');   // the Cargo workspace (TEST-MAP.md, crates/)
const argv = process.argv.slice(2);
const allowUnmapped = argv.includes('--allow-unmapped');
const noPending = argv.includes('--no-pending');     // integration gate: every `pending WS-` ignore must be gone
const list = argv.includes('--list');
const mapPath = argv.includes('--map') ? resolve(argv[argv.indexOf('--map') + 1]) : join(RUST, 'TEST-MAP.md');
const ids = readFileSync(join(HERE, 'pytest-ids.txt'), 'utf8').split('\n').map((l) => l.trimEnd()).filter(Boolean);
const idSet = new Set(ids);

/** Split a markdown table row into cells: `|` separates unless escaped as `\|`. */
function cells(line) {
  const out = [];
  let cur = '';
  const body = line.trim().replace(/^\|/, '').replace(/\|$/, '');
  for (let i = 0; i < body.length; i++) {
    if (body[i] === '\\' && body[i + 1] === '|') { cur += '|'; i++; continue; }
    if (body[i] === '|') { out.push(cur.trim()); cur = ''; continue; }
    cur += body[i];
  }
  out.push(cur.trim());
  return out;
}

/** The text of a cell that is one code span (`x`, `` x ``); null when the cell is not a code span. */
function codeSpan(cell) {
  const m = /^(`+)([\s\S]*?)\1$/.exec(cell);
  if (!m) return null;
  let t = m[2];
  if (t.length > 1 && t.startsWith(' ') && t.endsWith(' ') && t.trim()) t = t.slice(1, -1);   // CommonMark padding
  return t;
}

/** The names of the TEST functions of a Rust source: a `fn` whose attribute lines (above it, through doc comments
 *  and other attributes) include `#[test]` or `#[tokio::test...]` (proptest! blocks write `#[test]` too). A helper
 *  with a matching name is not a test (audit finding 10). */
function testFnsIn(text) {
  const names = new Set();
  const lines = text.split('\n');
  for (let i = 0; i < lines.length; i++) {
    const m = /^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)\s*[<(]/.exec(lines[i]);
    if (!m) continue;
    for (let k = i - 1; k >= 0; k--) {
      const l = lines[k].trim();
      if (/^#\[(?:tokio::)?test\b/.test(l)) { names.add(m[1]); break; }
      if (l === '' || l.startsWith('//') || l.startsWith('#[') || l.startsWith('#![') || /^[)\]},"\w =:.]*\)\]$/.test(l)) continue;
      break;
    }
  }
  return names;
}

/** True when a TEST `fn <name>` is defined in <crate>/<rel> for some crate under backend/crates. */
function fnInFile(rel, name) {
  const root = join(RUST, 'crates');
  if (!existsSync(root)) return false;
  for (const crate of readdirSync(root)) {
    const f = join(root, crate, rel);
    if (existsSync(f) && testFnsIn(readFileSync(f, 'utf8')).has(name)) return true;
  }
  return false;
}

/** Every TEST `fn name` defined under backend/crates (*.rs). */
function rustFns() {
  const names = new Set();
  const root = join(RUST, 'crates');
  if (!existsSync(root)) return names;
  const walk = (d) => {
    for (const e of readdirSync(d)) {
      if (e === 'target' || e.startsWith('.')) continue;
      const p = join(d, e);
      const st = statSync(p);
      if (st.isDirectory()) walk(p);
      else if (e.endsWith('.rs')) for (const n of testFnsIn(readFileSync(p, 'utf8'))) names.add(n);
    }
  };
  walk(root);
  return names;
}

if (!existsSync(mapPath)) {
  console.log(`${mapPath} does not exist yet: 0 mapped, 0 N/A, ${ids.length} unmapped (of ${ids.length} pytest ids).`);
  console.log('Create it with one row per pytest node id (see the header of this script for the row format).');
  console.log(allowUnmapped ? 'check-test-map: PASS (--allow-unmapped)' : 'check-test-map: FAIL');
  process.exit(allowUnmapped ? 0 : 1);
}

const fns = rustFns();
const rows = new Map();
const duplicates = [];
const malformed = [];
const lines = readFileSync(mapPath, 'utf8').split('\n');
lines.forEach((line, n) => {
  if (!line.trim().startsWith('|')) return;
  const c = cells(line);
  const id = codeSpan(c[0] || '');
  if (id === null) return;                                    // header / separator / prose rows
  if (c.length < 3) { malformed.push(`line ${n + 1}: fewer than 3 columns`); return; }
  if (rows.has(id)) { duplicates.push(`line ${n + 1}: ${id}`); return; }
  rows.set(id, { line: n + 1, owner: c[1] || '', tests: c[2] || '', status: c[3] || '' });
});

/** `#[ignore = "pending WS-X: ..."]` tests: "<crate-relative file>::<fn>" -> {dep, why}. */
function pendingIgnores() {
  const out = new Map();
  const root = join(RUST, 'crates');
  if (!existsSync(root)) return out;
  const walk = (dir) => {
    for (const e of readdirSync(dir)) {
      const p = join(dir, e);
      if (statSync(p).isDirectory()) { if (e !== 'target') walk(p); continue; }
      if (!e.endsWith('.rs')) continue;
      const text = readFileSync(p, 'utf8');
      const crateDir = p.slice(root.length + 1).split('/')[0];
      const rel = p.slice(join(root, crateDir).length + 1);
      for (const m of text.matchAll(/#\[ignore\s*=\s*"pending (WS-[A-Z]+):?([^"]*)"\]/g)) {
        const after = text.slice(m.index);
        const f = /\bfn\s+([A-Za-z_][A-Za-z0-9_]*)/.exec(after);
        if (f) out.set(`${rel}::${f[1]}`, { dep: m[1], why: m[2].trim() });
      }
    }
  };
  walk(root);
  return out;
}
const pendingFns = pendingIgnores();

const result = { mapped: [], na: [], unmapped: [], pending: [] };
for (const id of ids) {
  const r = rows.get(id);
  if (!r) { result.unmapped.push({ id, why: 'no row in TEST-MAP.md' }); continue; }
  const cell = r.tests.replace(/<br\s*\/?>/gi, ' ').trim();
  const status = r.status.trim();
  if (/^N\/A:\s*\S/i.test(cell) || /^N\/A:\s*\S/i.test(status)) { result.na.push({ id }); continue; }
  if (/^N\/A/i.test(cell) || /^N\/A/i.test(status)) { result.unmapped.push({ id, why: `line ${r.line}: N/A needs a reason (N/A: <reason>)` }); continue; }
  // `tests/x.rs::fn` code spans (the generator's format): the fn must exist in THAT file (relative to the crate);
  // bare identifiers: the fn must exist somewhere under backend/crates
  const spans = [...cell.matchAll(/`([^`]+)`/g)].map((m) => m[1].trim());
  const named = spans.length ? spans
    : [...cell.matchAll(/[A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)*/g)].map((m) => m[0]);
  if (!named.length) { result.unmapped.push({ id, why: `line ${r.line}: no Rust test named` }); continue; }
  const absent = named.filter((t) => {
    const m = /^(.+\.rs)::([A-Za-z_][A-Za-z0-9_]*)$/.exec(t);
    if (m) return !fnInFile(m[1], m[2]);
    return !fns.has(t.split('::').pop());
  });
  if (absent.length) { result.unmapped.push({ id, why: `line ${r.line}: not found: ${absent.join(', ')}` }); continue; }
  const pend = named.map((t) => pendingFns.get(t)).find(Boolean);
  if (pend) result.pending.push({ id, owner: r.owner, dep: pend.dep, why: pend.why });
  result.mapped.push({ id });
}
const extra = [...rows.keys()].filter((id) => !idSet.has(id));

console.log(`TEST-MAP: ${ids.length} pytest ids · ${result.mapped.length} mapped · ${result.na.length} N/A · ${result.unmapped.length} unmapped · ${result.pending.length} pending`);
if (result.pending.length) {
  const by = {};
  for (const p of result.pending) { const k = `${p.owner} waits on ${p.dep}`; by[k] = (by[k] || 0) + 1; }
  console.log(`  pending (mapped tests marked #[ignore = "pending WS-..."]): ${Object.entries(by).map(([k, n]) => `${k}: ${n}`).join(', ')}`);
}
const missingRows = result.unmapped.filter((u) => u.why === 'no row in TEST-MAP.md');
if (missingRows.length) console.log(`  ${missingRows.length} id(s) have no row in the map`);
const show = (title, xs, fmt) => {
  if (!xs.length) return;
  console.log(`\n${title} (${xs.length}):`);
  for (const x of (list ? xs : xs.slice(0, 30))) console.log(`  ${fmt(x)}`);
  if (!list && xs.length > 30) console.log(`  ... ${xs.length - 30} more (--list shows all)`);
};
show('unmapped', result.unmapped, (u) => `${u.id}  — ${u.why}`);
show('pending', result.pending, (p) => `${p.id}  — ${p.owner} waits on ${p.dep}${p.why ? `: ${p.why}` : ''}`);
show('map rows whose id is not in pytest-ids.txt', extra, (x) => `${x} (line ${rows.get(x).line})`);
show('duplicate rows', duplicates, (x) => x);
show('malformed rows', malformed, (x) => x);

const bad = (result.unmapped.length > 0 && !allowUnmapped) || (noPending && result.pending.length > 0);
const structural = extra.length > 0 || duplicates.length > 0 || malformed.length > 0;
console.log(`\ncheck-test-map: ${bad || structural ? 'FAIL' : 'PASS'}${allowUnmapped && result.unmapped.length ? ' (--allow-unmapped)' : ''}`);
process.exitCode = bad || structural ? 1 : 0;
