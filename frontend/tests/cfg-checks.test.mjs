// cfg-checks.js: the live edit checks. The schema rules copy the backend adapters' rules; the first test reads the
// constants out of the Rust source so a change on either side fails here (e2e/config-editor.mjs then compares the
// two validators end to end on mutated files).
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { checkConfig, contentChecks, schemaFor, specAt, describeSpec, choicesFor, baseUrlProblem, countBy, AIPERF, MODELOPT, EVALUATOR } from '../js/cfg-checks.js';
import { parseYaml } from '../js/cfg-parse.js';

const ADAPTERS = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../backend/crates/nemo-console/src/adapters');
const rust = (f) => fs.readFileSync(path.join(ADAPTERS, f), 'utf8');
const strings = (src, name) => { const m = new RegExp(`${name}: \\[&str; \\d+\\] =\\s*\\[([^\\]]*)\\]`).exec(src); assert.ok(m, name); return [...m[1].matchAll(/"([^"]+)"/g)].map((x) => x[1]); };
const num = (src, name) => { const m = new RegExp(`${name}: \\w+ = ([\\d_]+);`).exec(src); assert.ok(m, name); return Number(m[1].replace(/_/g, '')); };

test('the browser rules match the Rust adapters (aiperf.rs, modelopt.rs, evaluator.rs, switchyard.rs)', () => {
  const a = rust('aiperf.rs');
  const ints = [...a.slice(a.indexOf('pub const INT_KEYS')).split('];')[0].matchAll(/\("(\w+)", ([\d_]+), ([\d_]+), (true|false)\)/g)].map((m) => [m[1], Number(m[2].replace(/_/g, '')), Number(m[3].replace(/_/g, '')), m[4] === 'true']);
  assert.deepEqual(AIPERF.INT_KEYS, ints);
  assert.deepEqual(AIPERF.ENDPOINT_TYPES, strings(a, 'ENDPOINT_TYPES'));
  assert.equal(AIPERF.MAX_PROFILES, num(a, 'MAX_PROFILES'));
  const m = rust('modelopt.rs');
  assert.deepEqual(MODELOPT.METHODS, strings(m, 'METHODS'));
  assert.deepEqual(MODELOPT.RECIPE_KEYS, strings(m, 'RECIPE_KEYS'));
  assert.deepEqual(Object.keys(specAt(schemaFor('modelopt', 'recipes'), ['recipes', 'x']).keys), MODELOPT.RECIPE_KEYS);
  assert.equal(MODELOPT.MAX_RECIPES, num(m, 'MAX_RECIPES'));
  const e = rust('evaluator.rs');
  assert.deepEqual(EVALUATOR.SUPPORTED, strings(e, 'SUPPORTED_BENCHMARKS'));
  assert.deepEqual(Object.entries(EVALUATOR.REFUSED), [...e.slice(e.indexOf('pub const REFUSED_BENCHMARKS')).split('];')[0].matchAll(/\(\s*"([^"]+)",\s*"([^"]+)",?\s*\)/g)].map((x) => [x[1], x[2]]));
  const ev = schemaFor('evaluator', 'suites.yaml');
  assert.deepEqual(Object.keys(specAt(ev, ['suites', 'x']).keys), strings(e, 'SUITE_KEYS'));
  assert.deepEqual(Object.keys(specAt(ev, ['suites', 'x', 'benchmarks', 0]).keys), strings(e, 'RUN_KEYS'));
  for (const k of ['MAX_SUITES', 'MAX_BENCHES', 'MAX_PROBLEMS', 'MAX_WORK']) assert.equal(EVALUATOR[k], num(e, k), k);
  assert.match(rust('switchyard.rs'), /SERVICE_NAME_RE: LazyLock<regex::Regex> = LazyLock::new\(\|\| re\(r"\^\[a-z\]\[a-z0-9_-\]\*\$"\)\)/);
});

const diags = (system, file, text, format = 'yaml') => checkConfig({ system, file, format, text }).diagnostics;
const errors = (...a) => diags(...a).filter((d) => d.severity === 'error').map((d) => d.message);
const PROFILE = 'version: 1\nprofiles:\n  p1:\n    model: aitop/nemotron-lightning\n    concurrency: 2\n    requests: 8\n    isl: 128\n    osl: 64\n';

test('AIPerf profiles: a valid file is clean; each rule reports on the right line', () => {
  assert.deepEqual(diags('aiperf', 'profiles', PROFILE), []);
  const e = (t) => errors('aiperf', 'profiles', t);
  assert.deepEqual(e(PROFILE.replace('concurrency: 2', 'concurrency: 65')), ['profiles › p1 › concurrency: must be a whole number from 1 to 64 (found 65)']);
  assert.match(e(PROFILE.replace('concurrency: 2', 'concurrency: "2"'))[0], /quoted, so it is text/);
  assert.match(e(PROFILE.replace('    isl: 128\n', ''))[0], /'isl' is required/);
  assert.ok(e(PROFILE.replace('osl:', 'olsx:')).some((m) => /unknown key 'olsx' \(did you mean 'osl'\?\)/.test(m)));
  assert.match(e(PROFILE.replace('    model', '    endpoint: chat\n    model'))[0], /unknown key 'endpoint' \(did you mean 'endpoint_type'\?\)/);
  assert.match(e(PROFILE.replace('  p1:', '  P1:'))[0], /the preset name 'P1' must be lower case/);
  assert.match(e(PROFILE.replace('version: 1', 'version: 2'))[0], /version: must be 1/);
  assert.match(e(PROFILE.replace('    model', '    endpoint_type: chatty\n    model'))[0], /must be one of chat, completions \(did you mean 'chat'\?\)/);
  const d = diags('aiperf', 'profiles', PROFILE.replace('requests: 8', 'requests: 1'));
  assert.equal(d[0].severity, 'warn'); assert.match(d[0].message, /fewer requests than the concurrency/); assert.equal(d[0].line, 6);
  assert.equal(diags('aiperf', 'profiles', PROFILE.replace('concurrency: 2', 'concurrency: 65'))[0].line, 5);
});

test('Model Optimizer recipes and Evaluator suites', () => {
  const R = 'version: 1\nrecipes:\n  r1:\n    method: reap\n    base_model: org/name\n    output: /runs/r1\n';
  assert.deepEqual(diags('modelopt', 'recipes', R), []);
  assert.deepEqual(diags('modelopt', 'recipes', R.replace('version: 1', 'version: true')), []);      // Python: True == 1
  assert.match(errors('modelopt', 'recipes', R.replace('/runs/r1', '/tmp/r1'))[0], /must be a path under \/runs/);
  assert.match(errors('modelopt', 'recipes', R.replace('org/name', 'justname'))[0], /Hugging Face repo id/);
  assert.match(errors('modelopt', 'recipes', `${R}    sparsity: 1.0\n`)[0], /number from 0 to below 1/);
  assert.match(errors('modelopt', 'recipes', `${R}    sparsity: 1e-1\n`)[0], /read as text by PyYAML/);
  const S = 'version: 1\nsuites:\n  s1:\n    model: m\n    benchmarks:\n      - bench: gsm8k\n        max_problems: 10\n';
  assert.deepEqual(diags('evaluator', 'suites.yaml', S), []);
  assert.match(errors('evaluator', 'suites.yaml', S.replace('gsm8k', 'humaneval'))[0], /"humaneval" is not supported: needs a Docker sandbox/);
  assert.match(errors('evaluator', 'suites.yaml', `${S}      - bench: gsm8k\n        max_problems: 5\n`)[0], /'gsm8k' is listed twice/);
  assert.match(errors('evaluator', 'suites.yaml', S.replace('max_problems: 10', 'max_problems: 5000\n        repeats: 8'))[0], /sums to 40000; the limit is 20000/);
  assert.deepEqual(errors('evaluator', 'suites.yaml', S.replace('max_problems: 10', 'max_problems: 10\n        repeats: many')), []);   // evaluator.rs counts it as 1
  assert.match(errors('evaluator', 'suites.yaml', S.replace('max_problems: 10', 'max_problems: 10\n        repeats: 9'))[0], /repeats: must be a whole number from 1 to 8/);
});

test('Switchyard routes: cross-references, ids, base_url', () => {
  const T = '[llm_clients.litellm]\nbase_url = "http://litellm:4000/v1"\n[targets.fast]\nid = "m"\nllm_client = "litellm"\n[routes.r]\nid = "r"\ntype = "stage_router"\nefficient_target = "fast"\ncapable_target = "fast"\n[routes.r.classifier]\ntarget = "fast"\n';
  const d = (t) => diags('switchyard', 'routes', t, 'toml');
  assert.deepEqual(d(T), []);
  assert.match(d(T.replace('capable_target = "fast"', 'capable_target = "fats"'))[0].message, /'fats' is not a defined \[targets\.\*\] entry \(did you mean 'fast'\?\)/);
  assert.match(d(T.replace('llm_client = "litellm"', 'llm_client = "other"'))[0].message, /not a defined \[llm_clients\.\*\] entry/);
  assert.match(d(T.replace('[routes.r.classifier]\ntarget = "fast"', '[routes.r.classifier]\ntarget = "nope"'))[0].message, /classifier › target: 'nope'/);
  assert.match(d(T.replace('id = "r"', 'id = "renamed"'))[0].message, /differs from its id 'renamed'/);
  assert.match(d(T.replace(/\[routes[^]*/, ''))[0].message, /'routes' is required/);
  assert.equal(schemaFor('relay', 'routes'), schemaFor('switchyard', 'routes'));
  const root = parseYaml('targets:\n  a: {}\n  b: {}\n').root;
  assert.deepEqual(choicesFor(specAt(schemaFor('switchyard', 'routes'), ['routes', 'x', 'capable_target']), root), ['a', 'b']);
  assert.equal(baseUrlProblem('http://litellm:4000/v1'), null); assert.equal(baseUrlProblem('HTTP://127.0.0.1:1'), null); assert.equal(baseUrlProblem('http://[::1]:4000'), null);
  assert.match(baseUrlProblem('litellm:4000'), /http or https/);
  assert.match(baseUrlProblem('https://api.example.com/v1'), /neither loopback nor a bare service name/);
  assert.match(baseUrlProblem('http://10.0.0.1/v1'), /not loopback/);
  assert.match(baseUrlProblem('http://u:p@litellm/v1'), /userinfo/);
});

test('content checks: what the backend refuses (secrets, NUL) and invisible trouble', () => {
  const c = (t) => contentChecks(t).map((d) => `${d.severity}:${d.line}:${d.source}`);
  assert.deepEqual(c('api_key = "LITELLM_KEY"\ntoken: ${TOKEN}\npassword_env = "x"\n'), []);     // env-var names are fine
  assert.deepEqual(c('a = 1\napi_key = "abcd1234efgh"\n'), ['error:2:secret']);
  assert.deepEqual(c('k: sk-abcdefghijkl\n'), ['error:1:secret']);
  assert.deepEqual(c('Authorization: Bearer abcdefgh12345\n'), ['error:1:secret']);
  assert.deepEqual(c('x: 1\0\n'), ['error:1:lint']);
  assert.deepEqual(c('a: 1\r\nb: 2\n'), ['warn:1:lint']);
  assert.deepEqual(c('a:\u00a0b\n'), ['warn:1:lint']);
  assert.deepEqual(c('a: 1  \nb: 2 \n'), ['info:1:lint']);
  assert.match(contentChecks('a: 1  \nb: 2 \n')[0].message, /2 lines/);
});

test('checkConfig: syntax first, schema only on a parsed file, sorted by severity then line; counts', () => {
  const r = checkConfig({ system: 'aiperf', file: 'profiles', format: 'yaml', text: 'version: 1\nprofiles:\n\tp: 1\n' });
  assert.equal(r.parse.root, null); assert.equal(r.schema.id, 'aiperf_profiles');
  assert.deepEqual(r.diagnostics.map((d) => d.source), ['syntax']);
  const s = checkConfig({ system: 'aiperf', file: 'profiles', format: 'yaml', text: `${PROFILE.replace('requests: 8', 'requests: 1').replace('concurrency: 2', 'concurrency: 3')}    bogus: 1\n` });
  assert.deepEqual(s.diagnostics.map((d) => d.severity), ['error', 'warn']);
  assert.deepEqual(countBy(s.diagnostics), { error: 1, warn: 1, info: 0 });
  assert.equal(checkConfig({ system: 'dcgm', file: 'counters', format: 'text', text: 'x\n' }).schema, null);
  assert.equal(schemaFor('guardrails', 'x.yml'), null);
});

test('describeSpec explains a field for the Form view', () => {
  const sp = schemaFor('aiperf', 'profiles');
  assert.equal(describeSpec(specAt(sp, ['profiles', 'x', 'concurrency'])), 'whole number 1–64 · requests kept in flight');
  assert.match(describeSpec(specAt(sp, ['profiles', 'x', 'endpoint_type'])), /^one of chat, completions/);
  assert.equal(specAt(sp, ['profiles', 'x', 'nope']), null);
});

test('0.9.0 audit: keys named like Object.prototype members are unknown keys, not schema hits; trailing spaces are linear', () => {
  const r = checkConfig({ system: 'aiperf', file: 'profiles', format: 'yaml', text: 'version: 1\nprofiles:\n  p:\n    model: m\n    concurrency: 1\n    requests: 1\n    isl: 1\n    osl: 1\n    constructor: 1\n    toString: 2\n' });
  assert.equal(r.diagnostics.filter((d) => /unknown key/.test(d.message)).length, 2, JSON.stringify(r.diagnostics));
  const t0 = Date.now();
  checkConfig({ system: 'x', file: 'y', format: 'yaml', text: `${' '.repeat(200000)}x\n` + 'k: v \n'.repeat(40000) });
  assert.ok(Date.now() - t0 < 2000, `took ${Date.now() - t0} ms`);
});
