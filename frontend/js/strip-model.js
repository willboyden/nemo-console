// Pure shaping of an up / down strip (addendum 9.16): the history of a 0 / 1 metric (`gr.up`, `os.gateway_up`, `relay.gateway_up`, `dyn.up`) as a row of equal cells over a
// window. A cell is UP when every sample in it said up, DOWN when every sample said down, PARTIAL when it held both, and NO DATA when nothing was sampled (the console was not
// running, or the system had no such sample): a gap is never drawn as down and never as up. No DOM, no i18n. Unit-tested in tests/strip-model.test.mjs.
import { normalizeAnswer } from './series-model.js';
import { baseKey, finite, gridSize, alignToGrid, mergeFactor } from './series-math.js';

export const STRIP_STATES = ['up', 'down', 'part', 'none'];
/** A cell whose share of up samples is at or above this is up, at or below (1 - this) is down; anything between is partial. */
const EDGE = 0.999;

/**
 * @param points   [[ts, avg, max]] of ONE series (avg = the share of polls in the bucket that were up, 0..1)
 * @param opts     {from, to, stepS (the answer's bucket size), maxCells (the most cells to draw)}
 * Returns {cells: [{ts, binS, state, share|null, n}], binS, from, to, uptime|null, known, downCells, partCells, downRuns: [{cells, from, to}], longestDown: {cells, from, to} | null, empty}.
 * `uptime` is the mean share over every sampled bucket (null when none was sampled); `share` of a cell is the mean over its sampled buckets.
 */
export function stripFromPoints(points, { from, to, stepS, maxCells = 60 } = {}) {
  const n = gridSize(from, to, stepS);
  const out = { cells: [], binS: stepS, from: from ?? null, to: to ?? null, uptime: null, known: 0, downCells: 0, partCells: 0, longestDown: null, downRuns: [], empty: true };
  if (!n || !finite(stepS)) return out;
  const raw = alignToGrid(points, { from, stepS, n });
  const factor = mergeFactor(n, maxCells);
  let sum = 0,
    known = 0;
  for (let i = 0; i < n; i += factor) {
    const part = raw.slice(i, i + factor).filter(finite);
    const share = part.length ? part.reduce((a, b) => a + Math.min(1, Math.max(0, b)), 0) / part.length : null;
    const state = share == null ? 'none' : share >= EDGE ? 'up' : share <= 1 - EDGE ? 'down' : 'part';
    out.cells.push({ ts: from + i * stepS, binS: stepS * factor, state, share, n: part.length });
    for (const v of part) {
      sum += Math.min(1, Math.max(0, v));
      known++;
    }
  }
  out.binS = stepS * factor;
  out.known = known;
  out.uptime = known ? sum / known : null;
  out.empty = known === 0;
  // runs of consecutive down cells: every run of neighbouring down cells (a partial or no-data cell ends it) and the longest
  let run = null;
  for (const c of out.cells) {
    if (c.state === 'part') out.partCells++;
    if (c.state === 'down') {
      out.downCells++;
      if (run) {
        run.cells++;
        run.to = c.ts + c.binS;
      } else {
        run = { cells: 1, from: c.ts, to: c.ts + c.binS };
        out.downRuns.push(run);
      }
      if (!out.longestDown || run.cells > out.longestDown.cells) out.longestDown = run;
    } else run = null;
  }
  return out;
}

/** The strip of one metric key out of a series answer (the first series of that key; a labelled key's variants are not merged). null when the answer has no such series. */
export function stripFromAnswer(answer, key, opts = {}) {
  const want = baseKey(key);
  const s = normalizeAnswer(answer).find((x) => baseKey(x.key) === want);
  if (!s) return null;
  return stripFromPoints(s.points, { from: answer.from, to: answer.to, stepS: answer.step_s, ...opts });
}

/** Down time in seconds, from the number of fully-down cells (a lower bound: partial cells are not counted). */
export const downSeconds = (strip) => (strip ? strip.downCells * strip.binS : 0);
