// Pure shaping for the NeMo Gym cards (addendum 9.15), from the `gym.smoke` events: the per-task reward distribution of a run (and an A/B
// overlay) from the `rewards` array, rewarded-of-rollouts bars per run, and the mean reward / pass@1 trends by model. No DOM. A run
// that recorded no rewards has none (an empty list), never a list of zeros. Unit-tested in tests/gym-model.test.mjs.
import { trendGroups } from './runs.js';
import { sortedNums, mean } from './viz/stats.js';

const isNum = (n) => typeof n === 'number' && Number.isFinite(n);

/** The most rewards an event carries (the contract's bound). */
export const MAX_REWARDS = 20;

/** The rewards of a run row (runs.shapeRuns): the finite numbers of `data.rewards`, at most MAX_REWARDS, in task order; [] when the run has none. */
export function rewardsOf(row) {
  const r = row?.event?.data?.rewards;
  return Array.isArray(r) ? r.filter(isNum).slice(0, MAX_REWARDS) : [];
}

/** {n, mean, min, max, full (rewards of 1 or more), zero (rewards of 0 or less)} of a list of rewards; mean / min / max are null for none. */
export function rewardStats(rewards) {
  const v = sortedNums(rewards);
  return { n: v.length, mean: mean(v), min: v.length ? v[0] : null, max: v.length ? v[v.length - 1] : null, full: v.filter((x) => x >= 1).length, zero: v.filter((x) => x <= 0).length };
}

/**
 * Bins for the reward distribution, shared by A and B so the two overlay. The range is 0..1 (a reward is a share) unless a reward is
 * outside it, then the data's own range widened to whole steps. Every reward falls in exactly one bin; the last bin includes the top edge.
 * Returns {edges, labels, a: counts, b: counts | null, lo, hi}. Labels read "0.0-0.1" ("0.9-1.0" is the last, closed bin).
 */
export function rewardBins(rewardsA, rewardsB = null, { bins = 10 } = {}) {
  const all = [...(rewardsA || []), ...(rewardsB || [])].filter(isNum);
  let lo = 0,
    hi = 1;
  if (all.length) {
    lo = Math.min(0, Math.floor(Math.min(...all) * 10) / 10);
    hi = Math.max(1, Math.ceil(Math.max(...all) * 10) / 10);
  }
  const step = (hi - lo) / bins;
  const edges = Array.from({ length: bins + 1 }, (_, i) => +(lo + step * i).toFixed(6));
  const count = (list) => {
    const c = new Array(bins).fill(0);
    for (const x of (list || []).filter(isNum)) c[Math.min(bins - 1, Math.max(0, Math.floor((x - lo) / step + 1e-9)))]++;
    return c;
  };
  const fmt = (v) => (Number.isInteger(v * 10) ? v.toFixed(1) : String(+v.toFixed(2)));
  return { edges, labels: edges.slice(0, -1).map((e, i) => `${fmt(e)}–${fmt(edges[i + 1])}`), a: count(rewardsA), b: rewardsB ? count(rewardsB) : null, lo, hi };
}

/**
 * The rollouts of the last `max` runs (of the rows given, oldest first) that have rollouts: [{id, label, rewarded, rollouts, none}] where `none` = rollouts with no
 * reward (rollouts - rewarded; null when the event has no `rewarded`). A run with no rollouts count is left out, never drawn as zero.
 */
export function rolloutRows(rows, { max = 12 } = {}) {
  const out = [];
  for (const r of rows) {
    const rollouts = r.fields?.rollouts,
      rewarded = r.fields?.rewarded;
    if (!isNum(rollouts) || rollouts <= 0) continue;
    const got = isNum(rewarded) ? Math.min(rollouts, Math.max(0, rewarded)) : null;
    out.push({ id: r.id, label: r.label, ts: r.ts, rewarded: got, rollouts, none: got == null ? null : rollouts - got });
  }
  return out.slice(-max);
}

/** The model trends: the mean reward and pass@1 across the rows, one group (line) per model — runs.trendGroups with `model` as the group. */
export function modelTrends(rows) {
  return {
    mean_reward: trendGroups(rows, { field: 'mean_reward' }, 'model'),
    pass_at_1: trendGroups(rows, { field: 'pass_at_1' }, 'model'),
  };
}
