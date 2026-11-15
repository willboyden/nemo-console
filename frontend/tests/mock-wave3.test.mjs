// The mock's wave-3 data follows addendum 9.15: the three routes (summary, requests, profile), their 404 / 422 answers, the role rule for the profile's top values,
// and the new event keys. The UI is built against this mock, so it must behave like the contract.
import test, { before, after } from 'node:test';
import assert from 'node:assert/strict';
import { server } from '../dev/mock-server.mjs';
import { aiperfRequests, aiperfSummary, skillSummary, MAX_REQUEST_ROWS, RUN_ID_RE, stableKey } from '../dev/mock-wave3.mjs';
import { runSeeds } from '../dev/mock-runs.mjs';
import { percentileCurves, httpPhases, cleanRequests, PCT_KEYS } from '../js/aiperf-model.js';
import { benchesOf } from '../js/evaluator-model.js';
import { rewardsOf, rewardStats } from '../js/gym-model.js';
import { shapeRuns } from '../js/runs.js';
import { EVALUATOR, GYM } from '../js/run-descriptors.js';

let base;
before(
  () =>
    new Promise((r) =>
      server.listen(0, '127.0.0.1', () => {
        base = `http://127.0.0.1:${server.address().port}`;
        r();
      }),
    ),
);
after(() => {
  server.closeAllConnections?.();
  server.close();
});
const get = (p) => fetch(`${base}/api/v1${p}`);
const json = async (p) => (await get(p)).json();
const dev = (u) => fetch(`${base}${u}`, { headers: { 'X-Console': '1' } });
const events = async (id) => (await json(`/systems/${id}/events?limit=500`)).filter((e) => e.data?.run_id);
const enc = encodeURIComponent;
const REAL_NAMES = /spark\d|ai300|aitop|node\d|willaybee|willboyden/i;

test('aiperf summary: the documented shape, percentiles that rise, units named, throughput as an average only, nothing for a run that measured nothing', async () => {
  const evs = await events('aiperf');
  const ok = evs.find((e) => e.data.verdict === 'succeeded' && e.data.ttft_p50_ms != null);
  const s = await json(`/systems/aiperf/runs/${enc(ok.data.run_id)}/summary`);
  assert.deepEqual(Object.keys(s).sort(), ['concurrency', 'duration_s', 'http_phases_ms', 'isl', 'metrics', 'model', 'osl', 'preset', 'requests', 'run_id', 'verdict']);
  assert.equal(s.run_id, ok.data.run_id);
  assert.equal(s.concurrency, ok.data.concurrency);
  const m = s.metrics.time_to_first_token;
  assert.equal(m.unit, 'ms');
  assert.deepEqual(Object.keys(m).sort(), ['avg', 'max', 'min', 'p1', 'p10', 'p25', 'p5', 'p50', 'p75', 'p90', 'p95', 'p99', 'std', 'unit']);
  assert.ok(Math.abs(m.p50 - ok.data.ttft_p50_ms) < 0.05, "the median is the event's median");
  assert.ok(Math.abs(m.p99 - ok.data.ttft_p99_ms) / ok.data.ttft_p99_ms < 0.01, "and p99 is the event's p99");
  assert.ok(PCT_KEYS.every((k, i) => i === 0 || m[k] > m[PCT_KEYS[i - 1]]));
  const tput = s.metrics.output_token_throughput;
  assert.ok(tput.avg > 0 && tput.p50 === null && tput.p99 === null, 'a statistic the file lacks is null');
  assert.ok(Object.keys(s.metrics).length <= 100);
  assert.equal(percentileCurves(s, null).length, 3);
  const bad = evs.find((e) => e.data.verdict === 'failed');
  const f = await json(`/systems/aiperf/runs/${enc(bad.data.run_id)}/summary`);
  assert.deepEqual(f.metrics, {});
  assert.equal(f.http_phases_ms, null);
  assert.equal(httpPhases(f), null);
  // phases are recorded by most runs and missing from some
  const phases = await Promise.all(evs.filter((e) => e.data.verdict === 'succeeded').map(async (e) => (await json(`/systems/aiperf/runs/${enc(e.data.run_id)}/summary`)).http_phases_ms));
  assert.ok(phases.some((p) => p === null) && phases.some((p) => p && p.waiting > 0));
  for (const p of phases.filter(Boolean)) assert.deepEqual(Object.keys(p), ['dns', 'connecting', 'sending', 'waiting', 'receiving']);
});

test('aiperf requests: the documented shape, error codes (0 included), workers by first appearance, deterministic, sampled to 5000 and says so', async () => {
  const evs = await events('aiperf');
  const ok = evs.find((e) => e.data.verdict === 'succeeded' && e.data.error_rate_pct > 0);
  const a = await json(`/systems/aiperf/runs/${enc(ok.data.run_id)}/requests`);
  assert.deepEqual(Object.keys(a).sort(), ['returned', 'rows', 'run_id', 'total', 'truncated']);
  assert.equal(a.total, ok.data.requests);
  assert.equal(a.returned, a.rows.length);
  assert.equal(a.truncated, false);
  for (const r of a.rows) {
    assert.deepEqual(Object.keys(r).sort(), ['end_ms', 'error_code', 'i', 'input_tokens', 'itl_avg_ms', 'latency_ms', 'output_tokens', 'start_ms', 'ttft_ms', 'worker']);
    assert.ok(r.end_ms >= r.start_ms && r.start_ms >= 0);
    assert.ok(r.error_code === null || Number.isInteger(r.error_code));
  }
  assert.equal(Math.min(...a.rows.map((r) => r.start_ms)), 0, 'start_ms counts from the earliest start');
  assert.ok(
    a.rows.some((r) => r.error_code !== null),
    'a run with an error rate has failures',
  );
  assert.deepEqual(await json(`/systems/aiperf/runs/${enc(ok.data.run_id)}/requests`), a, 'two reads agree');
  assert.equal(cleanRequests(a).rows.length, a.rows.length);
  // the soak run has more requests than the route returns
  const soak = evs.find((e) => e.data.requests > MAX_REQUEST_ROWS);
  assert.ok(soak, 'the mock has a run with more than 5000 requests');
  const big = await json(`/systems/aiperf/runs/${enc(soak.data.run_id)}/requests`);
  assert.equal(big.total, soak.data.requests);
  assert.equal(big.returned, MAX_REQUEST_ROWS);
  assert.equal(big.truncated, true);
  assert.equal(big.rows.length, MAX_REQUEST_ROWS);
  assert.ok(
    big.rows.every((r, i) => i === 0 || r.i > big.rows[i - 1].i),
    'evenly sampled, in order',
  );
  const failed = evs.find((e) => e.data.verdict === 'failed');
  const f = await json(`/systems/aiperf/runs/${enc(failed.data.run_id)}/requests`);
  assert.ok(f.rows.length > 0 && f.rows.every((r) => r.error_code === 503), 'a failed run has only failures');
  // the function itself: error rows carry no latency parts, workers are small integers
  const direct = aiperfRequests({ data: { ...ok.data } });
  assert.ok(direct.rows.filter((r) => r.error_code !== null).every((r) => r.ttft_ms === null && r.output_tokens === 0));
});

test('a codes list includes 0 (a failure with no usable code) somewhere in the mock', async () => {
  const evs = (await events('aiperf')).filter((e) => e.data.error_rate_pct > 0);
  const codes = new Set();
  for (const e of evs) for (const r of (await json(`/systems/aiperf/runs/${enc(e.data.run_id)}/requests`)).rows) if (r.error_code !== null) codes.add(r.error_code);
  assert.ok(codes.has(0) && codes.has(429), [...codes].join());
});

test('skillevaluator summary: Tier 1 validators and rubric, Tier 3 agents, null for the tier a run did not have, numbers and identifiers only', async () => {
  const evs = await events('skillevaluator');
  const t1 = evs.find((e) => e.data.tier1 && e.data.tier1.rubric_score != null);
  const s1 = await json(`/systems/skillevaluator/runs/${enc(t1.data.run_id)}/summary`);
  assert.deepEqual(Object.keys(s1).sort(), ['run_id', 'skill', 'tier1', 'tier3', 'tiers', 'verdict']);
  assert.equal(s1.tier3, null);
  assert.ok(s1.tier1.validators.length >= 8);
  for (const v of s1.tier1.validators) {
    assert.deepEqual(Object.keys(v).sort(), ['findings', 'name', 'passed', 'status']);
    assert.deepEqual(Object.keys(v.findings), ['critical', 'high', 'medium', 'low', 'info']);
  }
  assert.ok(s1.tier1.rubric.length > 3 && s1.tier1.rubric.every((r) => typeof r.id === 'string' && typeof r.pass === 'boolean' && typeof r.judge_pass === 'boolean' && typeof r.score === 'number'));
  assert.ok(Object.values(s1.tier1.dimensions).every((x) => typeof x === 'number'));
  const t3 = evs.find((e) => e.data.tier3?.agents && Object.values(e.data.tier3.agents).some((a) => a.skill_lift != null));
  const s3 = await json(`/systems/skillevaluator/runs/${enc(t3.data.run_id)}/summary`);
  assert.equal(s3.tier1, null);
  assert.ok(s3.tier3.agents.length >= 2);
  assert.ok(s3.tier3.agents.every((a) => ['name', 'model', 'skill_lift', 'with_skill', 'without_skill', 'metrics'].every((k) => k in a)));
  assert.ok(s3.tier3.agents.some((a) => a.metrics.length >= 3 && a.metrics.every((m) => Math.abs(m.delta - (m.with_skill - m.without_skill)) < 0.002)));
  const failedT1 = evs.find((e) => e.data.tier1?.status === 'failed');
  const sf = await json(`/systems/skillevaluator/runs/${enc(failedT1.data.run_id)}/summary`);
  assert.ok(
    sf.tier1.validators.some((v) => v.passed === false),
    'a failed run has failing validators',
  );
  assert.ok(!/message|criterion|notes|conclusion|recommend|title/i.test(JSON.stringify(sf)), 'no skill- or model-written text');
});

test('the summary route is aiperf and skillevaluator only; requests is aiperf only; profile is datadesigner only; an unknown run is 404; a malformed id is 422', async () => {
  for (const [path, want] of [
    ['/systems/gym/runs/x/summary', 404],
    ['/systems/evaluator/runs/x/summary', 404],
    ['/systems/datadesigner/runs/x/summary', 404],
    ['/systems/skillevaluator/runs/x/requests', 404],
    ['/systems/aiperf/runs/x/profile', 404],
    ['/systems/aiperf/runs/nope/summary', 404],
    ['/systems/aiperf/runs/nope/requests', 404],
    ['/systems/skillevaluator/runs/nope/summary', 404],
    ['/systems/datadesigner/runs/nope/profile', 404],
    ['/systems/aiperf/runs/bad%20id/summary', 422],
    ['/systems/datadesigner/runs/a%2Fb/profile', 422],
    ['/systems/skillevaluator/runs/%E0%A4%A/summary', 422],
  ]) {
    const r = await get(path);
    assert.equal(r.status, want, `${path} -> ${r.status}`);
    const b = await r.json();
    assert.equal(b.error.code, want === 404 ? 'not_found' : 'invalid_request', path);
  }
  assert.ok(RUN_ID_RE.test('20261112T091512-104-chat-short') && RUN_ID_RE.test('abc#2') && !RUN_ID_RE.test('a b') && !RUN_ID_RE.test(''));
});

test('profile: an admin gets top values and values_hidden false; a viewer gets top null and values_hidden true; the aggregates are the same for both', async () => {
  const runs = (await events('datadesigner')).filter((e) => e.data.verdict === 'succeeded' && e.data.num_records > 0);
  const id = runs[0].data.run_id;
  await dev('/__dev/role?set=admin');
  const a = await json(`/systems/datadesigner/runs/${enc(id)}/profile`);
  await dev('/__dev/role?set=viewer');
  const v = await json(`/systems/datadesigner/runs/${enc(id)}/profile`);
  await dev('/__dev/role?set=admin');
  assert.deepEqual(Object.keys(a).sort(), ['columns', 'rows', 'run_id', 'truncated', 'values_hidden']);
  assert.equal(a.values_hidden, false);
  assert.equal(v.values_hidden, true);
  assert.ok(a.rows > 0 && a.rows === v.rows);
  assert.ok(a.columns.some((c) => Array.isArray(c.top) && c.top.length > 0));
  assert.ok(v.columns.every((c) => c.top === null));
  assert.deepEqual(
    a.columns.map((c) => ({ ...c, top: null })),
    v.columns,
    'only top differs',
  );
  for (const c of a.columns) {
    assert.deepEqual(Object.keys(c).sort(), ['distinct', 'distinct_capped', 'length', 'name', 'non_null', 'numeric', 'top', 'types']);
    assert.deepEqual(Object.keys(c.types), ['string', 'number', 'bool', 'null', 'object', 'array']);
    assert.equal(
      Object.values(c.types).reduce((x, y) => x + y, 0),
      a.rows,
    );
    assert.ok(c.top === null || c.top.length <= 10);
    for (const t of c.top ?? []) assert.ok(String(t.value).length <= 64);
  }
  const csat = a.columns.find((c) => c.name === 'csat');
  assert.ok(csat.types.null > 0 && csat.numeric.min >= 1 && csat.numeric.max <= 5);
  assert.ok(a.columns.some((c) => c.types.bool > 0) && a.columns.some((c) => c.types.array > 0));
  // a failed run wrote nothing: an empty profile, not an error
  const failed = (await events('datadesigner')).find((e) => e.data.verdict === 'failed');
  const e = await json(`/systems/datadesigner/runs/${enc(failed.data.run_id)}/profile`);
  assert.equal(e.rows, 0);
  assert.deepEqual(e.columns, []);
});

test('the new event keys: evaluator benchmarks always carry ci_low, ci_high, tokens, elapsed_s, failure_rate (null for an older run); gym.smoke carries rewards (null when none)', async () => {
  const ev = await events('evaluator');
  const benches = ev.flatMap((e) => e.data.benchmarks);
  assert.ok(benches.length > 10);
  for (const b of benches) for (const k of ['ci_low', 'ci_high', 'tokens', 'elapsed_s', 'failure_rate']) assert.ok(k in b, k);
  const rich = benches.filter((b) => b.ci_low !== null);
  assert.ok(rich.length > 5 && rich.length < benches.length, 'some runs have them, older ones do not');
  for (const b of rich) {
    assert.ok(b.ci_low <= b.score && b.score <= b.ci_high, 'the score is inside its interval');
    assert.ok(b.tokens > 0 && b.elapsed_s > 0 && b.failure_rate >= 0 && b.failure_rate < 0.2);
  }
  const runs = shapeRuns(ev, EVALUATOR);
  assert.ok(runs.some((r) => benchesOf(r).some((b) => b.lo !== null)));
  const gym = await events('gym');
  const withRewards = gym.filter((e) => Array.isArray(e.data.rewards));
  assert.ok(withRewards.length >= 5);
  for (const e of withRewards) {
    assert.ok(e.data.rewards.length > 0 && e.data.rewards.length <= 20 && e.data.rewards.every((x) => typeof x === 'number' && x >= 0 && x <= 1));
    assert.equal(e.data.rewarded, e.data.rewards.length, '`rewarded` = the rollouts that produced a reward');
    assert.ok(e.data.rewarded <= e.data.rollouts);
    assert.ok(Math.abs(rewardStats(e.data.rewards).mean - e.data.mean_reward) < 0.002, 'the mean agrees with the rewards');
  }
  assert.ok(gym.filter((e) => !Array.isArray(e.data.rewards)).every((e) => e.data.rewards === null && e.data.verdict !== 'succeeded'));
  assert.ok(shapeRuns(gym, GYM).some((r) => rewardsOf(r).length > 0));
});

test("a model optimizer job is still running (its age is the card's subject) and the other jobs are finished or failed", async () => {
  const jobs = (await json('/systems/modelopt/events?limit=500')).filter((e) => e.data?.job_id);
  const running = jobs.filter((e) => e.data.verdict === 'running');
  assert.equal(running.length, 1);
  assert.equal(running[0].data.duration_ms, null);
  assert.ok(jobs.filter((e) => e.data.verdict === 'succeeded').every((e) => e.data.output_bytes > 0 && e.data.base_bytes > e.data.output_bytes));
});

test('generic names only: nothing the wave-3 routes answer names a real machine, host or person', async () => {
  const bodies = [];
  for (const sys of ['aiperf', 'skillevaluator']) for (const e of (await events(sys)).slice(0, 4)) bodies.push(JSON.stringify(await json(`/systems/${sys}/runs/${enc(e.data.run_id)}/summary`)));
  for (const e of (await events('aiperf')).slice(0, 3)) bodies.push(JSON.stringify(await json(`/systems/aiperf/runs/${enc(e.data.run_id)}/requests`)));
  for (const e of (await events('datadesigner')).slice(0, 3)) bodies.push(JSON.stringify(await json(`/systems/datadesigner/runs/${enc(e.data.run_id)}/profile`)));
  for (const b of bodies) assert.ok(!REAL_NAMES.test(b), b.slice(0, 120));
});

test('deterministic across starts: the ids move with the clock, but every figure (the new event keys, the summaries, the request rows) is the same', () => {
  const strip = (evs) =>
    evs.map((e) => JSON.parse(JSON.stringify({ ...e, ts: 0, data: { ...e.data, ...(e.data.run_id ? { run_id: stableKey(e.data.run_id) } : { job_id: stableKey(e.data.job_id) }) } })));
  const a = runSeeds(1_800_000_000),
    b = runSeeds(1_800_000_000 + 5 * 86400 + 123);
  for (const sys of ['aiperf', 'evaluator', 'gym', 'modelopt', 'skillevaluator']) assert.deepEqual(strip(a[sys]), strip(b[sys]), `${sys} events`);
  assert.notEqual(a.aiperf[0].data.run_id, b.aiperf[0].data.run_id, 'the ids themselves differ');
  const strip2 = (o) => JSON.parse(JSON.stringify(o, (k, v) => (k === 'run_id' ? stableKey(v) : v)));
  for (let i = 0; i < 6; i++) {
    assert.deepEqual(strip2(aiperfSummary(a.aiperf[i])), strip2(aiperfSummary(b.aiperf[i])));
    assert.deepEqual(strip2(aiperfRequests(a.aiperf[i])), strip2(aiperfRequests(b.aiperf[i])));
    assert.deepEqual(strip2(skillSummary(a.skillevaluator[i])), strip2(skillSummary(b.skillevaluator[i])));
  }
});
