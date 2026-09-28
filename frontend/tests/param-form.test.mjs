import test from 'node:test';
import assert from 'node:assert/strict';
import { filterSpecs, groupSpecs, coerce, validateValue, validateAll, pruneValues, splitRedacted, changedKeys, needsRestart, isSecretSpec, isRedacted, actionParamValues } from '../js/param-form.js';
import { DYNAMO_CATALOG } from '../dev/mock-data.mjs';

const S = DYNAMO_CATALOG;
test('groupSpecs orders known groups first and keeps unknown groups', () => {
  const g = groupSpecs([{ key: 'a', group: 'zzz' }, { key: 'b', group: 'memory' }, { key: 'c' }]).map((x) => x.group);
  assert.deepEqual(g, ['general', 'memory', 'zzz']);
  assert.deepEqual(groupSpecs(S).map((x) => x.group)[0], 'model');
});
test('filterSpecs hides advanced unless asked; search overrides and matches flag/help', () => {
  assert.ok(!filterSpecs(S).some((s) => s.advanced)); assert.ok(filterSpecs(S, { showAdvanced: true }).some((s) => s.advanced));
  assert.deepEqual(filterSpecs(S, { query: 'enforce-eager' }).map((s) => s.key), ['enforce_eager']);
});
test('coerce: types, empty = unset, errors are i18n keys', () => {
  const i = { type: 'int' }, f = { type: 'float' }, b = { type: 'bool' }, l = { type: 'string_list' };
  assert.deepEqual(coerce(i, ''), { ok: true, value: undefined }); assert.deepEqual(coerce(i, '42'), { ok: true, value: 42 }); assert.equal(coerce(i, '4.2').error, 'params.err_int');
  assert.deepEqual(coerce(f, '0.85'), { ok: true, value: 0.85 }); assert.equal(coerce(f, 'abc').error, 'params.err_float');
  assert.equal(coerce(b, 'true').value, true); assert.equal(coerce(b, 'false').value, false);
  assert.deepEqual(coerce(l, 'a, b\n c ,'), { ok: true, value: ['a', 'b', 'c'] });
});
test('validateValue enforces min/max/choices with message keys', () => {
  const p = S.find((s) => s.key === 'gpu_memory_utilization');
  assert.equal(validateValue(p, 0.5), null); assert.deepEqual(validateValue(p, 0.99), { key: 'params.err_max', vars: { n: 0.98 } }); assert.equal(validateValue(p, 0.1).key, 'params.err_min');
  assert.equal(validateValue(S.find((s) => s.key === 'router_mode'), 'nope').key, 'params.err_choice');
  assert.deepEqual(Object.keys(validateAll(S, { gpu_memory_utilization: 2, max_num_seqs: 8 })), ['gpu_memory_utilization']);
});
test('secrets: "[set]" is never sent back; secret specs are detected', () => {
  assert.ok(isSecretSpec(S.find((s) => s.key === 'hf_token'))); assert.ok(!isSecretSpec(S.find((s) => s.key === 'max_model_len'))); assert.ok(isRedacted('[set]'));
  assert.deepEqual(pruneValues(S, { hf_token: '[set]', max_num_seqs: 8, bogus: 1, gpu_memory_utilization: undefined }), { max_num_seqs: 8 });
  assert.deepEqual(splitRedacted({ hf_token: '[set]', a: 1 }), { values: { a: 1 }, redacted: ['hf_token'] });
});
test('changedKeys / needsRestart', () => {
  assert.deepEqual(changedKeys(S, { a: 1, l: ['x', 'y'] }, { a: 1, l: ['x', 'y'], b: 2 }).filter((k) => ['a', 'b', 'l'].includes(k)), []);
  const ks = changedKeys(S, { max_num_seqs: 64 }, { max_num_seqs: 32 }); assert.deepEqual(ks, ['max_num_seqs']);
  assert.equal(needsRestart(S, ['max_num_seqs']), true); assert.equal(needsRestart(S, ['router_mode']), false);
});
test('actionParamValues: defaults are applied, empty overrides omitted', () => {
  const specs = [{ key: 'stop_sharing_engine', type: 'bool', default: false }, { key: 'n', type: 'int' }];
  assert.deepEqual(actionParamValues(specs, { stop_sharing_engine: 'true', n: '' }), { stop_sharing_engine: true });
  assert.deepEqual(actionParamValues(specs, {}), { stop_sharing_engine: false });
});
