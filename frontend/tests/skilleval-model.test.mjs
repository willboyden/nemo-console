// SkillEvaluator cards (addendum 9.15): the validator x run heatmap of status and finding severity, rubric bars and the Tier 3 lift dumbbell, from
// `GET /systems/skillevaluator/runs/{id}/summary`. Numbers and identifiers only: the summary has no finding text, criterion text or notes.
import test from 'node:test';
import assert from 'node:assert/strict';
import { SEVERITIES, findingCounts, worstSeverity, validatorCell, validatorsOf, validatorMatrix, rubricOf, dimensionsOf, scoreMax, liftAgents, liftRows } from '../js/skilleval-model.js';
import { skillSummary } from '../dev/mock-wave3.mjs';

const f = (o = {}) => ({ critical: 0, high: 0, medium: 0, low: 0, info: 0, ...o });
const v = (name, passed, status, findings) => ({ name, passed, status, findings: f(findings) });

test('findingCounts: whole counts per severity, missing is 0 findings; worstSeverity is the highest present', () => {
  assert.deepEqual(findingCounts({ high: 2, low: 1.9, info: -3, medium: 'x' }), { critical: 0, high: 2, medium: 0, low: 1, info: 0 });
  assert.deepEqual(findingCounts(null), f());
  assert.equal(worstSeverity(f({ low: 1, medium: 2 })), 'medium');
  assert.equal(worstSeverity(f()), null);
  assert.deepEqual(SEVERITIES, ['critical', 'high', 'medium', 'low', 'info']);
});

test('validatorCell: failed or a critical / high finding is bad; passed with medium or low is warn; clean (info does not count) is ok; not run is muted; a glyph and a letter go with the state', () => {
  const c = (x) => validatorCell(x);
  assert.deepEqual([c(v('a', false, 'failed', { high: 1 })).state, c(v('a', false, 'failed', { high: 1 })).letter, c(v('a', false, 'failed', { high: 1 })).glyph], ['bad', 'H', '✕']);
  assert.equal(c(v('a', true, 'passed', { critical: 1 })).state, 'bad', 'a critical finding fails the cell even when the validator says passed');
  assert.equal(c(v('a', true, 'passed', { medium: 2 })).state, 'warn');
  assert.equal(c(v('a', true, 'passed', { low: 1 })).letter, 'L');
  assert.equal(c(v('a', true, 'passed', { info: 3 })).state, 'ok');
  assert.equal(c(v('a', true, 'passed', { info: 3 })).letter, 'I', 'the letter still says an info finding exists');
  assert.equal(c(v('a', true, 'passed')).glyph, '✓');
  assert.equal(c(v('a', true, 'warning')).state, 'warn');
  assert.equal(c(v('a', null, 'skipped')).state, 'muted');
  assert.equal(c({ name: 'a' }).state, 'muted', 'no status, no verdict: it did not run');
  assert.equal(c(v('a', null, 'incomplete')).state, 'muted', 'a scan that did not finish is not a pass');
  assert.equal(c(v('a', true, 'passed', { medium: 3 })).total, 3);
});

test('validatorsOf: by name, the first of a repeated name wins, junk is dropped, a run with no Tier 1 has none', () => {
  const m = validatorsOf({ tier1: { validators: [v('a', true, 'passed'), v('a', false, 'failed'), { name: 7 }, null, v('b', false, 'failed', { high: 1 })] } });
  assert.deepEqual([...m.keys()], ['a', 'b']);
  assert.equal(m.get('a').state, 'ok');
  assert.equal(validatorsOf({ tier1: null }).size, 0);
  assert.equal(validatorsOf(null).size, 0);
});

test('validatorMatrix: validators x runs, the worst validators first, a run that did not run one has null there; capped at maxRows', () => {
  const run = (id, validators) => ({ id, label: id, ts: Number(id.slice(1)), summary: { tier1: { validators } } });
  const m = validatorMatrix([
    run('r1', [v('naming', true, 'passed'), v('links', false, 'failed', { high: 1 }), v('size', true, 'passed')]),
    run('r2', [v('naming', true, 'passed', { medium: 1 }), v('links', false, 'failed', { critical: 1 })]),
    { id: 'r3', label: 'r3', ts: 3, summary: { tier1: null } },
  ]);
  assert.deepEqual(
    m.cols.map((c) => [c.id, c.has]),
    [
      ['r1', true],
      ['r2', true],
      ['r3', false],
    ],
  );
  assert.deepEqual(
    m.rows.map((r) => r.name),
    ['links', 'naming', 'size'],
    'the validator that failed twice first, then by worst state, then by name',
  );
  assert.equal(m.rows[0].cells[1].worst, 'critical');
  assert.equal(m.rows[2].cells[1], null, 'run 2 did not run size');
  assert.equal(m.rows[0].cells[2], null);
  assert.equal(m.rows[0].worst, 'bad');
  assert.equal(m.rows[1].worst, 'warn');
  assert.equal(m.total, 3);
  assert.equal(validatorMatrix([run('r1', [v('a', true, 'passed'), v('b', true, 'passed'), v('c', true, 'passed')])], { maxRows: 2 }).rows.length, 2);
  assert.equal(validatorMatrix([run('r1', [v('a', true, 'passed'), v('b', true, 'passed'), v('c', true, 'passed')])], { maxRows: 2 }).total, 3);
  assert.deepEqual(validatorMatrix([]), { cols: [], rows: [], total: 0 });
});

test('rubricOf and dimensionsOf: ids and numbers; text importance or numeric; a missing score is null; the dimensions sorted high to low', () => {
  const s = {
    tier1: {
      rubric: [{ id: 'a', importance: 'high', score: 0.9, pass: true, judge_pass: false }, { id: 'b', importance: 3, score: null }, { id: 5, score: 0.1, pass: 'yes' }, { text: 'no id' }, null],
      dimensions: { clarity: 0.7, safety: 0.9, bad: 'x', n: NaN },
    },
  };
  const r = rubricOf(s);
  assert.deepEqual(
    r.map((x) => x.id),
    ['a', 'b', '5'],
  );
  assert.deepEqual([r[0].score, r[0].pass, r[0].judgePass, r[0].importance], [0.9, true, false, 'high']);
  assert.deepEqual([r[1].score, r[1].pass, r[1].importance], [null, null, '3']);
  assert.equal(r[2].pass, null, 'a non-boolean pass is unknown');
  assert.deepEqual(dimensionsOf(s), [
    { name: 'safety', value: 0.9 },
    { name: 'clarity', value: 0.7 },
  ]);
  assert.deepEqual(rubricOf({ tier1: null }), []);
  assert.deepEqual(dimensionsOf({}), []);
  assert.deepEqual(dimensionsOf({ tier1: { dimensions: [1] } }), []);
});

test('scoreMax: 1, 2, 5 or 10 times a power of ten at or above the best score, at least 1', () => {
  assert.equal(scoreMax([0.4, 0.9]), 1);
  assert.equal(scoreMax([]), 1);
  assert.equal(scoreMax([null, 'x']), 1);
  assert.equal(scoreMax([1.2]), 2);
  assert.equal(scoreMax([85, 62]), 100);
  assert.equal(scoreMax([7.5]), 10);
  assert.equal(scoreMax([3.2]), 5);
});

test("liftAgents: with minus without per agent and per metric (the agent's own lift when a side is missing); an agent with nothing is left out", () => {
  const a = liftAgents({
    tier3: {
      agents: [
        {
          name: 'opencode',
          model: 'm1',
          skill_lift: 0.5,
          with_skill: 0.8,
          without_skill: 0.6,
          metrics: [
            { name: 'correctness', with_skill: 0.9, without_skill: 0.7 },
            { name: 'format', with_skill: 0.5, without_skill: 0.6 },
            { name: 'tool_use', delta: 0.2 },
            { name: 'empty' },
            { nope: 1 },
          ],
        },
        { name: 'claude-local', model: null, skill_lift: 0.12, with_skill: null, without_skill: null, metrics: [] },
        { name: 'ghost', skill_lift: null, with_skill: null, without_skill: null },
        { model: 'x' },
      ],
    },
  });
  assert.deepEqual(
    a.map((x) => x.name),
    ['opencode', 'claude-local'],
  );
  assert.ok(Math.abs(a[0].overall.delta - 0.2) < 1e-9, 'with minus without, not the stored lift');
  assert.deepEqual(
    a[0].rows.map((r) => r.name),
    ['correctness', 'format', 'tool_use'],
  );
  assert.ok(Math.abs(a[0].rows[0].delta - 0.2) < 1e-9);
  assert.ok(Math.abs(a[0].rows[1].delta + 0.1) < 1e-9, 'a worse score with the skill is a negative delta');
  assert.equal(a[0].rows[2].delta, 0.2);
  assert.equal(a[1].overall.delta, 0.12);
  assert.equal(a[1].overall.with, null);
  assert.deepEqual(liftAgents({ tier3: null }), []);
  assert.deepEqual(liftAgents(null), []);
});

test('liftRows: a header row per agent, then its overall row (no label: the view words it) and a row per metric', () => {
  const rows = liftRows(liftAgents({ tier3: { agents: [{ name: 'a', model: 'm', with_skill: 0.8, without_skill: 0.6, metrics: [{ name: 'x', with_skill: 0.5, without_skill: 0.4 }] }] } }), 'b');
  assert.deepEqual(
    rows.map((r) => [r.header ?? false, r.overall ?? false, r.label]),
    [
      [true, false, 'a · m'],
      [false, true, null],
      [false, false, 'x'],
    ],
  );
  assert.ok(rows.every((r) => r.id.startsWith('b:')));
  assert.equal(rows[1].a, 0.6);
  assert.equal(rows[1].b, 0.8);
});

test('the mock summary, shaped: numbers and identifiers only, the failures of the event are in the validators, a Tier 3 run has agents with metrics', () => {
  const t1 = {
    data: {
      run_id: 'r1',
      skill: 's',
      tiers: 'tier1',
      verdict: 'failed',
      tier1: { status: 'failed', quality_score: 46, grade: 'D', rubric_score: 70, errors: 4, warnings: 3, high: 1, critical: 1, incomplete: [] },
    },
  };
  const s = skillSummary(t1);
  const m = validatorsOf(s);
  const cells = [...m.values()];
  assert.equal(cells.filter((c) => c.state === 'bad').length, 4, 'as many failing validators as the run had errors');
  assert.equal(
    cells.reduce((a, c) => a + c.counts.critical, 0),
    1,
  );
  assert.equal(
    cells.reduce((a, c) => a + c.counts.high, 0),
    1,
  );
  assert.ok(rubricOf(s).length > 3);
  assert.ok(dimensionsOf(s).length > 2);
  const text = JSON.stringify(s);
  assert.ok(!/message|criterion|notes|conclusion|recommend/i.test(text), 'no skill- or model-written text');
  const t3 = skillSummary({
    data: {
      run_id: 'r3',
      skill: 's',
      tiers: 'tier3',
      verdict: 'succeeded',
      tier1: null,
      tier3: { agents: { a: { model: 'm', skill_lift: 0.2, with_skill: 0.8, without_skill: 0.6 }, b: { model: 'n', skill_lift: null, with_skill: null, without_skill: null } } },
    },
  });
  assert.equal(t3.tier1, null);
  const ag = liftAgents(t3);
  assert.deepEqual(
    ag.map((x) => x.name),
    ['a'],
    'an agent that failed has no numbers and no row',
  );
  assert.equal(ag[0].rows.length, 4);
});
