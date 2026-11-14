// The config validators compiled to WebAssembly (ADR-0022), run in Node: the shared vectors must give exactly the
// answers the native build recorded (backend/crates/nemo-console-core/tests/vectors/check.json, checked natively by
// tests/vectors.rs), and the loader must report a broken module instead of passing silently.
import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { wasmReady } from './wasm-ready.mjs';
import { wasmCheck, wasmPlainType, wasmRoundtrip, wasmConstants, wasmState, onWasmState, API, _test } from '../js/cfg-wasm.js';

await wasmReady();
const VECTORS = JSON.parse(readFileSync(new URL('../../backend/crates/nemo-console-core/tests/vectors/check.json', import.meta.url), 'utf8'));
const textOf = (c) => (typeof c.text === 'string' ? c.text : c.build.map(([s, n]) => s.repeat(n)).join(''));

test('every shared vector gives the native answer in the WebAssembly build', () => {
  assert.ok(VECTORS.cases.length > 100);
  const wrong = [];
  for (const c of VECTORS.cases) {
    let got;
    if (c.fn === 'check') got = wasmCheck(c.args[0], c.args[1], c.args[2], textOf(c));
    else if (c.fn === 'yaml_plain_type') got = wasmPlainType(c.args[0]);
    else if (c.fn === 'schema_constants') got = wasmConstants();
    else if (c.fn === 'roundtrip') got = wasmRoundtrip(c.args[0], c.args[1], c.args[2], JSON.parse(c.args[3]), JSON.parse(c.args[4]));
    try {
      assert.deepEqual(got, c.expect);
    } catch {
      wrong.push(`${c.name}: expected ${JSON.stringify(c.expect).slice(0, 300)} got ${JSON.stringify(got).slice(0, 300)}`);
    }
  }
  assert.deepEqual(wrong, []);
});

test('the drift the JS copy had is gone: `token: abc"defghijk` is refused as the backend refuses it', () => {
  const r = wasmCheck('aiperf', 'profiles', 'yaml', 'version: 1\ntoken: abc"defghijk\n');
  assert.equal(r.api, API);
  const s = r.diagnostics.find((d) => d.source === 'secret');
  assert.equal(s.line, 2);
  assert.equal(s.message, "refusing content that contains a literal value for 'token' (reference an env-var NAME instead)");
});

test('a real routes.toml (more than 32 brackets) parses: no thread is needed on wasm32', () => {
  const routes = `${'[routes.r]\nid = "r"\n'.repeat(1)}${'[targets.t%]\nid = "m"\nllm_client = "c"\n'.repeat(40)}[llm_clients.c]\nbase_url = "http://litellm:4000"\n`;
  const text = routes.replace(/%/g, () => String(Math.random()).slice(2, 8));
  const r = wasmCheck('switchyard', 'routes', 'toml', text);
  assert.equal(r.syntax, 'ok', JSON.stringify(r.diagnostics));
  assert.ok(!r.diagnostics.some((d) => /thread/.test(d.message)));
});

test('a trap is reported as unavailable (no silent pass) and a fresh instance takes over', async () => {
  const seen = [];
  const off = onWasmState((s) => seen.push(s.state));
  _test.breakInstance(); // the next call traps like a Rust panic does (panic = abort -> RuntimeError: unreachable)
  assert.equal(wasmCheck('aiperf', 'profiles', 'yaml', 'version: 1\n'), null);
  assert.equal(seen[0], 'failed');
  for (let i = 0; i < 50 && wasmState().state !== 'ready'; i++) await new Promise((r) => setTimeout(r, 20));
  off();
  assert.equal(wasmState().state, 'ready');
  assert.equal(wasmCheck('aiperf', 'profiles', 'yaml', 'version: 1\n').api, API);
});
