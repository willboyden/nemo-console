// MOCK run events for the eight run-based systems (addendum 9.13): 10-30 runs each over a few days, several models / presets /
// scripts, a few failures, interruptions and one still running. MOCK-ONLY: every number is invented. The SHAPES follow what the
// Rust adapters put into SystemEvent.data (backend/crates/nemo-console/src/adapters/<system>.rs `event()`): the same keys, null
// where the adapter writes null, metric keys only for a run that measured them. Newest first, like the backend's run listing.
// Deterministic (its own seeded generator), so the history looks the same on every start.

const DAY = 86400;
let seed = 20261114;
const rnd = () => {
  seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0;
  return seed / 4294967296;
};
const pick = (a) => a[Math.floor(rnd() * a.length)];
const jit = (base, pct) => base * (1 + (rnd() * 2 - 1) * pct);
const r1 = (v) => Math.round(v * 10) / 10;
const r3 = (v) => Math.round(v * 1000) / 1000;
const pad = (n, w = 2) => String(n).padStart(w, '0');
/** `20261112T091512-104-<name>` like the adapters' run directories. */
const runId = (ts, name) => {
  const d = new Date(ts * 1000);
  return `${d.getUTCFullYear()}${pad(d.getUTCMonth() + 1)}${pad(d.getUTCDate())}T${pad(d.getUTCHours())}${pad(d.getUTCMinutes())}${pad(d.getUTCSeconds())}-${pad(Math.floor(rnd() * 1000), 3)}-${name}`;
};
/** n times spread over `days` days back from `now`, newest first, with the newest a little while ago. */
const times = (now, n, days, newestAgo = 1500) => {
  const out = [];
  for (let i = 0; i < n; i++) out.push(now - newestAgo - (i / Math.max(1, n - 1)) * (days * DAY - newestAgo) + (i ? (rnd() - 0.5) * 3600 : 0));
  return out.sort((a, b) => b - a);
};
const E = (system, ts, kind, summary, data) => ({ ts, system, kind, summary, data });

// ---- aiperf: preset x model, ttft / itl / throughput drift slowly, a few failures
function aiperf(now) {
  const presets = {
    'chat-short': { c: 8, requests: 200, isl: 256, osl: 128, k: 1 },
    'chat-long': { c: 4, requests: 60, isl: 8192, osl: 256, k: 4.5 },
    'lightning-burst': { c: 16, requests: 400, isl: 128, osl: 64, k: 0.7 },
  };
  const models = { 'qwen3.8-flash-next': { ttft: 190, itl: 17, tps: 1850 }, 'glm-5.3-flash-fp4-local': { ttft: 270, itl: 24, tps: 1420 }, 'nemotron-lightning': { ttft: 95, itl: 9, tps: 2650 } };
  return times(now, 26, 16).map((ts, i) => {
    const preset = i % 7 === 3 ? 'chat-long' : i % 3 === 0 ? 'lightning-burst' : 'chat-short';
    const model = preset === 'lightning-burst' ? 'nemotron-lightning' : i % 2 ? 'glm-5.3-flash-fp4-local' : 'qwen3.8-flash-next';
    const p = presets[preset],
      m = models[model];
    const fail = i === 5 || i === 17,
      interrupted = i === 11;
    const verdict = fail ? 'failed' : interrupted ? 'interrupted' : 'succeeded';
    const drift = 1 + (i - 13) * 0.006; // the older runs were a little slower
    const data = {
      run_id: runId(ts, preset),
      preset,
      model,
      verdict,
      concurrency: p.c,
      requests: p.requests,
      isl: p.isl,
      osl: p.osl,
      duration_ms: verdict === 'succeeded' ? Math.round(jit(70000 * p.k, 0.2)) : fail ? Math.round(jit(9000, 0.3)) : null,
    };
    if (verdict === 'succeeded') {
      Object.assign(data, {
        ttft_p50_ms: r1(jit(m.ttft * p.k * drift, 0.08)),
        ttft_p99_ms: r1(jit(m.ttft * p.k * drift * 2.6, 0.12)),
        itl_p50_ms: r1(jit(m.itl * drift, 0.06)),
        output_tps: r1(jit(m.tps / Math.sqrt(p.k) / drift, 0.05)),
        request_tps: r1(jit((p.c * 1.6) / Math.sqrt(p.k), 0.08)),
        error_rate_pct: i % 9 === 4 ? r1(jit(1.4, 0.3)) : 0,
      });
    }
    const summary = [
      preset,
      model,
      `c=${p.c}`,
      verdict,
      data.ttft_p50_ms != null ? `TTFT p50 ${Math.round(data.ttft_p50_ms)} ms` : null,
      data.output_tps != null ? `${Math.round(data.output_tps)} output tok/s` : null,
    ]
      .filter(Boolean)
      .join(' · ');
    return E('aiperf', ts, 'aiperf.run', summary, data);
  });
}

// ---- evaluator: suite x model, scores per benchmark
function evaluator(now) {
  const suites = { smoke: ['gsm8k'], quick: ['gsm8k', 'mmlu'], full: ['gsm8k', 'mmlu', 'mmlu_pro'] };
  const base = {
    'qwen3.8-flash-next': { gsm8k: 0.87, mmlu: 0.78, mmlu_pro: 0.61 },
    'glm-5.3-flash-fp4-local': { gsm8k: 0.9, mmlu: 0.8, mmlu_pro: 0.64 },
    'nemotron-lightning': { gsm8k: 0.74, mmlu: 0.69, mmlu_pro: 0.48 },
  };
  return times(now, 20, 14, 5 * 3600).map((ts, i) => {
    const suite = i % 5 === 4 ? 'full' : i % 2 ? 'quick' : 'smoke';
    const model = ['qwen3.8-flash-next', 'glm-5.3-flash-fp4-local', 'nemotron-lightning'][i % 3];
    const fail = i === 7,
      interrupted = i === 14;
    const verdict = fail ? 'failed' : interrupted ? 'interrupted' : 'succeeded';
    const benchmarks =
      verdict === 'succeeded'
        ? suites[suite].map((b) => ({
            bench: b,
            status: 'succeeded',
            metric: 'accuracy',
            score: r3(Math.min(0.99, jit(base[model][b], 0.04))),
            samples: suite === 'smoke' ? 50 : suite === 'quick' ? 200 : 500,
          }))
        : [];
    const data = {
      run_id: runId(ts, suite),
      suite,
      model,
      verdict,
      duration_ms: verdict === 'succeeded' ? Math.round(jit(suite === 'smoke' ? 45000 : suite === 'quick' ? 240000 : 900000, 0.2)) : fail ? 4000 : null,
      benchmarks,
    };
    const scores = benchmarks.map((b) => `${b.bench} ${b.score.toFixed(3)}`).join(', ');
    return E('evaluator', ts, 'evaluator.run', [suite, model, scores, verdict].filter(Boolean).join(' · '), data);
  });
}

// ---- gym: smoke runs, mean reward and pass@1 per model
function gym(now) {
  const models = { 'glm-5.3-flash-fp4-local': 0.82, 'qwen3.8-flash-next': 0.74, 'nemotron-lightning': 0.58 };
  return times(now, 16, 12, 1800).map((ts, i) => {
    const model = Object.keys(models)[i % 3];
    const limit = i % 4 === 3 ? 10 : 5;
    const fail = i === 4 || i === 12,
      interrupted = i === 9;
    const verdict = i === 0 ? 'running' : fail ? 'failed' : interrupted ? 'interrupted' : 'succeeded'; // the newest is still going
    const data = { run_id: runId(ts, 'smoke'), model, limit, verdict, duration_ms: verdict === 'succeeded' ? Math.round(jit(limit * 29000, 0.2)) : fail ? 8000 : null };
    if (verdict === 'succeeded') {
      const reward = Math.min(1, Math.max(0, jit(models[model], 0.12)));
      const rewarded = Math.round(reward * limit);
      Object.assign(data, { rollouts: limit, rewarded, mean_reward: r3(rewarded / limit), pass_at_1: r3(rewarded / limit) });
    }
    const summary = [
      model,
      verdict,
      data.rollouts != null ? `${data.rollouts} rollouts` : null,
      data.mean_reward != null ? `mean reward ${data.mean_reward.toFixed(2)}` : null,
      data.duration_ms != null ? `${(data.duration_ms / 60000).toFixed(1)} min` : null,
    ]
      .filter(Boolean)
      .join(' · ');
    return E('gym', ts, 'gym.smoke', summary, data);
  });
}

// ---- modelopt: compression jobs; a lower ratio is a smaller model
function modelopt(now) {
  const recipes = {
    'reap-glm53': { method: 'reap', base_model: 'zai-org/GLM-5.3-Flash', base: 1.9e11, ratio: 0.75 },
    'quantize-glm53-nvfp4': { method: 'quantize', base_model: 'zai-org/GLM-5.3-Flash', base: 1.9e11, ratio: 0.31 },
    'minitron-qwen38': { method: 'minitron', base_model: 'Qwen/Qwen3.8-27B', base: 5.4e10, ratio: 0.62 },
  };
  return times(now, 13, 18, 2 * 3600).map((ts, i) => {
    const recipe = Object.keys(recipes)[i % 3];
    const r = recipes[recipe];
    const fail = i === 3 || i === 10;
    const verdict = fail ? 'failed' : i === 7 ? 'interrupted' : 'succeeded';
    const data = {
      job_id: runId(ts, recipe),
      recipe,
      method: r.method,
      base_model: r.base_model,
      output: `/runs/${recipe}`,
      verdict,
      duration_ms: verdict === 'succeeded' ? Math.round(jit(r.method === 'quantize' ? 2.4e6 : 3.1e6, 0.15)) : fail ? 120000 : null,
    };
    if (verdict === 'succeeded') {
      const ratio = r3(jit(r.ratio, 0.04));
      Object.assign(data, { output_bytes: Math.round(r.base * ratio), base_bytes: r.base, ratio });
    }
    const gib = data.output_bytes != null ? `${(data.output_bytes / 1073741824).toFixed(2)} GiB` : null;
    return E(
      'modelopt',
      ts,
      'modelopt.job',
      [recipe, r.method, verdict, data.duration_ms != null ? `${(data.duration_ms / 60000).toFixed(1)} min` : null, gib, data.ratio != null ? `${(data.ratio * 100).toFixed(1)}% of the base` : null]
        .filter(Boolean)
        .join(' · '),
      data,
    );
  });
}

// ---- skillevaluator: tier1 (static / judge / +tier2) and tier3 (Skill Lift) runs
function skilleval(now) {
  const skills = ['write-skill-for-weak-models', 'serve-model', 'sandbox-mcp', 'verify-change', 'robust-scripts', 'grafana-no-data'];
  return times(now, 24, 10, 7200).map((ts, i) => {
    const skill = skills[i % skills.length];
    const tiers = i % 8 === 7 ? 'tier3' : i % 4 === 1 ? 'static' : i % 4 === 2 ? 'tier1+2' : 'tier1';
    const data = { run_id: runId(ts, skill), skill, tiers, verdict: 'passed', model_calls: tiers !== 'static', tier1: null, tier2_status: null, duration_ms: 0 };
    let summary;
    if (tiers === 'tier3') {
      const lift = r3(jit(0.12, 0.9));
      const withS = r3(Math.min(0.98, 0.62 + Math.abs(lift)));
      const failed = i === 15;
      data.verdict = failed ? 'failed' : 'succeeded';
      data.duration_ms = Math.round(jit(540000, 0.2));
      data.tier3 = {
        execution_status: failed ? 'failed' : 'succeeded',
        scored_attempts: failed ? 3 : 8,
        expected_attempts: 8,
        duration_seconds: r1(data.duration_ms / 1000),
        agents: {
          opencode: {
            model: 'glm-5.3-flash-fp4-local',
            skill_lift: failed ? null : lift,
            with_skill: failed ? null : withS,
            without_skill: failed ? null : r3(withS - lift),
            execution_status: failed ? 'failed' : 'succeeded',
          },
          'claude-local': {
            model: 'qwen3.8-flash-next',
            skill_lift: failed ? null : r3(lift * 0.6),
            with_skill: failed ? null : r3(withS - 0.04),
            without_skill: failed ? null : r3(withS - 0.04 - lift * 0.6),
            execution_status: failed ? 'failed' : 'succeeded',
          },
        },
      };
      data.live = {};
      summary = `${skill} · tier3 · ${failed ? '' : `lift ${lift >= 0 ? '+' : ''}${lift.toFixed(3)} (opencode) · `}${data.verdict} (${data.tier3.scored_attempts}/8 attempts scored)`;
    } else {
      const bad = i === 6 || i === 13,
        incomplete = i === 19;
      const q = r1(bad ? jit(46, 0.12) : Math.min(99, jit(86, 0.07)));
      const status = bad ? 'failed' : incomplete ? 'incomplete' : 'passed';
      data.verdict = status;
      data.duration_ms = Math.round(tiers === 'static' ? jit(2500, 0.3) : tiers === 'tier1' ? jit(21000, 0.3) : jit(48000, 0.3));
      data.tier1 = {
        status,
        quality_score: q,
        grade: q >= 90 ? 'A' : q >= 80 ? 'B' : q >= 65 ? 'C' : 'D',
        rubric_score: tiers === 'static' ? null : r1(Math.min(99, jit(80, 0.1))),
        errors: bad ? 3 + (i % 3) : 0,
        warnings: Math.round(jit(3, 0.8)),
        high: bad ? 1 : 0,
        critical: bad && i === 13 ? 1 : 0,
        incomplete: incomplete ? ['llm-security'] : [],
      };
      if (tiers === 'tier1+2') data.tier2_status = 'passed';
      summary = [skill, tiers, `quality ${q.toFixed(1)} ${data.tier1.grade}`, data.tier1.rubric_score != null ? `judge ${data.tier1.rubric_score.toFixed(1)}` : null, status]
        .filter(Boolean)
        .join(' · ');
    }
    return E('skillevaluator', ts, 'skilleval.run', summary, data);
  });
}

// ---- dcgm: diagnostic runs per GPU and level; the tests map is {name: status}
function dcgm(now) {
  const L1 = ['Denylist', 'NVML Library', 'CUDA Main Library', 'Permissions and OS Blocks', 'Persistence Mode', 'Environment Variables', 'Page Retirement/Row Remap', 'Graphics Processes', 'Inforom'];
  const L2 = [...L1, 'Memory', 'Diagnostic', 'PCIe'];
  const model = ['NVIDIA RTX PRO 6000 Blackwell Max-Q', 'NVIDIA RTX PRO 6000 Blackwell Workstation'];
  return times(now, 22, 9, 1800).map((ts, i) => {
    const gpu = i % 2,
      level = i % 5 === 4 ? 2 : 1;
    const names = level === 2 ? L2 : L1;
    const err = i === 12;
    const tests = {};
    if (!err)
      for (const n of names) {
        let s = 'Pass';
        if (n === 'Persistence Mode' && i % 6 === 1) s = 'Warn';
        if (n === 'Graphics Processes' && i % 4 === 0) s = 'Skip';
        if (n === 'Memory' && i === 9) s = 'Fail';
        if (n === 'PCIe' && i === 14) s = 'Warn';
        tests[n] = s;
      }
    const st = Object.values(tests);
    const verdict = err ? 'error' : st.some((s) => s === 'Fail') ? 'fail' : st.some((s) => s === 'Warn') ? 'warn' : 'pass';
    const passed = st.filter((s) => s === 'Pass').length;
    const data = { run_id: runId(ts, `diag-gpu${gpu}`), gpu, level, verdict, duration_ms: err ? 600 : Math.round(jit(level === 2 ? 168000 : 13000, 0.12)), model: model[gpu], tests };
    return E('dcgm', ts, 'dcgm.diag', `diag level ${level} on GPU ${gpu}: ${verdict} (${passed}/${st.length} tests passed)`, data);
  });
}

// ---- guardrails: the `check` action: three fixed probes through one rails config
function guardrails(now) {
  const PROBES = [
    ['benign', 'passed'],
    ['jailbreak', 'blocked'],
    ['harmful', 'blocked'],
  ];
  const verdict = (s) => (s === 'passed' ? 'allowed' : s);
  return times(now, 21, 8, 900).map((ts, i) => {
    const config = i % 7 === 5 ? 'lab-default' : 'lab-input-output';
    const slip = i === 4 || i === 15; // the rails let a jailbreak through
    const down = i === 9; // the router did not answer the second probe
    const results = [];
    for (const [probe, expected] of PROBES) {
      if (down && probe !== 'benign') {
        if (probe === 'jailbreak') results.push({ probe, status: 'error', rail: null, ms: 90001 });
        break;
      }
      const status = slip && probe === 'jailbreak' ? 'passed' : expected;
      results.push({ probe, status, rail: status === 'blocked' ? 'self check input' : null, ms: Math.round(jit(probe === 'benign' ? 520 : 780, 0.35)) });
    }
    const ok = !slip && !down;
    return E('guardrails', ts, 'guardrails.check', `check ${config}: ${results.map((r) => `${r.probe}=${verdict(r.status)}`).join(', ')}`, { config, ok, results });
  });
}

/** Everything the mock seeds: {system: [events newest first]} for the seven systems (datadesigner is built from DD_RUNS below). */
export function runSeeds(now) {
  seed = 20261114;
  return { aiperf: aiperf(now), evaluator: evaluator(now), gym: gym(now), modelopt: modelopt(now), skillevaluator: skilleval(now), dcgm: dcgm(now), guardrails: guardrails(now) };
}

/**
 * Extra Data Designer runs behind the three hand-written ones (so the Runs tab and the history agree): returns run objects in
 * the shape of mock-data.mjs DD_RUNS, newest first, `columns` / `rows` / `files` supplied by the caller's helpers.
 */
export function extraDdRuns(now, { cols, rows, previewFiles, createFiles }) {
  seed = 20261114 ^ 0x5bd1e995;
  const scripts = ['support_tickets.py', 'code_review_prs.py', 'release_notes.py'];
  const out = [];
  for (const ts of times(now, 19, 9, 2 * DAY).slice(0, 19)) {
    const i = out.length;
    const script = scripts[i % 3];
    const mode = i % 3 === 1 ? 'preview' : 'create';
    const fail = i === 3 || i === 11,
      interrupted = i === 8;
    const verdict = fail ? 'failed' : interrupted ? 'interrupted' : 'succeeded';
    const requested = mode === 'preview' ? pick([3, 5, 10]) : pick([100, 200, 500, 1000]);
    const format = mode === 'create' ? pick(['parquet', 'csv', 'jsonl']) : 'parquet';
    const dataset = mode === 'create' ? script.replace('.py', '') + (i % 2 ? '_v2' : '') : null;
    const records = verdict === 'succeeded' ? requested : fail ? 0 : null;
    const run = {
      run_id: runId(ts, `${mode}-${script.replace('.py', '')}`),
      script,
      mode,
      verdict,
      num_records: records,
      num_records_requested: requested,
      batches: mode === 'create' && verdict === 'succeeded' ? Math.ceil(requested / 100) : null,
      dataset_name: dataset,
      destination: dataset,
      format,
      duration_ms: verdict === 'succeeded' ? Math.round(mode === 'preview' ? jit(6500, 0.3) : requested * jit(2100, 0.25)) : fail ? Math.round(jit(70000, 0.3)) : null,
      ts,
      columns: cols,
      rows: rows(Math.min(records || 0, 60)),
      files: verdict === 'succeeded' ? (mode === 'preview' ? previewFiles(records) : createFiles({ dataset_name: dataset, num_records: records })) : [],
    };
    if (fail) run.error = 'the model answered 429 for 3 consecutive retries (records: 0 written)';
    out.push(run);
  }
  return out;
}
