// cfg-checks.js: the live edit checks. Their verdicts are the backend's own validators run as WebAssembly (ADR-0022),
// so these tests check the wiring (what reaches the editor, with which source and line), the hints that have no server
// rule, and the Form's field descriptions, which take their numbers from the validators (schema_constants). That the
// verdicts equal the native build's is tests/cfg-wasm.test.mjs (shared vectors) and e2e/config-editor.mjs (the server).
import test from 'node:test';
import assert from 'node:assert/strict';
import { checkConfig, lintChecks, schemaFor, specAt, describeSpec, choicesFor, countBy, LIVE_UNAVAILABLE } from '../js/cfg-checks.js';
import { parseYaml } from '../js/cfg-parse.js';
import { _test, wasmState } from '../js/cfg-wasm.js';
import { wasmReady } from './wasm-ready.mjs';

await wasmReady();
const diags = (system, file, text, format = 'yaml') => checkConfig({ system, file, format, text }).diagnostics;
const errors = (...a) =>
  diags(...a)
    .filter((d) => d.severity === 'error')
    .map((d) => d.message);
const PROFILE = 'version: 1\nprofiles:\n  p1:\n    model: aitop/nemotron-lightning\n    concurrency: 2\n    requests: 8\n    isl: 128\n    osl: 64\n';

test('AIPerf profiles: a valid file is clean; the backend validator reports on the right line', () => {
  assert.deepEqual(diags('aiperf', 'profiles', PROFILE), []);
  const d = diags('aiperf', 'profiles', PROFILE.replace('concurrency: 2', 'concurrency: 65'));
  assert.deepEqual(d, [{ severity: 'error', line: 5, source: 'schema', message: "p1: 'concurrency' must be an integer from 1 to 64" }]);
  assert.ok(errors('aiperf', 'profiles', PROFILE.replace('osl:', 'oslx:')).some((m) => /unknown key 'oslx' \(did you mean 'osl'\?\)/.test(m)));
  const w = diags('aiperf', 'profiles', PROFILE.replace('requests: 8', 'requests: 1'));
  assert.equal(w[0].severity, 'warn');
  assert.match(w[0].message, /fewer requests than the concurrency/);
  assert.equal(w[0].line, 6);
});

test('the other schema files reach the editor with the server texts', () => {
  const R = 'version: 1\nrecipes:\n  r1:\n    method: reap\n    base_model: org/name\n    output: /runs/r1\n';
  assert.deepEqual(diags('modelopt', 'recipes', R), []);
  assert.deepEqual(diags('modelopt', 'recipes', R.replace('version: 1', 'version: true')), []); // Python: True == 1
  assert.match(errors('modelopt', 'recipes', R.replace('/runs/r1', '/tmp/r1'))[0], /path under \/runs/);
  const S = 'version: 1\nsuites:\n  s1:\n    model: m\n    benchmarks:\n      - bench: gsm8k\n        max_problems: 10\n';
  assert.deepEqual(diags('evaluator', 'suites.yaml', S), []);
  assert.match(errors('evaluator', 'suites.yaml', S.replace('gsm8k', 'humaneval'))[0], /'humaneval' is not supported — needs a Docker sandbox/);
  const T =
    '[llm_clients.litellm]\nbase_url = "http://litellm:4000/v1"\n[targets.fast]\nid = "m"\nllm_client = "litellm"\n[routes.r]\nid = "r"\ntype = "stage_router"\nefficient_target = "fast"\ncapable_target = "fast"\n[routes.r.classifier]\ntarget = "fast"\n';
  assert.deepEqual(diags('switchyard', 'routes', T, 'toml'), []);
  assert.match(errors('switchyard', 'routes', T.replace('capable_target = "fast"', 'capable_target = "fats"'), 'toml')[0], /capable_target = 'fats' is not a defined \[targets\.\*\] entry/);
  assert.match(errors('switchyard', 'routes', T.replace('litellm:4000', 'api.example.com'), 'toml')[0], /neither loopback nor a bare service name/);
  const c = checkConfig({ system: 'switchyard', file: 'routes', format: 'toml', text: T });
  assert.deepEqual(c.serverAdds, ['switchyard-server --dry-run']);
  assert.equal(c.schema.id, 'switchyard_routes');
  assert.equal(checkConfig({ system: 'relay', file: 'routes', format: 'toml', text: 'schema_version = 1\n' }).schema.id, 'relay_routes');
});

test('the save-path refusals: secrets and NUL, as the backend refuses them', () => {
  const c = (t) => diags('dcgm', 'counters', t, 'text').map((d) => `${d.severity}:${d.line}:${d.source}`);
  assert.deepEqual(c('api_key = "LITELLM_KEY"\ntoken: ${TOKEN}\npassword_env = "x"\n'), []); // env-var names are fine
  assert.deepEqual(c('a = 1\napi_key = "abcd1234efgh"\n'), ['error:2:secret']);
  assert.deepEqual(c('k: sk-abcdefghijkl\n'), ['error:1:secret']);
  assert.deepEqual(c('Authorization: Bearer abcdefgh12345\n'), ['error:1:secret']);
  assert.deepEqual(c('token: abc"defghijk\n'), ['error:1:secret']); // the drift the JS copy had (ADR-0022)
  assert.deepEqual(c('x: 1\0\n'), ['error:1:content']);
});

test('hints with no server rule stay in JS: line endings, look-alike spaces, trailing spaces', () => {
  const c = (t) => lintChecks(t).map((d) => `${d.severity}:${d.line}:${d.source}`);
  assert.deepEqual(c('a: 1\r\nb: 2\n'), ['warn:1:lint']);
  assert.deepEqual(c('a:\u00a0b\n'), ['warn:1:lint']);
  assert.deepEqual(c('a: 1  \nb: 2 \n'), ['info:1:lint']);
  assert.match(lintChecks('a: 1  \nb: 2 \n')[0].message, /2 lines/);
});

test('checkConfig: the real loader decides the syntax; sorted by severity then line; counts', () => {
  const r = checkConfig({ system: 'aiperf', file: 'profiles', format: 'yaml', text: 'version: 1\nprofiles:\n\tp: 1\n' });
  assert.equal(r.live, 'ok');
  assert.equal(r.syntax, 'error');
  assert.equal(r.schema.id, 'aiperf_profiles');
  assert.deepEqual(
    r.diagnostics.map((d) => d.source),
    ['syntax'],
  );
  // YAML the Form view does not model (anchors) is still checked by the real loader
  const a = checkConfig({ system: 'guardrails', file: 'x.yml', format: 'yaml', text: 'a: &x 1\nb: *x\n' });
  assert.equal(a.parse.unsupported, 'anchors, aliases or tags (&, *, !)');
  assert.equal(a.syntax, 'ok');
  const s = checkConfig({ system: 'aiperf', file: 'profiles', format: 'yaml', text: `${PROFILE.replace('requests: 8', 'requests: 1').replace('concurrency: 2', 'concurrency: 3')}    bogus: 1\n` });
  assert.deepEqual(
    s.diagnostics.map((d) => d.severity),
    ['error'],
  ); // an error drops the preset, so its warnings are not reported (aiperf.rs)
  assert.deepEqual(countBy(s.diagnostics), { error: 1, warn: 0, info: 0 });
  assert.equal(checkConfig({ system: 'dcgm', file: 'counters', format: 'text', text: 'x\n' }).schema, null);
  assert.equal(schemaFor('guardrails', 'x.yml'), null);
});

test("describeSpec and choicesFor explain a field for the Form view, with the validators' numbers", () => {
  const sp = schemaFor('aiperf', 'profiles');
  assert.equal(describeSpec(specAt(sp, ['profiles', 'x', 'concurrency'])), 'whole number 1–64 · requests kept in flight');
  assert.match(describeSpec(specAt(sp, ['profiles', 'x', 'endpoint_type'])), /^one of chat, completions/);
  assert.equal(specAt(sp, ['profiles', 'x', 'nope']), null);
  assert.equal(specAt(sp, ['profiles', 'x', 'constructor']), null); // never Object.prototype
  assert.deepEqual(Object.keys(specAt(schemaFor('modelopt', 'recipes'), ['recipes', 'x']).keys), ['method', 'base_model', 'output', 'description', 'preset', 'sparsity', 'config']);
  assert.equal(specAt(schemaFor('evaluator', 'suites.yaml'), ['suites', 'x', 'benchmarks', 0, 'max_problems']).max, 5000);
  const root = parseYaml('targets:\n  a: {}\n  b: {}\n').root;
  assert.deepEqual(choicesFor(specAt(schemaFor('switchyard', 'routes'), ['routes', 'x', 'capable_target']), root), ['a', 'b']);
});

test('0.9.0 audit: prototype-named keys are unknown keys; the checks stay fast on a large draft', () => {
  const r = checkConfig({
    system: 'aiperf',
    file: 'profiles',
    format: 'yaml',
    text: 'version: 1\nprofiles:\n  p:\n    model: m\n    concurrency: 1\n    requests: 1\n    isl: 1\n    osl: 1\n    constructor: 1\n    toString: 2\n',
  });
  assert.equal(r.diagnostics.filter((d) => /unknown key/.test(d.message)).length, 2, JSON.stringify(r.diagnostics));
  const t0 = Date.now();
  checkConfig({ system: 'x', file: 'y', format: 'yaml', text: `${' '.repeat(200000)}x\n` + 'k: v \n'.repeat(40000) });
  assert.ok(Date.now() - t0 < 2000, `took ${Date.now() - t0} ms`);
});

test('no WebAssembly, no silent pass: the editor gets "live checks unavailable", never a clean verdict', async () => {
  _test.breakInstance();
  const r = checkConfig({ system: 'aiperf', file: 'profiles', format: 'yaml', text: PROFILE.replace('concurrency: 2', 'concurrency: 65') });
  assert.equal(r.live, 'unavailable');
  assert.equal(r.syntax, null);
  assert.deepEqual(
    r.diagnostics.map((d) => [d.severity, d.source, d.message]),
    [['warn', 'live', LIVE_UNAVAILABLE]],
  );
  for (let i = 0; i < 50 && wasmState().state !== 'ready'; i++) await new Promise((res) => setTimeout(res, 20));
  assert.equal(checkConfig({ system: 'aiperf', file: 'profiles', format: 'yaml', text: PROFILE }).live, 'ok');
});
