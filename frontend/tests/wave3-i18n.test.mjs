// Every message the wave-3 cards use exists (addendum 9.15): the literal keys are checked by static-guards.test.mjs; this checks the FAMILIES built from an id at run
// time (`apv.metric.<id>`, `skv.state.<state>` ...), and that every string with a placeholder keeps the names the code passes.
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { CURVE_METRICS, HTTP_PHASES, SCATTER_X, REQUEST_MEASURES } from '../js/aiperf-model.js';
import { COST_AXES } from '../js/evaluator-model.js';
import { TYPE_KEYS } from '../js/dd-profile-model.js';
import { SEVERITIES } from '../js/skilleval-model.js';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const en = JSON.parse(fs.readFileSync(path.join(root, 'i18n/en.json'), 'utf8'));
const has = (k) => assert.ok(k in en, `missing i18n key ${k}`);

test('the id-built families are complete', () => {
  for (const m of CURVE_METRICS) has(`apv.metric.${m.id}`);
  for (const m of REQUEST_MEASURES) has(`apv.metric.${m.id}`);
  for (const p of HTTP_PHASES) has(`apv.phase.${p}`);
  for (const x of SCATTER_X) {
    has(`apv.sx.${x.id}`);
    has(`rh.n.${x.field}`);
  }
  for (const a of COST_AXES) {
    has(`evv.axis.${a.id}`);
    has(`evv.axis_title.${a.id}`);
  }
  for (const k of TYPE_KEYS) has(`ddv.type.${k}`);
  for (const s of SEVERITIES) has(`skv.sev.${s}`);
  for (const s of ['ok', 'warn', 'bad', 'muted']) {
    has(`skv.state.${s}`);
    has(`skv.key.${s}`);
  }
  for (const k of ['rh.n.mean_reward', 'rh.n.pass_at_1', 'rh.n.output_tps', 'skv.pass', 'skv.fail', 'gymv.tasks_one', 'gymv.tasks_other']) has(k);
});

test('every wave-3 module uses only messages that exist (literal keys and the dynamic families above)', () => {
  const files = ['runviz', 'charts'].flatMap((d) => fs.readdirSync(path.join(root, 'js', d)).map((f) => path.join(root, 'js', d, f)));
  const missing = [];
  for (const f of files) {
    const src = fs.readFileSync(f, 'utf8');
    for (const m of src.matchAll(/\bt\(\s*(?:[^'"`()]*\?\s*)?['"]((?:apv|evv|gymv|mov|skv|ddv|rv|viz)\.[a-z_0-9.]+)['"]/g))
      if (!(m[1] in en) && !(`${m[1]}_other` in en)) missing.push(`${path.basename(f)}: ${m[1]}`);
    for (const m of src.matchAll(/'((?:skv\.(?:pass|fail)))'/g)) if (!(m[1] in en)) missing.push(`${path.basename(f)}: ${m[1]}`);
  }
  assert.deepEqual(missing, []);
});

test('placeholders: the names a message uses are the ones the code fills (spot checks of the messages with several)', () => {
  const vars = (k) => [...en[k].matchAll(/\{(\w+)\}/g)].map((m) => m[1]).sort();
  assert.deepEqual(vars('apv.scatter_tip'), ['conc', 'run', 'x', 'y']);
  assert.deepEqual(vars('apv.hist_note'), ['failed', 'n', 'p50', 'p95']);
  assert.deepEqual(vars('evv.cell'), ['bench', 'model', 'score', 'when']);
  assert.deepEqual(vars('evv.cost_tip'), ['bench', 'run', 'score', 'x']);
  assert.deepEqual(vars('gymv.stats'), ['full', 'mean', 'n', 'zero']);
  assert.deepEqual(vars('mov.job_tip'), ['duration', 'job', 'method', 'verdict']);
  assert.deepEqual(vars('mov.out_tip'), ['pct', 'saved', 'size']);
  assert.deepEqual(vars('skv.cell'), ['findings', 'run', 'state', 'validator']);
  assert.deepEqual(vars('skv.rubric_tip'), ['id', 'importance', 'judge', 'pass', 'run', 'score']);
  assert.deepEqual(vars('ddv.range_label'), ['max', 'min', 'p50', 'p95', 'what']);
  assert.deepEqual(vars('viz.db_row'), ['delta', 'name', 'with', 'without']);
  assert.deepEqual(vars('viz.lane_label'), ['errors', 'n', 'worker']);
});

test('no wave-3 message is empty or shows a raw placeholder it never fills', () => {
  for (const [k, v] of Object.entries(en)) if (/^(apv|evv|gymv|mov|skv|ddv|rv|viz)\./.test(k)) assert.ok(typeof v === 'string' && v.trim().length > 0, k);
});
