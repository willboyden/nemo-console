// cfg-help.js + i18n/fields.en.json: every field of every config file the mock serves has a description, patterns
// pick the most specific entry, and a Dynamo command-line argument finds its flag in the Params catalog.
import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { parseConfig } from '../js/cfg-parse.js';
import { flattenFields } from '../js/cfg-edit.js';
import { globMatch, catalogsFor, matchScore, lookupHelp, flagFor, envFor } from '../js/cfg-help.js';
import { SWITCHYARD_ROUTES, RELAY_ROUTES, RELAY_PLUGINS, RELAY_CONFIG, DYNAMO_CATALOG } from '../dev/mock-data.mjs';
import { SEVEN_FILES, SEVEN_SPECS } from '../dev/mock-seven.mjs';

const doc = JSON.parse(readFileSync(new URL('../i18n/fields.en.json', import.meta.url), 'utf8'));

test('the catalog document is well formed', () => {
  const used = new Set(doc.files.flatMap((r) => r.catalogs));
  for (const id of used) assert.ok(doc.catalogs[id], `files row names a missing catalog '${id}'`);
  for (const id of Object.keys(doc.catalogs)) assert.ok(used.has(id), `catalog '${id}' is never used`);
  for (const [id, cat] of Object.entries(doc.catalogs)) {
    for (const [pat, e] of Object.entries(cat)) {
      const segs = pat.split('.');
      assert.ok(
        segs.every((s) => s !== ''),
        `${id}: empty segment in '${pat}'`,
      );
      assert.ok(!segs.slice(0, -1).includes('**'), `${id}: '**' must be last in '${pat}'`);
      assert.equal(typeof e.what, 'string', `${id}.${pat}: what`);
      assert.ok(e.what.length >= 10, `${id}.${pat}: what is too short`);
      if ('change' in e) assert.ok(typeof e.change === 'string' && e.change.length >= 10, `${id}.${pat}: change`);
      assert.deepEqual(
        Object.keys(e).filter((k) => k !== 'what' && k !== 'change'),
        [],
        `${id}.${pat}: unknown keys`,
      );
      assert.equal((`${e.what}${e.change || ''}`.match(/`/g) || []).length % 2, 0, `${id}.${pat}: unbalanced backticks`);
    }
  }
});

test('every field of every mock config file has a description', () => {
  const files = [
    ['switchyard', 'routes', 'toml', SWITCHYARD_ROUTES],
    ['relay', 'routes', 'toml', RELAY_ROUTES],
    ['relay', 'plugins', 'toml', RELAY_PLUGINS],
    ['relay', 'config', 'toml', RELAY_CONFIG],
  ];
  for (const [k, text] of Object.entries(SEVEN_FILES)) {
    const [system, file] = k.split('/');
    const spec = SEVEN_SPECS[system]?.find((s) => s.id === file);
    if (spec && spec.format !== 'text') files.push([system, file, spec.format, text]);
  }
  assert.ok(files.length >= 12, `only ${files.length} files`);
  let n = 0;
  for (const [system, file, fmt, text] of files) {
    const r = parseConfig(fmt, text);
    assert.ok(r.root, `${system}/${file} does not parse`);
    const cat = catalogsFor(doc, system, file);
    assert.ok(cat, `${system}/${file}: no catalog`);
    for (const f of flattenFields(r.root)) {
      n++;
      assert.ok(lookupHelp(cat, f.path), `${system}/${file}: no description for ${f.path.join('.')}`);
    }
  }
  assert.ok(n > 150, `only ${n} fields checked`);
});

test('patterns: the most specific entry wins; * is a key, # an index, ** the rest', () => {
  assert.ok(globMatch('*--config.y*ml', 'lab-input-output--config.yml'));
  assert.ok(globMatch('*--config.y*ml', 'x--config.yaml'));
  assert.ok(!globMatch('*--config.y*ml', 'x--prompts.yml'));
  assert.ok(!globMatch('routes', 'routes.bak'));
  assert.ok(globMatch('a.b', 'a.b'));
  assert.ok(!globMatch('a.b', 'axb'));
  assert.equal(matchScore(['a', '*'], ['a', 0]), -1);
  assert.equal(matchScore(['a', '#'], ['a', 'k']), -1);
  assert.ok(matchScore(['a', 'b'], ['a', 'b']) > matchScore(['a', '*'], ['a', 'b']));
  assert.ok(matchScore(['a', '**'], ['a', 'b', 'c']) >= 0);
  assert.equal(matchScore(['a', '**'], ['a']), -1);
  const cat = catalogsFor(doc, 'openshell', 'compose');
  assert.match(lookupHelp(cat, ['services', 'gateway', 'environment', 'OPENSHELL_ENABLE_MTLS_AUTH']).what, /mutual TLS/); // specific beats generic
  assert.match(lookupHelp(cat, ['services', 'x', 'environment', 'OTHER']).what, /environment variable/);
  assert.match(lookupHelp(catalogsFor(doc, 'relay', 'routes'), ['targets', 't', 'extra_body', 'chat_template_kwargs', 'enable_thinking']).what, /request-body field/);
  assert.equal(lookupHelp(catalogsFor(doc, 'switchyard', 'routes'), ['nonsense']), null);
  assert.equal(catalogsFor(doc, 'datadesigner', 'x.py'), null);
  assert.ok(catalogsFor(doc, 'dynamo', 'override').flags);
});

test('a Dynamo command-line argument finds its flag in the Params catalog', () => {
  const cmd = ['python3', '-m', 'dynamo.vllm', '--max-num-seqs', '64', '--enable-prefix-caching', '--kv-cache-dtype=fp8', 'positional'];
  assert.equal(flagFor(DYNAMO_CATALOG, cmd, 3).param.key, 'max_num_seqs');
  assert.equal(flagFor(DYNAMO_CATALOG, cmd, 3).isValue, false);
  assert.deepEqual([flagFor(DYNAMO_CATALOG, cmd, 4).flag, flagFor(DYNAMO_CATALOG, cmd, 4).isValue], ['--max-num-seqs', true]);
  assert.equal(flagFor(DYNAMO_CATALOG, cmd, 6).param.key, 'kv_cache_dtype');
  assert.equal(flagFor(DYNAMO_CATALOG, cmd, 6).isValue, true);
  assert.equal(flagFor(DYNAMO_CATALOG, cmd, 7), null); // follows --kv-cache-dtype=..., and a switch takes no value
  assert.equal(flagFor(DYNAMO_CATALOG, ['--enable-prefix-caching', 'x'], 1), null);
  assert.equal(flagFor(DYNAMO_CATALOG, cmd, 0), null);
  assert.equal(flagFor(null, cmd, 3), null);
  assert.equal(envFor(DYNAMO_CATALOG, 'HF_TOKEN').param.key, 'hf_token');
  assert.equal(envFor(DYNAMO_CATALOG, 'PATH'), null);
});

test('every key the four known schemas accept has a description', async () => {
  const { schemaFor } = await import('../js/cfg-checks.js');
  const files = [
    ['aiperf', 'profiles'],
    ['modelopt', 'recipes'],
    ['evaluator', 'suites.yaml'],
    ['switchyard', 'routes'],
  ];
  let n = 0;
  for (const [system, file] of files) {
    const cat = catalogsFor(doc, system, file);
    const walk = (spec, path) => {
      if (!spec) return;
      if (spec.t === 'map')
        for (const [k, v] of Object.entries(spec.keys || {})) {
          n++;
          assert.ok(lookupHelp(cat, [...path, k]), `${system}/${file}: no description for ${[...path, k].join('.')}`);
          walk(v, [...path, k]);
        }
      else if (spec.t === 'dict') walk(spec.values, [...path, 'x']);
      else if (spec.t === 'list') walk(spec.items, [...path, 0]);
    };
    walk(schemaFor(system, file), []);
  }
  assert.ok(n > 30, `only ${n} keys walked`);
});
