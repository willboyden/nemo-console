// Pure shaping for the SkillEvaluator cards (addendum 9.15), from `GET /systems/skillevaluator/runs/{id}/summary`: the validator x run heatmap of
// status and finding severity, the rubric bars (score and pass) and the Tier 3 lift dumbbell per agent and per metric. No DOM. The summary has
// numbers and identifiers only (no finding text, no criteria, no notes: they are skill- or model-influenced content). A figure it lacks is null and
// shows as an en dash, never as 0. Unit-tested in tests/skilleval-model.test.mjs.
const isNum = (n) => typeof n === 'number' && Number.isFinite(n);
const num = (v) => (isNum(v) ? v : null);
const word = (v) => (typeof v === 'string' && v ? v : null);

/** Finding severities, worst first. */
export const SEVERITIES = ['critical', 'high', 'medium', 'low', 'info'];
const LETTER = { critical: 'C', high: 'H', medium: 'M', low: 'L', info: 'I' };

/** The finding counts of one validator as {critical, high, medium, low, info} (each a whole number >= 0; absent = 0 findings of that kind). */
export function findingCounts(f) {
  const out = {};
  for (const s of SEVERITIES) out[s] = isNum(f?.[s]) && f[s] > 0 ? Math.floor(f[s]) : 0;
  return out;
}

/** The worst severity with at least one finding, or null for none. */
export const worstSeverity = (counts) => SEVERITIES.find((s) => counts[s] > 0) ?? null;

/**
 * One cell of the validator heatmap. State: 'bad' when the validator failed or has a critical / high finding, 'warn' when it passed with a medium or low finding, 'ok'
 * when it passed clean (info findings do not count), 'muted' when it did not run or did not finish (skipped / incomplete / unknown status with no verdict). The glyph and the letter of the worst severity
 * (C, H, M, L, I) go with the colour: the heatmap is not colour-only.
 * Returns {state, glyph, worst, letter, counts, total, status, passed}.
 */
export function validatorCell(v) {
  const counts = findingCounts(v?.findings);
  const worst = worstSeverity(counts);
  const status = word(v?.status);
  const passed = typeof v?.passed === 'boolean' ? v.passed : null;
  const skipped = /^(skip|skipped|not_run|not_applicable|na|unknown|incomplete)$/i.test(status ?? '') && passed == null;
  let state;
  if (skipped || (passed == null && status == null)) state = 'muted';
  else if (passed === false || /^(fail|failed|error)$/i.test(status ?? '') || worst === 'critical' || worst === 'high') state = 'bad';
  else if (worst === 'medium' || worst === 'low' || /^(warn|warning)$/i.test(status ?? '')) state = 'warn';
  else state = 'ok';
  const total = SEVERITIES.reduce((a, s) => a + counts[s], 0);
  return { state, glyph: { ok: '✓', warn: '!', bad: '✕', muted: '–' }[state], worst, letter: worst ? LETTER[worst] : '', counts, total, status, passed };
}

/** The validators of a summary as cells: Map(name -> validatorCell); [] when the run had no Tier 1 (a Tier 3 only run). */
export function validatorsOf(summary) {
  const list = summary?.tier1?.validators;
  const out = new Map();
  if (!Array.isArray(list)) return out;
  for (const v of list) {
    const name = word(v?.name);
    if (name && !out.has(name)) out.set(name, validatorCell(v));
  }
  return out;
}

const RANK = { bad: 3, warn: 2, ok: 1, muted: 0 };

/**
 * The validator x run matrix. `runs` = [{id, label, ts, summary}] (oldest first, the runs that have a summary). Returns {cols: [{id, label, ts, has}], rows: [{name, worst (a state), cells: [cell|null per run]}]}.
 * Rows are ordered worst first (the most failing runs, then the highest state anywhere, then name) and limited to `maxRows`; a run that did not run a validator has null there (an empty cell).
 */
export function validatorMatrix(runs, { maxRows = 30 } = {}) {
  const per = runs.map((r) => validatorsOf(r.summary));
  const names = [...new Set(per.flatMap((m) => [...m.keys()]))];
  const rows = names.map((name) => {
    const cells = per.map((m) => m.get(name) ?? null);
    const bad = cells.filter((c) => c?.state === 'bad').length;
    const top = cells.reduce((a, c) => Math.max(a, c ? RANK[c.state] : 0), 0);
    return { name, cells, bad, top, worst: Object.keys(RANK).find((k) => RANK[k] === top) ?? 'muted' };
  });
  rows.sort((a, b) => b.bad - a.bad || b.top - a.top || (a.name < b.name ? -1 : 1));
  return {
    cols: runs.map((r, i) => ({ id: r.id, label: r.label, ts: r.ts, has: per[i].size > 0 })),
    rows: rows.slice(0, maxRows).map(({ name, cells, worst }) => ({ name, cells, worst })),
    total: rows.length,
  };
}

/** The rubric of a summary: [{id, importance, score|null, pass: boolean|null, judgePass: boolean|null}]; [] when none. importance is text (a word or a number as written). */
export function rubricOf(summary) {
  const list = summary?.tier1?.rubric;
  if (!Array.isArray(list)) return [];
  return list
    .filter((r) => r && (typeof r.id === 'string' || isNum(r.id)))
    .map((r) => ({
      id: String(r.id),
      importance: typeof r.importance === 'string' || isNum(r.importance) ? String(r.importance) : null,
      score: num(r.score),
      pass: typeof r.pass === 'boolean' ? r.pass : null,
      judgePass: typeof r.judge_pass === 'boolean' ? r.judge_pass : null,
    }));
}

/** The quality dimensions of a summary: [{name, value}] with finite values, highest first (ties by name); [] when none. */
export function dimensionsOf(summary) {
  const d = summary?.tier1?.dimensions;
  if (!d || typeof d !== 'object' || Array.isArray(d)) return [];
  return Object.entries(d)
    .filter(([, v]) => isNum(v))
    .map(([name, value]) => ({ name, value }))
    .sort((a, b) => b.value - a.value || (a.name < b.name ? -1 : 1));
}

/** The scale of the rubric bars: the smallest 1, 2, 5 or 10 times a power of ten at or above the best score, but at least 1 (scores are shares of 1; a score above 1 widens the scale, it never runs off the bar). */
export function scoreMax(values) {
  const top = Math.max(1, ...values.filter(isNum));
  const mag = 10 ** Math.floor(Math.log10(top));
  return [1, 2, 5, 10].map((m) => m * mag).find((x) => x >= top - 1e-12) ?? 10 * mag;
}

/**
 * The Tier 3 lift per agent: [{name, model, overall: {without, with, delta}, rows: [{name, without, with, delta}]}]. `delta` is with - without (the agent's own `skill_lift` when both are
 * missing). An agent with no numbers at all is left out. [] for a run with no Tier 3.
 */
export function liftAgents(summary) {
  const list = summary?.tier3?.agents;
  if (!Array.isArray(list)) return [];
  const out = [];
  for (const a of list) {
    const name = word(a?.name);
    if (!name) continue;
    const w = num(a.with_skill),
      wo = num(a.without_skill);
    const delta = w != null && wo != null ? w - wo : num(a.skill_lift);
    const rows = (Array.isArray(a.metrics) ? a.metrics : [])
      .filter((m) => word(m?.name))
      .map((m) => {
        const mw = num(m.with_skill),
          mo = num(m.without_skill);
        return { name: m.name, without: mo, with: mw, delta: mw != null && mo != null ? mw - mo : num(m.delta) };
      })
      .filter((m) => m.without != null || m.with != null || m.delta != null);
    if (w == null && wo == null && delta == null && !rows.length) continue;
    out.push({ name, model: word(a.model), overall: { without: wo, with: w, delta }, rows });
  }
  return out;
}

/** The dumbbell rows of one summary: a header row per agent, then its overall row (label null: the view words it) and one row per metric. */
export function liftRows(agents, runKey = 'a') {
  const rows = [];
  for (const a of agents) {
    rows.push({ id: `${runKey}:${a.name}`, label: a.model ? `${a.name} · ${a.model}` : a.name, header: true });
    rows.push({ id: `${runKey}:${a.name}:overall`, label: null, overall: true, a: a.overall.without, b: a.overall.with, delta: a.overall.delta });
    for (const m of a.rows) rows.push({ id: `${runKey}:${a.name}:${m.name}`, label: m.name, a: m.without, b: m.with, delta: m.delta });
  }
  return rows;
}
