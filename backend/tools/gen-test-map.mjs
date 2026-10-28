#!/usr/bin/env node
// Generate backend/TEST-MAP.md: one row per pytest node id (parity/pytest-ids.txt), its owner, and the Rust test
// that covers it. Zero dependencies (Node 22), so it survives Python's removal.
//
//   node backend/tools/gen-test-map.mjs            write backend/TEST-MAP.md and print the counts
//   node backend/tools/gen-test-map.mjs --check    exit 1 if TEST-MAP.md is out of date
//
// Where a Rust test must live (PORTING.md section 10):
//   tests/<dir>/test_<stem>.py::<name>[params]  ->  backend/crates/nemo-console/tests/<dir>_<stem>.rs, `fn <name>(`
//   ... when a workstream owns a test of tests/core/:   tests/<ws>_core_<stem>.rs   (ws = wsa | wsb | wsc)
//   ... when it is an integration test written after the merges: tests/integration_core_<stem>.rs
// All parametrisations of one Python test map to the one Rust test of the same name (it covers every case).
// Exceptions are written next to the Rust test, in its file:
//   // pytest-map: <pytest id or id without [params]> -> <file>.rs::<fn>
//   // pytest-na: <pytest id or id without [params]> -- <reason>      (Python-only mechanics: pydantic, mypy, ...)
import { readFileSync, readdirSync, writeFileSync, existsSync, statSync } from 'node:fs';
import { dirname, join, relative } from 'node:path';
import { fileURLToPath } from 'node:url';

const RUST = dirname(dirname(fileURLToPath(import.meta.url)));
const CRATE = join(RUST, 'crates', 'nemo-console');
const TESTS = join(CRATE, 'tests');
const IDS = join(RUST, '..', 'parity', 'pytest-ids.txt');
const OUT = join(RUST, 'TEST-MAP.md');

const WS_OF_DIR = {
  core: 'core',
  switchyard: 'workstream A', relay: 'workstream A', dynamo: 'workstream A',
  dcgm: 'workstream B', aiperf: 'workstream B', guardrails: 'workstream B', openshell: 'workstream B',
  datadesigner: 'workstream C', evaluator: 'workstream C', skillevaluator: 'workstream C',
};
// tests of tests/core/ that exercise an adapter (owned by its workstream) or several (integration, after the merges)
const CORE_OVERRIDES = {
  'tests/core/test_deployment_settings.py::test_defaults_equal_the_adapters_former_constants': 'integration',
  'tests/core/test_deployment_settings.py::test_container_configures_the_registry_built_adapters': 'integration',
  'tests/core/test_deployment_settings.py::test_switchyard_checks_the_configured_compose_labels': 'workstream A',
  'tests/core/test_deployment_settings.py::test_relay_asks_systemd_about_the_configured_unit': 'workstream A',
  'tests/core/test_deployment_settings.py::test_dynamo_inspects_the_configured_containers': 'workstream A',
  'tests/core/test_deployment_settings.py::test_render_and_gpu_parse_use_the_configured_services': 'workstream A',
  'tests/core/test_deployment_settings.py::test_lab_helpers_follow_the_layout_dir_first': 'workstream B',
  'tests/core/test_dynamo_policy.py::test_settings_reach_the_registry_built_dynamo_adapter': 'workstream A',
  'tests/core/test_observe.py::test_real_adapters_with_no_paths_give_problems_not_500': 'integration',
};

/** id -> { base, dir, stem, name } */
function parse(id) {
  const cut = id.indexOf('::');
  const file = id.slice(0, cut);
  const rest = id.slice(cut + 2);
  const name = rest.split('[')[0];
  const m = /^tests\/([a-z_]+)\/test_([a-z0-9_]+)\.py$/.exec(file);
  if (!m) throw new Error(`unexpected pytest id: ${id}`);
  return { base: `${file}::${name}`, dir: m[1], stem: m[2], name };
}

export function ownerOf(id) {
  const p = parse(id);
  if (p.dir === 'core') {
    const o = CORE_OVERRIDES[p.base];
    if (o === 'integration') return 'integration';
    return o || 'core';
  }
  return WS_OF_DIR[p.dir] || '?';
}

export function expectedFile(id) {
  const p = parse(id);
  const owner = ownerOf(id);
  if (p.dir === 'core' && owner.startsWith('workstream ')) return `tests/ws${owner.slice(-1).toLowerCase()}_core_${p.stem}.rs`;
  if (p.dir === 'core' && owner === 'integration') return `tests/integration_core_${p.stem}.rs`;
  return `tests/${p.dir}_${p.stem}.rs`;
}

function rsFiles(dir) {
  const out = [];
  if (!existsSync(dir)) return out;
  for (const e of readdirSync(dir)) {
    const p = join(dir, e);
    if (statSync(p).isDirectory()) out.push(...rsFiles(p));
    else if (e.endsWith('.rs')) out.push(p);
  }
  return out;
}

export function scan() {
  const fns = new Map();       // relative file -> Set(fn names)
  const maps = new Map();      // id or base -> "file::fn"
  const nas = new Map();       // id or base -> reason
  for (const f of [...rsFiles(TESTS), ...rsFiles(join(CRATE, 'src'))]) {
    const rel = relative(CRATE, f);
    const text = readFileSync(f, 'utf8');
    fns.set(rel, new Set([...text.matchAll(/\bfn\s+([a-z0-9_]+)\s*[<(]/g)].map((m) => m[1])));
    for (const m of text.matchAll(/\/\/\s*pytest-map:\s*(\S+)\s*->\s*(\S+)/g)) maps.set(m[1], m[2]);
    for (const m of text.matchAll(/\/\/\s*pytest-na:\s*(\S+)\s*--\s*(.+)/g)) nas.set(m[1], m[2].trim());
  }
  return { fns, maps, nas };
}

export function build() {
  const ids = readFileSync(IDS, 'utf8').split('\n').map((s) => s.trim()).filter(Boolean);
  const { fns, maps, nas } = scan();
  const rows = [];
  const counts = { mapped: 0, na: 0, unmapped: 0 };
  for (const id of ids) {
    const p = parse(id);
    const owner = ownerOf(id);
    let rust = '';
    let status;
    const na = nas.get(id) ?? nas.get(p.base);
    const explicit = maps.get(id) ?? maps.get(p.base);
    if (na !== undefined) {
      status = `N/A: ${na}`;
      counts.na++;
    } else {
      const target = explicit || `${expectedFile(id)}::${p.name}`;
      const [file, fn] = target.split('::');
      if (fns.get(file)?.has(fn)) {
        rust = `\`${file}::${fn}\``;
        status = 'mapped';
        counts.mapped++;
      } else {
        rust = '';
        status = `unmapped (expected \`${file}::${fn}\`)`;
        counts.unmapped++;
      }
    }
    rows.push(`| \`${id.replaceAll('|', '\\|')}\` | ${owner} | ${rust} | ${status.replaceAll('|', '\\|')} |`);
  }
  const byOwner = {};
  ids.forEach((id, i) => {
    const o = ownerOf(id);
    byOwner[o] ||= { total: 0, mapped: 0, na: 0 };
    byOwner[o].total++;
    if (rows[i].endsWith('| mapped |')) byOwner[o].mapped++;
    else if (rows[i].includes('| N/A: ')) byOwner[o].na++;
  });
  const ownerLines = Object.entries(byOwner).sort().map(([o, c]) =>
    `| ${o} | ${c.total} | ${c.mapped} | ${c.na} | ${c.total - c.mapped - c.na} |`);
  const md = [
    '# TEST-MAP: every pytest node id -> its Rust test',
    '',
    'GENERATED by `node backend/tools/gen-test-map.mjs` from `parity/pytest-ids.txt` and the Rust test sources;',
    'do not edit by hand. Placement rules and the `pytest-map:` / `pytest-na:` annotations: PORTING.md section 10 and',
    'the header of backend/tools/gen-test-map.mjs. Checked by `node parity/check-test-map.mjs`.',
    '',
    `Totals: ${ids.length} ids, ${counts.mapped} mapped, ${counts.na} N/A, ${counts.unmapped} unmapped.`,
    '',
    '| owner | ids | mapped | N/A | unmapped |',
    '|---|---|---|---|---|',
    ...ownerLines,
    '',
    '| pytest id | owner | Rust test(s) | status |',
    '|---|---|---|---|',
    ...rows,
    '',
  ].join('\n');
  return { md, counts, total: ids.length };
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const { md, counts, total } = build();
  if (process.argv.includes('--check')) {
    const cur = existsSync(OUT) ? readFileSync(OUT, 'utf8') : '';
    if (cur !== md) {
      console.error('TEST-MAP.md is out of date: run node backend/tools/gen-test-map.mjs');
      process.exit(1);
    }
  } else {
    writeFileSync(OUT, md);
  }
  console.log(`test-map: ${total} ids, ${counts.mapped} mapped, ${counts.na} N/A, ${counts.unmapped} unmapped`);
}
