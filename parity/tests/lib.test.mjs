// Self-tests of the harness's own building blocks (no backend needed):   node --test parity/tests/lib.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { parse, stringify, pointer } from '../lib/json.mjs';
import { compareTrees, compareSets } from '../lib/compare.mjs';
import { parseSse } from '../lib/http.mjs';
import { makeNormaliser, isCachedArgv } from '../corpus/normalise.mjs';
import { respond, initialWorld } from '../fakes/cli-table.mjs';
import { FIXTURES } from '../lib/seed.mjs';
import { ALL_SCENARIOS, casesFor } from '../corpus/index.mjs';

const diff = (a, b) => compareTrees(parse(a), parse(b));

test('json: ints and floats keep their kind', () => {
  assert.equal(parse('1').k, 'int');
  assert.equal(parse('1.0').k, 'float');
  assert.equal(parse('1e3').k, 'float');
  assert.equal(parse('-0').k, 'int');
  assert.equal(parse('12345678901234567890').v, 12345678901234567890n);
  assert.equal(parse('NaN').k, 'float');
  assert.equal(stringify(parse('{"b":1.0,"a":[1,"x\\n",null,true]}')), '{"a":[1,"x\\n",null,true],"b":1.0}');
  assert.equal(parse('"\\u00e9\\ud83d\\ude00"').v, 'é😀');
  assert.throws(() => parse('{"a":1} x'));
  assert.throws(() => parse('{"a":}'));
  assert.equal(pointer(parse('{"a":[{"b~/c":5}]}'), '/a/0/b~0~1c').v, 5n);
});

test('compare: values not text; key order ignored, list order kept, null vs absent, int vs float', () => {
  assert.deepEqual(diff('{"a":1,"b":2}', '{"b":2,"a":1}'), []);
  assert.equal(diff('[1,2]', '[2,1]').length, 2);
  assert.equal(diff('{"a":null}', '{}')[0].path, '/a');
  assert.equal(diff('{"a":1}', '{"a":1.0}')[0].path, '/a');
  assert.deepEqual(diff('{"a":1.0}', '{"a":1.00}'), []);
  assert.deepEqual(diff('{"a":0.1}', '{"a":1e-1}'), []);
  assert.equal(diff('{"a":{"b":[1,{"c":"x"}]}}', '{"a":{"b":[1,{"c":"y"}]}}')[0].path, '/a/b/1/c');
  assert.equal(compareSets(['x', 'y'], ['y', 'x'], 'p', []).length, 0);
  assert.equal(compareSets(['x'], ['x', 'z'], 'p', []).length, 1);
});

test('sse parser: comments, events, data lines, a partial tail', () => {
  const ev = parseSse(': connected\n\nevent: shutdown\ndata: {}\n\ndata: a\ndata: b\n\ndata: part');
  assert.deepEqual(ev, [{ comment: 'connected' }, { event: 'shutdown', data: '{}' }, { data: 'a\nb' }, { data: 'part', partial: true }]);
});

test('normaliser: temp paths, ports, per-class ids, time keys keep their number kind', () => {
  const n = makeNormaliser({ root: '/tmp/nc-x/a', port: 4321, upstreamPorts: { litellm: 5555 } });
  assert.equal(n.str('/tmp/nc-x/a/state/x http://127.0.0.1:5555/v1 127.0.0.1:4321'), '<TMP>/state/x http://127.0.0.1:<up:litellm>/v1 127.0.0.1:<console>');
  assert.equal(n.str('key:0123456789ab rev 0123456789abcdef again key:0123456789ab'), 'key:<kid:1> rev <rev:1> again key:<kid:1>');
  assert.equal(n.str('run 20261027T101010-123-preset'), 'run <run:1>-preset');
  const t = n.json(parse('{"ts":1.5,"checked_at":3,"duration_ms":17,"x":2}'));
  assert.equal(stringify(t), '{"checked_at":0,"duration_ms":0,"ts":0.0,"x":2}');
  assert.ok(isCachedArgv('nemo-relay --version'));
  assert.ok(!isCachedArgv('docker compose -f x up -d'));
});

test('fake CLI table: known argv answer, unknown argv is null', () => {
  const ctx = (name, args, extra = {}) => ({ fixtures: FIXTURES, repo: '/r', home: '/h', name, args, argv: [name, ...args], stdin: '', world: initialWorld(), ...extra });
  assert.equal(respond(ctx('docker', ['inspect', '-f', '{{.State.Status}}', 'ai-lab-vllm-nemotron-lightning-nemotron-lightning-1'])).stdout, 'running\n');
  assert.equal(respond(ctx('docker', ['inspect', '-f', '{{.State.Status}}', 'nope'])).code, 1);
  assert.match(respond(ctx('nemo-relay', ['--version'])).stdout, /^nemo-relay /);
  assert.equal(respond(ctx('docker', ['frobnicate'])), null);
  assert.equal(respond(ctx('openshell', ['-g', 'openshell', 'nonsense'])), null);
});

test('corpus: unique ids, every case names a route', () => {
  for (const s of ALL_SCENARIOS) {
    const cases = casesFor(s);
    assert.ok(cases.length >= 10, `${s}: ${cases.length} cases`);
    for (const c of cases) assert.match(c.route, /^(GET|POST|PUT|HEAD|CLI) /);
  }
});

test('check-test-map: a row naming a helper fn (no #[test]) is not mapped', async () => {
  const { spawnSync } = await import('node:child_process');
  const { mkdtempSync, writeFileSync, readFileSync } = await import('node:fs');
  const { tmpdir } = await import('node:os');
  const { join, dirname } = await import('node:path');
  const { fileURLToPath } = await import('node:url');
  const here = dirname(fileURLToPath(import.meta.url));
  const real = readFileSync(join(here, '..', '..', 'backend', 'TEST-MAP.md'), 'utf8');
  const id = 'tests/core/test_roles.py::test_every_route_carries_exactly_one_role_dependency';
  // point that one id at `concrete`, a helper of tests/core_route_guard.rs (a fn, but not a test)
  const swapped = real.split('\n').map((l) => (l.startsWith(`| \`${id}\``)
    ? `| \`${id}\` | core | \`tests/core_route_guard.rs::concrete\` | mapped |` : l)).join('\n');
  const dir = mkdtempSync(join(tmpdir(), 'nc-testmap-'));
  writeFileSync(join(dir, 'TEST-MAP.md'), swapped);
  const r = spawnSync(process.execPath, [join(here, '..', 'check-test-map.mjs'), '--map', join(dir, 'TEST-MAP.md')], { encoding: 'utf8' });
  assert.notEqual(r.status, 0, r.stdout);
  assert.match(r.stdout, /1 unmapped/);
});

test('reference: python = python-final = the pinned last Python commit; other refs pass through', async () => {
  const { refTag, resolveRef, DEFAULT_REF, PYTHON_FINAL_COMMIT } = await import('../ref.mjs');
  assert.equal(DEFAULT_REF, 'python-final');
  assert.equal(refTag('python'), 'python-final');
  assert.equal(refTag('python@python-final'), 'python-final');
  assert.equal(resolveRef('python-final'), PYTHON_FINAL_COMMIT);   // the tag, when present, must name this commit
  assert.equal(resolveRef('v0.3.1'), 'v0.3.1');
  assert.throws(() => refTag('python@main'), /python-final, a tag vX.Y.Z or a commit id/);
});
