// Every message the wave-4 cards use exists (addendum 9.16): the literal keys are checked by static-guards.test.mjs; this checks the FAMILIES built from an id at run time, the plural
// pairs, and that the placeholders a message uses are the ones the code fills.
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { PHASES } from '../js/openshell-model.js';
import { TARGET_STATES } from '../js/flare-model.js';
import { STRIP_STATES } from '../js/strip-model.js';
import { STRIP_KEYS } from '../js/charts/strip-chart.js';
import { SPARKS, SEVERITIES } from '../js/overview-model.js';
import { SUMMARY_WINDOWS, LOG_RANGES } from '../js/audit-model.js';
import { SYSTEM_IDS } from '../js/systems.js';
import { setMessages, t } from '../js/i18n.js';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const en = JSON.parse(fs.readFileSync(path.join(root, 'i18n/en.json'), 'utf8'));
setMessages(en);
const has = (k) => assert.ok(k in en, `missing i18n key ${k}`);
const vars = (k) => [...en[k].matchAll(/\{(\w+)\}/g)].map((m) => m[1]).sort();

test('the id-built families are complete', () => {
  for (const p of PHASES) has(`osv.phase.${p.toLowerCase()}`);
  for (const s of TARGET_STATES) has(`flv.state_${s}`);
  for (const s of STRIP_STATES) has(`strip.state_${s}`);
  for (const k of STRIP_KEYS) has(`strip.state_${k}`);
  for (const k of ['input', 'output', 'model']) has(`grv.stage_${k}`);
  for (const id of Object.keys(SPARKS)) has(`ovw.spark.${id}`);
  for (const id of SYSTEM_IDS) assert.ok(id in SPARKS, `${id} has no sparkline metric`);
  for (const sev of SEVERITIES) {
    has(`ovw.sev_${sev}`);
    has(`ovw.count_${sev}_one`);
    has(`ovw.count_${sev}_other`);
  }
  for (const w of SUMMARY_WINDOWS) has(`audv.win_${w}`);
  for (const r of LOG_RANGES) has(`audv.range_${r}`);
  for (const code of ['configs.capped', 'config.unreadable', 'config.too_large', 'config.not_utf8', 'config.yaml_error', 'config.not_a_mapping', 'flows.dropped', 'flows.capped', 'main_model.dropped'])
    has(`grv.problem_${code.replace(/\./g, '_')}`);
  for (const k of ['tab.summary', 'audv.system_console', 'audv.actor_none']) has(k);
});

test('plural messages come in pairs and the right one is picked', () => {
  for (const k of Object.keys(en)) {
    if (/_one$/.test(k) && /^(grv|osv|flv|ovw|audv|strip)\./.test(k)) assert.ok(k.replace(/_one$/, '_other') in en, `${k} has no _other`);
  }
  assert.equal(t('grv.rail_count', { count: 1 }), '1 rail');
  assert.equal(t('grv.rail_count', { count: 5 }), '5 rails');
  assert.equal(t('ovw.count_error', { count: 1 }), '1 error');
  assert.equal(t('ovw.count_warn', { count: 0 }), '0 warnings');
  assert.equal(t('flv.group_count', { count: 1 }), '1 target');
  assert.equal(t('strip.downs_single', { longest: '8m' }), 'One down period (longest 8m).');
  assert.equal(t('strip.dips', { count: 1 }), 'No stretch was down for all of it; 1 stretch had a dip.');
});

test('every wave-4 message is non-empty and a message with a placeholder keeps the names the code passes', () => {
  for (const [k, v] of Object.entries(en)) if (/^(grv|osv|flv|ovw|audv|strip)\./.test(k)) assert.ok(typeof v === 'string' && v.trim().length, k);
  assert.deepEqual(vars('grv.cell'), ['got', 'probe', 'want', 'when']);
  assert.deepEqual(vars('grv.matrix_alt'), ['errors', 'probes', 'runs', 'slips']);
  assert.deepEqual(vars('grv.hidden'), ['count', 'shown']);
  assert.deepEqual(vars('grv.streaming_on'), ['chunk', 'context']);
  assert.deepEqual(vars('strip.summary'), ['downs', 'noun', 'pct', 'window']);
  assert.deepEqual(vars('osv.dec_totals'), ['allowed', 'denied', 'from', 'to', 'total']);
  assert.deepEqual(vars('osv.dec_alt'), ['allowed', 'denied', 'lanes', 'name', 'total']);
  assert.deepEqual(vars('osv.dec_bin'), ['allowed', 'denied', 'from', 'to']);
  assert.deepEqual(vars('osv.lane_ready_after'), ['after']);
  assert.deepEqual(vars('flv.grid_summary'), ['count', 'states']);
  assert.deepEqual(vars('flv.load_peak'), ['active', 'pending']);
  assert.deepEqual(vars('ovw.spark_alt'), ['latest', 'max', 'min', 'name', 'window']);
  assert.deepEqual(vars('ovw.problems_across'), ['total', 'with']);
  assert.deepEqual(vars('audv.cell'), ['actor', 'count', 'system']);
  assert.deepEqual(vars('audv.matrix_alt'), ['actors', 'systems', 'total']);
  assert.deepEqual(vars('grv.problem_flows_dropped'), ['config', 'count']);
});

test('the wave-4 modules use only messages that exist (literal keys; the dynamic families are above)', () => {
  const dirs = ['guardrails', 'openshell', 'flare', 'overview', 'audit'];
  const files = [
    ...dirs.flatMap((d) => fs.readdirSync(path.join(root, 'js', d)).map((f) => path.join(root, 'js', d, f))),
    path.join(root, 'js/runviz/guardrails-detail.js'),
    ...['strip-chart', 'strip-parts', 'lifecycle-chart', 'decision-chart'].map((f) => path.join(root, `js/charts/${f}.js`)),
  ];
  const missing = [];
  for (const f of files) {
    const src = fs.readFileSync(f, 'utf8');
    for (const m of src.matchAll(/\bt\(\s*['"]((?:grv|osv|flv|ovw|audv|strip)\.[a-z_0-9.]+)['"]/g)) if (!(m[1] in en) && !(`${m[1]}_one` in en)) missing.push(`${path.basename(f)}: ${m[1]}`);
  }
  assert.deepEqual(missing, []);
});
