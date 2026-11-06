// Canned fixtures for the seven systems of ARCHITECTURE.md 9.8, 10.9 and 10.10: openshell, dcgm, aiperf, guardrails, evaluator, modelopt, gym.
// MOCK-ONLY: every number, version, path, container name and score below is invented so the UI can be built and
// screenshotted against contract-shaped data (domain/models.py). None of it is a fact about the real systems, and
// the real adapters are being written separately (metric keys beyond the documented ap.* / ev.score are guesses).
export const SEVEN = ['openshell', 'dcgm', 'aiperf', 'guardrails', 'evaluator', 'modelopt', 'gym', 'flare'];
// skillevaluator is not new in 9.8; it is mocked here only so the twelve-system overview is complete in dev.
export const MOCKED = [...SEVEN, 'skillevaluator'];
export const SEVEN_LABEL = {
  skillevaluator: 'NVIDIA SkillEvaluator',
  openshell: 'NVIDIA OpenShell',
  dcgm: 'NVIDIA DCGM',
  aiperf: 'NVIDIA AIPerf',
  guardrails: 'NeMo Guardrails',
  evaluator: 'NeMo Evaluator',
  modelopt: 'NVIDIA Model Optimizer',
  gym: 'NeMo Gym',
  flare: 'NVIDIA Flare',
};

// ------------------------------------------------------------------ status (one example per severity across the set)
// openshell: up + info (live shape)  | dcgm: degraded + error | aiperf: up + warn | guardrails: up | evaluator: up + warn + info
export function sevenStatus(id, now, flags = {}) {
  const checked_at = now;
  if (id === 'openshell')
    return {
      system: id,
      health: 'up',
      version: '0.9.4-mock',
      summary: 'Gateway up on 127.0.0.1:8443; 0 sandboxes; sandbox network is internal',
      endpoints: [{ name: 'gateway', url: 'https://127.0.0.1:8443', role: 'api', auth: 'bearer' }],
      components: [
        { kind: 'process', name: 'openshell CLI', state: 'running', detail: 'openshell 0.9.4-mock' },
        { kind: 'container', name: 'openshell-gateway', state: 'running', detail: 'healthy, up 2 d' },
        { kind: 'http', name: 'network openshell-docker', state: 'running', detail: 'internal: true (no route out)' },
      ],
      problems: [
        {
          severity: 'info',
          code: 'openshell.version.skew',
          message: 'The CLI (0.9.4-mock) and the gateway (0.9.3-mock) differ by a patch version. Harmless today; keep them together before the next upgrade. (mock)',
          fix: 'openshell doctor',
        },
      ],
      checked_at,
    };
  if (id === 'skillevaluator')
    return {
      system: id,
      health: 'up',
      version: '0.5.0-mock',
      summary: '66 skills · 12 stored runs · last quality 87.5 (mock)',
      endpoints: [{ name: 'skilleval MCP', url: 'http://127.0.0.1:8797', role: 'api', auth: 'bearer' }],
      components: [
        { kind: 'process', name: 'skilleval server', state: 'running', detail: 'v0.5.0-mock' },
        { kind: 'http', name: 'router http://127.0.0.1:4000', state: 'running', detail: '200' },
      ],
      problems: [],
      checked_at,
    };
  if (id === 'dcgm')
    return {
      system: id,
      health: 'degraded',
      version: '4.4.1-mock',
      summary: '2 GPUs reporting on this host; exporter image is older than 4.8.3 (mock)',
      endpoints: [
        { name: 'exporter metrics', url: 'http://127.0.0.1:9400/metrics', role: 'metrics', auth: 'none' },
        { name: 'prometheus', url: 'http://127.0.0.1:9090', role: 'api', auth: 'none' },
      ],
      components: [
        { kind: 'container', name: 'dcgm-exporter', state: 'running', image: 'nvcr.io/nvidia/k8s/dcgm-exporter:4.4.1-4.1.0-mock', detail: 'healthy, up 6 d' },
        { kind: 'http', name: 'exporter /metrics', state: 'running', detail: '200, 2 GPUs' },
        { kind: 'http', name: 'prometheus job dcgm-aitop100', state: 'failed', detail: 'target down: connection refused (mock)' },
        { kind: 'http', name: 'prometheus job dcgm', state: 'running', detail: '2 GPUs' },
      ],
      problems: flags.dcgmFixed
        ? []
        : [
            {
              severity: 'error',
              code: 'dcgm.exporter.vulnerable',
              message: 'The running dcgm-exporter (4.4.1) is older than 4.8.3 and exposes /debug/pprof (CVE-2026-47483). A direct probe of /debug/pprof/ answered 200. (mock data)',
              fix: 'observability/dcgm/bump-exporter.sh 4.8.3   # then: docker compose up -d dcgm-exporter',
            },
            { severity: 'warn', code: 'dcgm.host.down', message: 'Prometheus job dcgm-aitop100 has no live target; that host is missing from the numbers below.' },
          ],
      checked_at,
    };
  if (id === 'aiperf')
    return {
      system: id,
      health: 'up',
      version: '0.6.0-mock',
      summary: '4 profiles · last run: chat-short (2026-10-24 08:12, 40 req/s sustained)',
      endpoints: [{ name: 'router (load target)', url: 'http://127.0.0.1:4000/v1', role: 'api', auth: 'bearer' }],
      components: [
        { kind: 'process', name: 'aiperf-local', state: 'running', detail: 'aiperf 0.6.0-mock (wrapper injects its own key file)' },
        { kind: 'http', name: 'router http://127.0.0.1:4000', state: 'running', detail: '200 on /health/liveliness' },
        { kind: 'process', name: 'key file ~/.config/aiperf/litellm.key', state: 'running', detail: 'present (existence only)' },
      ],
      problems: [
        {
          severity: 'warn',
          code: 'aiperf.engine.shared',
          message: 'The default target alias shares the Workstation card with another engine; a profile run will also slow chat users. (mock)',
          fix: 'scripts/use-model.sh status',
        },
      ],
      checked_at,
    };
  if (id === 'guardrails')
    return {
      system: id,
      health: 'up',
      version: '0.21.0-mock',
      summary: 'Serving 1 rail config (lab-input-output) on 127.0.0.1:8806, no egress',
      endpoints: [
        { name: 'server', url: 'http://127.0.0.1:8806', role: 'api', auth: 'bearer' },
        { name: 'health', url: 'http://127.0.0.1:8806/v1/rails/configs', role: 'health', auth: 'none' },
      ],
      components: [
        { kind: 'container', name: 'ai-lab-guardrails', state: 'running', image: 'ai-lab/nemo-guardrails:0.21.0-mock', detail: 'healthy, up 2 d' },
        { kind: 'http', name: 'rails configs', state: 'running', detail: 'lab-input-output loaded' },
        { kind: 'http', name: 'router http://127.0.0.1:4000', state: 'running', detail: '200 (self-check model alias)' },
      ],
      problems: [],
      checked_at,
    };
  if (id === 'modelopt')
    return {
      system: id,
      health: 'up',
      version: '0.47.0-mock',
      summary: '3 recipes · last job: reap-glm53 (reap · succeeded · 47.0 min · 71.4 GiB · 31.2% of the base) (mock)',
      endpoints: [],
      components: [
        { kind: 'process', name: 'modelopt-local', state: 'running', detail: 'Model Optimizer 0.47.0-mock (offline: the model store is a read-only mount, no egress)' },
        { kind: 'process', name: 'model store (read-only)', state: 'running', detail: 'the pinned base checkpoint it reads' },
      ],
      problems: [
        { severity: 'warn', code: 'modelopt.last_job_failed', message: 'The last job failed: base model not in the offline cache. (mock)', fix: "the job report has the wrapper's own error text" },
      ],
      checked_at,
    };
  if (id === 'flare') {
    // The shape of the real adapter (flare-local status-json): one component per target, sm120 and sm121.
    const T = (t, chip, fam, hosts, state, detail) => ({
      kind: 'container',
      name: `flare @ ${t}`,
      state,
      image: state === 'absent' && t !== 'local' ? null : 'lab/flare:902c88e',
      detail: `${chip} ${fam} (${hosts}), ${detail}`,
    });
    return {
      system: id,
      health: 'up',
      version: '902c88e',
      summary: 'serving on spark-all (sm121): glm-5.3-flash (mock)',
      endpoints: [{ name: 'spark-all API', url: 'http://192.0.2.10:8035/v1', role: 'api', auth: 'bearer' }],
      components: [
        {
          kind: 'process',
          name: 'flare-local',
          state: 'running',
          image: null,
          detail: 'the lab launcher (inference/flare/flare.sh): checks the GPUs, starts and stops Flare, reaches remote hosts over ssh',
        },
        T('local', 'sm120', 'gb202', 'local', 'absent', 'modes tp1 tp2 · container absent · image lab/flare:902c88e loaded · 1 package(s) · GPU busy: GPU 0 holds 81611 MiB (> 3072)'),
        T('gpu-box', 'sm120', 'gb202', 'gpu-box', 'absent', 'modes tp1 · container absent · image not loaded · 1 package(s) · GPU free'),
        ...['spark-a', 'spark-b', 'spark-c', 'spark-d'].map((h) =>
          T(h, 'sm121', 'gb10', h, 'absent', 'modes tp1 · container absent · image lab/flare:902c88e loaded · 1 package(s) · GPU busy: 1 CUDA process(es) on the GPU'),
        ),
        T(
          'spark-all',
          'sm121',
          'gb10',
          'spark-a spark-b spark-c spark-d',
          'running',
          'modes tp4 · serving glm-5.3-flash, context 524288 · image lab/flare:902c88e loaded · 1 package(s) · GPU busy: 1 CUDA process(es) on the GPU',
        ),
      ],
      problems: [],
      checked_at,
    };
  }
  if (id === 'gym')
    return {
      system: id,
      health: 'up',
      version: '0.7.0',
      summary: 'image built · key [set] · broker stopped · network internal · last smoke: glm-5.3-flash-fp4-local · succeeded · 5 rollouts · mean reward 0.80 · pass@1 80% · 2.4 min (mock)',
      endpoints: [],
      components: [
        {
          kind: 'process',
          name: 'gym-local',
          state: 'running',
          image: 'lab/nemo-gym:0.7.0',
          detail:
            'the lab wrapper (NeMo Gym 0.7.0, pinned commit); runs each Gym command in a client container (lab/nemo-gym:0.7.0 built) on the internal network ai-lab-nemo-gym (network internal) (mock)',
        },
        {
          kind: 'container',
          name: 'nemo-gym-router-broker',
          state: 'stopped',
          detail: "Exited (0) 2 hours ago: the client's only way out (holds the wrapper's router key; the client never does). Started on demand, so 'stopped' is normal.",
        },
      ],
      problems: [],
      checked_at,
    };
  return {
    system: 'evaluator',
    health: 'up',
    version: '1.3.0-mock',
    summary: '3 suites · 5 benchmarks cached offline · last run: smoke (gsm8k 0.87)',
    endpoints: [{ name: 'router (model under test)', url: 'http://127.0.0.1:4000/v1', role: 'api', auth: 'bearer' }],
    components: [
      { kind: 'process', name: 'evaluator-local', state: 'running', detail: 'nel 1.3.0-mock, HF_*_OFFLINE=1' },
      { kind: 'http', name: 'router http://127.0.0.1:4000', state: 'running', detail: '200 on /health/liveliness' },
      { kind: 'process', name: 'dataset cache mmlu_pro', state: 'absent', detail: 'not prefetched' },
    ],
    problems: [
      {
        severity: 'warn',
        code: 'evaluator.dataset.missing',
        message: 'Benchmark mmlu_pro is in suite "full" but its dataset is not prefetched; a run would fail offline. (mock)',
        fix: 'clients/nemo-evaluator/evaluator-local prefetch mmlu_pro   # the only step with egress; run it by hand',
      },
      { severity: 'info', code: 'evaluator.judge.none', message: 'Only benchmarks that need no judge model and no Docker sandbox are offered.' },
    ],
    checked_at,
  };
}
export const sevenHealth = (id, flags) => sevenStatus(id, 0, flags).health;

// ------------------------------------------------------------------ metrics (keys and labels as captured from the real adapters; VALUES are mock)
export function sevenMetrics(id, ts, wob) {
  const P = (key, value, unit = '', labels = {}) => ({ key, value, unit, labels });
  const gpus = [
    ['0', 'NVIDIA RTX PRO 6000 Blackwell Max-Q'],
    ['1', 'NVIDIA RTX PRO 6000 Blackwell Workstation'],
  ];
  if (id === 'skillevaluator') return [P('se.skills', 66), P('se.runs', 12), P('se.last_quality_score', 87.5), P('se.last_judge_score', 82.0), P('se.last_skill_lift', 0.125), P('se.live_running', 0)];
  if (id === 'openshell')
    return [
      P('os.gateway_up', 1),
      P('os.network_internal', 1),
      P('os.gateway_uptime_s', 86400 * 2 + 5400, 's'),
      P('os.gateway_restarts', 0),
      P('os.sandboxes', 0),
      ...['Provisioning', 'Ready', 'Error', 'Deleting', 'Other'].map((phase) => P('os.sandboxes_by_phase', 0, '1', { phase })),
      P('os.providers', 1),
    ];
  if (id === 'dcgm')
    return [
      ...gpus.flatMap(([g, model], i) => {
        const L = { gpu: g, model };
        return [
          P('dcgm.gpu_temp_c', wob(i ? 54 : 63, 3), 'C', L),
          P('dcgm.power_w', wob(i ? 190 : 285, 25), 'W', L),
          P('dcgm.gpu_util', Math.min(1, wob(i ? 0.38 : 0.71, 0.1)), 'ratio', L),
          P('dcgm.mem_util', i ? 0.64 : 0.63, 'ratio', L),
          P('dcgm.fb_used_b', (i ? 61200 : 60800) * 1048576, 'B', L),
          P('dcgm.fb_free_b', (i ? 37000 : 37400) * 1048576, 'B', L),
          P('dcgm.sm_clock_mhz', i ? 1980 : 2100, 'MHz', L),
          P('dcgm.pcie_replay_total', 0, '1', L),
          P('dcgm.gr_active', i ? 0.35 : 0.7, 'ratio', L),
          P('dcgm.tensor_active', i ? 0.2 : 0.5, 'ratio', L),
          P('dcgm.dram_active', i ? 0.3 : 0.45, 'ratio', L),
          P('dcgm.pcie_tx_bps', 2.1e8, 'B/s', L),
          P('dcgm.pcie_rx_bps', 1.4e8, 'B/s', L),
        ];
      }),
      ...[
        ['node1', 'dcgm'],
        ['node5', 'dcgm-aitop100'],
      ].flatMap(([host, job], i) => {
        const L = { host, job };
        return [P('dcgm.host_up', i ? 0 : 1, '1', L), P('dcgm.fleet_gpus', i ? 0 : 2, '1', L), P('dcgm.fleet_temp_max_c', 58, 'C', L), P('dcgm.fleet_power_w', 410, 'W', L)];
      }),
    ];
  if (id === 'aiperf')
    return [
      P('ap.ttft_ms', wob(210, 25), 'ms', { q: 'p50' }),
      P('ap.ttft_ms', wob(540, 70), 'ms', { q: 'p99' }),
      P('ap.itl_ms', wob(18, 2), 'ms', { q: 'p50' }),
      P('ap.itl_ms', wob(31, 4), 'ms', { q: 'p99' }),
      P('ap.latency_ms', 2400, 'ms', { q: 'p50' }),
      P('ap.latency_ms', 4100, 'ms', { q: 'p99' }),
      P('ap.output_tps', 1840.5, 'tok/s'),
      P('ap.request_tps', 12.4, 'req/s'),
      P('ap.error_rate', 0.005, 'ratio'),
      P('ap.runs', 7),
      P('ap.runs_by_verdict', 6, '1', { verdict: 'succeeded' }),
      P('ap.runs_by_verdict', 1, '1', { verdict: 'failed' }),
      P('ap.live_running', 0),
    ];
  if (id === 'guardrails') return [P('gr.up', 1), P('gr.configs', 1)];
  if (id === 'modelopt')
    return [
      P('mo.jobs', 4),
      P('mo.live_running', 0),
      P('mo.jobs_by_verdict', 3, '1', { verdict: 'succeeded' }),
      P('mo.jobs_by_verdict', 1, '1', { verdict: 'failed' }),
      P('mo.output_bytes', 71.4 * 1073741824, 'B'),
      P('mo.compression_ratio', 0.312, 'ratio'),
      P('mo.duration_s', 47 * 60, 's'),
    ];
  if (id === 'flare') {
    const T = { target: 'spark-all' };
    return [
      P('flare.targets_serving', 1),
      ...['local', 'gpu-box', 'spark-a', 'spark-b', 'spark-c', 'spark-d', 'spark-all'].map((target) => P('flare.packages', 1, '1', { target })),
      P('flare.generated_total', wob(182400, 4000), 'tokens', T),
      P('flare.prefill_total', 912000, 'tokens', T),
      P('flare.active_requests', 3, '1', T),
      P('flare.pending_requests', 0, '1', T),
      P('flare.requests_completed_total', 412, '1', T),
      P('flare.requests_failed_total', 1, '1', T),
      P('flare.cache_reused_total', 240000, 'tokens', T),
      P('flare.draft_acceptance', 0.71, 'ratio', T),
      P('flare.max_model_len', 524288, 'tokens', T),
    ];
  }
  if (id === 'gym')
    return [
      P('gym.runs', 3),
      P('gym.live_running', 0),
      ...['running', 'succeeded', 'failed', 'error', 'interrupted'].map((verdict) => P('gym.runs_by_verdict', verdict === 'succeeded' ? 2 : verdict === 'failed' ? 1 : 0, '1', { verdict })),
      P('gym.mean_reward', 0.8, '1'),
      P('gym.rollouts', 5, '1'),
      P('gym.pass_at_1', 0.8, 'ratio'),
      P('gym.duration_s', 144, 's'),
    ];
  return [
    P('ev.suites', 3),
    P('ev.runs', 4),
    P('ev.live_running', 0),
    P('ev.runs_by_verdict', 3, '1', { verdict: 'succeeded' }),
    P('ev.runs_by_verdict', 1, '1', { verdict: 'failed' }),
    P('ev.score', 0.87, 'ratio', { bench: 'gsm8k', model: 'qwen3.8-flash-next' }),
    P('ev.score', 0.74, 'ratio', { bench: 'gsm8k', model: 'nemotron-lightning' }),
    P('ev.score', 0.61, 'ratio', { bench: 'ifeval', model: 'qwen3.8-flash-next' }),
    P('ev.score', 0.58, 'ratio', { bench: 'ifeval', model: 'nemotron-lightning' }),
  ];
}
// Plausible per-key history so every chart has a shape. Returns undefined for keys it does not know.
export function sevenHistory(key, ts, rnd) {
  const g = /gpu=1/.test(key) ? 1 : 0;
  if (/^dcgm\.gpu_util/.test(key)) return Math.min(1, Math.max(0, (g ? 0.38 : 0.71) + Math.sin(ts / 160 + g) * 0.14 + rnd() * 0.05));
  if (/^dcgm\.gpu_temp_c/.test(key)) return (g ? 54 : 63) + Math.sin(ts / 400 + g) * 3 + rnd();
  if (/^dcgm\.power_w/.test(key)) return (g ? 190 : 285) + Math.sin(ts / 150 + g) * 30 + rnd() * 8;
  if (/^dcgm\.fb_used_b/.test(key)) return ((g ? 61200 : 60800) + Math.round(rnd() * 40)) * 1048576;
  if (/^ap\.ttft_ms\{q=p99/.test(key)) return 540 + Math.sin(ts / 90) * 70 + rnd() * 30;
  if (/^ap\.ttft_ms/.test(key)) return 210 + Math.sin(ts / 110) * 25 + rnd() * 12;
  if (/^ap\.itl_ms\{q=p99/.test(key)) return 31 + Math.sin(ts / 70) * 4 + rnd() * 2;
  if (/^ap\.itl_ms/.test(key)) return 18 + Math.sin(ts / 80) * 2 + rnd();
  if (/^ap\.(output|request)_tps/.test(key)) return (/output/.test(key) ? 1840 : 12.4) * (0.92 + Math.abs(Math.sin(ts / 200)) * 0.12) + rnd();
  if (/^mo\.output_bytes/.test(key)) return 71.4 * 1073741824 * (0.98 + Math.abs(Math.sin(ts / 300)) * 0.04);
  if (/^mo\.duration_s/.test(key)) return 47 * 60 * (0.95 + Math.abs(Math.sin(ts / 240)) * 0.1);
  if (/^gym\.(mean_reward|pass_at_1)/.test(key)) return 0.7 + Math.abs(Math.sin(ts / 500)) * 0.2;
  if (/^gym\.duration_s/.test(key)) return 144 * (0.9 + Math.abs(Math.sin(ts / 260)) * 0.2);
  if (/^flare\.(active_requests)/.test(key)) return Math.max(0, Math.round(2 + Math.sin(ts / 120) * 2));
  if (/^flare\.draft_acceptance/.test(key)) return 0.66 + Math.abs(Math.sin(ts / 300)) * 0.1;
  if (/^flare\.generated_total/.test(key)) return 150000 + (ts % 40000);
  if (/^(os|ev|gr|se|gym|flare)\./.test(key)) return /uptime/.test(key) ? ts % 1e6 : 0;
  return undefined;
}

// ------------------------------------------------------------------ config files
const DCGM_COUNTERS = `# DCGM exporter counters (mock copy of observability/dcgm/counters.csv)
# Format: DCGM field, Prometheus type, help message
DCGM_FI_DEV_GPU_UTIL,      gauge, GPU utilization (in %).
DCGM_FI_DEV_FB_USED,       gauge, Framebuffer memory used (in MiB).
DCGM_FI_DEV_GPU_TEMP,      gauge, GPU temperature (in C).
DCGM_FI_DEV_POWER_USAGE,   gauge, Power draw (in W).
DCGM_FI_DEV_XID_ERRORS,    gauge, Value of the last XID error encountered.
`;
const AIPERF_PROFILES = `# AIPerf load presets (mock copy of clients/aiperf/profiles.yaml)
# Schema: profiles.<name> -> model, endpoint_type (chat|completions), streaming, concurrency 1..64,
# requests 1..2000, isl 1..32768, osl 1..4096, warmup_requests, timeout_s, seed.
version: 1
profiles:
  chat-short:
    description: 200 short chats at concurrency 8 - the everyday load
    model: qwen3.8-flash-next
    endpoint_type: chat
    streaming: true
    concurrency: 8        # requests kept in flight
    requests: 200
    isl: 256
    osl: 128
    warmup_requests: 2
  chat-long:
    description: long prompts, 60 requests at concurrency 4
    model: qwen3.8-flash-next
    endpoint_type: chat
    streaming: true
    concurrency: 4
    requests: 60
    isl: 8192
    osl: 256
    timeout_s: 300
  lightning-burst:
    description: a burst of 400 small requests at concurrency 16 on Lightning
    model: nemotron-lightning
    endpoint_type: chat
    streaming: true
    concurrency: 16
    requests: 400
    isl: 512
    osl: 64
`;
const GR_CONFIG = `# Rails config lab_default (mock copy of security/nemo-guardrails/config/lab_default/config.yml)
models:
  - type: main
    engine: openai
    model: nemotron-lightning
    parameters:
      base_url: http://litellm:4000/v1
      api_key_env: GUARDRAILS_ROUTER_KEY   # an env NAME, never a value
rails:
  input:
    flows:
      - self check input
  output:
    flows:
      - self check output
`;
const GR_PROMPTS = `# prompts.yml for lab_default (mock)
prompts:
  - task: self_check_input
    content: |
      Your task is to check if the user message complies with the policy.
      User message: "{{ user_input }}"
      Question: Should the user message be blocked (Yes or No)?
      Answer:
`;
const EV_SUITES = `# NeMo Evaluator suites (mock copy of clients/nemo-evaluator/suites.yaml)
# Every suite names a router model and 1-8 benchmarks; max_problems is required (a bounded, shuffled sample).
version: 1
suites:
  smoke:
    description: 50 GSM8K problems - proves the whole path in seconds
    model: qwen3.8-flash-next
    benchmarks:
      - bench: gsm8k
        max_problems: 50
        max_tokens: 1024
  quick:
    description: GSM8K + MMLU, 200 each
    model: qwen3.8-flash-next
    benchmarks:
      - bench: gsm8k
        max_problems: 200
      - bench: mmlu
        max_problems: 200
  full:
    description: three benchmarks at 500 problems, twice each
    model: qwen3.8-flash-next
    benchmarks:
      - bench: gsm8k
        max_problems: 500
        repeats: 2
      - bench: mmlu
        max_problems: 500
      - bench: mmlu_pro
        max_problems: 500
        temperature: 0.0
`;
const OS_GATEWAY = `# OpenShell gateway.toml (mock copy, same shape as the runner's config/gateway.toml). Read-only in the console.
[openshell]
version = 1

[openshell.gateway]
compute_drivers = ["docker"]
host_gateway_ip = "172.16.0.2"

[openshell.drivers.docker]
network_name = "openshell-docker"
supervisor_image = "ghcr.io/nvidia/openshell/supervisor@sha256:0000000000000000000000000000000000000000000000000000000000000000"
image_pull_policy = "Never"
`;
const OS_POLICY = `# OpenShell sandbox policy (mock copy). Read-only in the console.
version: 1
filesystem_policy:
  include_workdir: true
  read_only:
    - /opt/dd-config
    - /usr
  read_write:
    - /sandbox
network_policies:
  lab_router:
    endpoints:
      - host: 172.16.0.5
        port: 4000
        protocol: rest
        enforcement: enforce
        allowed_ips:
          - 172.16.0.5/32
        rules:
          - allow:
              method: "*"
              path: "/v1/**"
    binaries:
      - path: /usr/local/bin/python3.13
`;
const OS_PROVIDER = `# OpenShell provider profile (mock copy). Read-only in the console.
id: lab-litellm-dd
display_name: Lab LiteLLM Router (Data Designer)
category: inference
inference_capable: true
credentials:
  - name: litellm_dd_key
    env_vars: [LITELLM_DD_KEY]
    required: true
    header_name: authorization
endpoints:
  - host: 172.16.0.5
    port: 4000
    path: /v1/**
    protocol: rest
    enforcement: enforce
binaries:
  - /usr/local/bin/python3.13
`;
const OS_COMPOSE = `# OpenShell gateway compose (mock copy). Read-only in the console.
name: ai-lab-dd
services:
  gateway:
    image: ghcr.io/nvidia/openshell/gateway:0.0.77-mock
    container_name: ai-lab-dd-openshell
    cap_drop: [ALL]
    security_opt: ["no-new-privileges:true"]
    networks:
      openshell-docker:
        ipv4_address: 172.16.0.2
    environment:
      OPENSHELL_COMPUTE_DRIVER: docker
      OPENSHELL_ENABLE_MTLS_AUTH: "true"
      OPENSHELL_TELEMETRY_ENABLED: "false"
networks:
  openshell-docker:
    external: true
`;
const OS_WHY = 'A web console must never loosen a sandbox trust policy (ADR 0007). Edit it by hand in the repo, then restart the gateway; Prove policy checks it without changing anything.';

const MO_RECIPES = `# Model Optimizer compression recipes (mock copy of clients/modelopt/recipes.yaml)
version: 1
recipes:
  reap-glm53:
    description: "REAP expert pruning (25%) of the GLM-5.3-Flash checkpoint"
    method: reap
    base_model: zai-org/GLM-5.3-Flash
    sparsity: 0.25
    output: /runs/reap-glm53
  quantize-glm53-nvfp4:
    description: "NVFP4 quantization of the GLM-5.3-Flash checkpoint"
    method: quantize
    base_model: zai-org/GLM-5.3-Flash
    preset: NVFP4
    output: /runs/quantize-glm53-nvfp4
  minitron-qwen38:
    description: "Minitron layer pruning of the Qwen3.8 checkpoint"
    method: minitron
    base_model: Qwen/Qwen3.8-27B
    config: minitron.yaml
    output: /runs/minitron-qwen38
`;
const ed = (o) => ({ authoritative: true, validators: [], apply_effect: '', ...o });
const GR_VAL = ['guardrails-local validate (loads the candidate in a --network none container)'];
const GR_EFFECT = 'Writes the file, restarts the guardrails server and checks that the config is served; rolled back if it is not.';
export const SEVEN_SPECS = {
  gym: [], // no editable file: the Gym environment config belongs to the lab wrapper
  flare: [
    ['compose', 'docker-compose.yml', 'yaml', 'The local serving container: image, ports, mounts, the `flare run` command line.'],
    ['compose-tp1', 'docker-compose.tp1.yml', 'yaml', 'The local tp1 override: one GPU.'],
    ['compose-tp2', 'docker-compose.tp2.yml', 'yaml', 'The local tp2 override: both GPUs.'],
    ['ignition', 'ignition-config.toml', 'toml', 'The seed config of Ignition, the package preparation tool (cache and toolkit paths).'],
  ].map(([id, label, format, description]) =>
    ed({
      id,
      label,
      format,
      description,
      display_path: `inference/flare/${label}`,
      editable: false,
      authoritative: false,
      validators: [`${format.toUpperCase()} parse`],
      apply_effect: 'read-only: nothing to apply',
      why_readonly:
        "Flare is started and stopped with flare.sh (this page's actions run it as flare-local), which checks that the GPUs are free first; a bare compose start skips those checks. These files change in git, not from the console.",
    }),
  ),
  openshell: [
    ed({
      id: 'gateway',
      label: 'gateway.toml',
      description: 'Gateway listen address, TLS and runtime network.',
      display_path: 'clients/data-designer/runner/gateway.toml',
      format: 'toml',
      editable: false,
      why_readonly: OS_WHY,
    }),
    ed({
      id: 'policy',
      label: 'policy.yaml',
      description: 'Default-deny sandbox policy: filesystem and network allowances.',
      display_path: 'clients/data-designer/runner/policy.yaml',
      format: 'yaml',
      editable: false,
      why_readonly: OS_WHY,
    }),
    ed({
      id: 'provider-profile',
      label: 'provider profile',
      description: 'The router provider the sandbox may call; the credential is held by the gateway.',
      display_path: 'clients/data-designer/runner/provider.yaml',
      format: 'yaml',
      editable: false,
      why_readonly: OS_WHY,
    }),
    ed({
      id: 'compose',
      label: 'docker-compose.yml',
      description: 'How the gateway container and the internal sandbox network are started.',
      display_path: 'clients/data-designer/runner/docker-compose.yml',
      format: 'yaml',
      editable: false,
      why_readonly: OS_WHY,
    }),
  ],
  dcgm: [
    ed({
      id: 'counters',
      label: 'counters.csv',
      description: 'Which DCGM fields the exporter publishes (field name, Prometheus type, help).',
      display_path: 'observability/dcgm/counters.csv',
      format: 'text',
      editable: true,
      validators: ['field-name and type check against the exporter image (proxy)'],
      authoritative: false,
      apply_effect: 'Writes the file and restarts the exporter container; rolled back automatically if /metrics stops answering or the configured families vanish.',
    }),
  ],
  aiperf: [
    ed({
      id: 'profiles',
      label: 'profiles.yaml',
      description: 'Named load presets for aiperf-local profile.',
      display_path: 'clients/aiperf/profiles.yaml',
      format: 'yaml',
      editable: true,
      validators: ['aiperf profile schema check'],
      apply_effect: 'Writes the file. Nothing is restarted; the next run reads it.',
    }),
  ],
  guardrails: [
    ed({
      id: 'lab-input-output--config.yml',
      label: 'lab-input-output / config.yml',
      description: 'Models and rail flows for the lab-input-output rails config.',
      display_path: 'security/nemo-guardrails/config/lab-input-output/config.yml',
      format: 'yaml',
      editable: true,
      validators: GR_VAL,
      apply_effect: GR_EFFECT,
    }),
    ed({
      id: 'lab-input-output--prompts.yml',
      label: 'lab-input-output / prompts.yml',
      description: 'Self-check prompt templates used by the rails.',
      display_path: 'security/nemo-guardrails/config/lab-input-output/prompts.yml',
      format: 'yaml',
      editable: true,
      validators: GR_VAL,
      apply_effect: GR_EFFECT,
    }),
  ],
  evaluator: [
    ed({
      id: 'suites.yaml',
      label: 'suites.yaml',
      description: 'Named benchmark suites for evaluator-local run.',
      display_path: 'clients/nemo-evaluator/suites.yaml',
      format: 'yaml',
      editable: true,
      validators: ['evaluator suites schema check'],
      apply_effect: 'Writes the file. Nothing is restarted; the next run reads it.',
    }),
  ],
  modelopt: [
    ed({
      id: 'recipes',
      label: 'recipes.yaml',
      description: 'Named compression recipes: the method (reap expert pruning, quantize or minitron), the base model it reads and the checkpoint it writes.',
      display_path: 'clients/modelopt/recipes.yaml',
      format: 'yaml',
      editable: true,
      validators: ['nemo-console modelopt recipes schema'],
      apply_effect: 'Writes the file. Nothing is restarted; the next job reads it.',
    }),
  ],
};
const FLARE_COMPOSE = `# NVIDIA Flare serving container (mock copy, trimmed from the lab's inference/flare/docker-compose.yml). Read-only in the console.
name: ai-lab-flare
services:
  flare:
    image: lab/flare:902c88e
    container_name: ai-lab-flare
    init: false
    user: "0:0"
    networks:
      ai-lab:
        aliases: [flare]
    ports:
      - "127.0.0.1:8035:8000"
    restart: "no"
    stop_grace_period: 60s
    security_opt:
      - seccomp=./seccomp-io-uring.json
    ulimits:
      memlock: { soft: 8192, hard: 8192 }
    volumes:
      - /srv/flare/packages:/packages:ro
      - /srv/flare/trust:/trust
    environment:
      HOME: /trust
    command:
      - run
      - /packages/UNSET-start-with-flare.sh
      - --offline
      - --trust-store
      - /trust/trust.toml
      - --gpus
      - 0,1
      - --host
      - 0.0.0.0
      - --port
      - "8000"
networks:
  ai-lab:
    external: true
`;
const FLARE_TP = (ids) => `# Flare GPU override (mock copy). Read-only in the console.
services:
  flare:
    deploy:
      resources:
        reservations:
          devices:
            - driver: nvidia
              device_ids: [${ids}]
              capabilities: [compute, utility]
`;
const FLARE_IGNITION = `# Ignition seed config (mock copy of inference/flare/ignition-config.toml). Read-only in the console.
[paths]
hub_cache = "/srv/hf/hub"
kernel_cache = "/work/kernels"
package_cache = "/work/package-cache"
cuda = "/usr/local/cuda"
`;
export const SEVEN_FILES = {
  'flare/compose': FLARE_COMPOSE,
  'flare/compose-tp1': FLARE_TP("'1'"),
  'flare/compose-tp2': FLARE_TP("'0', '1'"),
  'flare/ignition': FLARE_IGNITION,
  'openshell/gateway': OS_GATEWAY,
  'openshell/policy': OS_POLICY,
  'openshell/provider-profile': OS_PROVIDER,
  'openshell/compose': OS_COMPOSE,
  'dcgm/counters': DCGM_COUNTERS,
  'aiperf/profiles': AIPERF_PROFILES,
  'guardrails/lab-input-output--config.yml': GR_CONFIG,
  'guardrails/lab-input-output--prompts.yml': GR_PROMPTS,
  'evaluator/suites.yaml': EV_SUITES,
  'modelopt/recipes': MO_RECIPES,
};
/** Which of the five restart something on apply (the rest only write a file). */
export const SEVEN_RESTARTS = new Set(['dcgm', 'guardrails']);

/** Mock validator for the five systems' editable files. Returns the ValidationResult shape. */
export function sevenValidate(system, file, content) {
  const spec = SEVEN_SPECS[system].find((s) => s.id === file);
  const diags = [];
  const tab = content.split('\n').findIndex((l) => /^\t/.test(l));
  if (system === 'dcgm') {
    const known = /^DCGM_FI_[A-Z0-9_]+$/;
    content.split('\n').forEach((l, i) => {
      if (!l.trim() || l.trim().startsWith('#')) return;
      const f = l.split(',').map((x) => x.trim());
      if (f.length < 3) diags.push({ severity: 'error', message: 'expected "field, type, help"', line: i + 1 });
      else if (!known.test(f[0])) diags.push({ severity: 'error', message: `unknown DCGM field ${f[0]}`, line: i + 1 });
      else if (!['gauge', 'counter', 'label'].includes(f[1])) diags.push({ severity: 'error', message: `type must be gauge, counter or label, got ${f[1]}`, line: i + 1 });
    });
  } else if (tab >= 0) diags.push({ severity: 'error', message: 'YAML does not allow tab indentation', line: tab + 1 });
  if (system === 'aiperf' && !/^profiles:/m.test(content)) diags.push({ severity: 'error', message: 'top-level "profiles:" is required', line: 1 });
  if (system === 'evaluator' && !/^suites:/m.test(content)) diags.push({ severity: 'error', message: 'top-level "suites:" is required', line: 1 });
  if (system === 'modelopt' && !/^recipes:/m.test(content)) diags.push({ severity: 'error', message: 'top-level "recipes:" is required', line: 1 });
  if (system === 'modelopt' && !/^version: 1$/m.test(content)) diags.push({ severity: 'error', message: "'version' must be 1", line: 1 });
  if (system === 'guardrails' && file === 'lab-input-output--config.yml' && !/^models:/m.test(content)) diags.push({ severity: 'error', message: 'a rails config needs a "models:" section', line: 1 });
  return {
    ok: !diags.some((d) => d.severity === 'error'),
    validator: spec.validators[0],
    authoritative: spec.authoritative,
    diagnostics: diags,
    note: spec.authoritative ? null : 'This check is a proxy (field names and types only). The exporter image is the real authority; only a restart with the file proves it.',
  };
}

// ------------------------------------------------------------------ actions (ids, params and danger as captured from the real adapters)
const P = (key, label, type, o = {}) => ({ key, label, type, default: null, requires_restart: false, help: '', ...o });
const NAME = P('name', 'Sandbox name', 'string', { help: 'A name from List sandboxes.' });
const LINES = P('lines', 'Lines', 'int', { default: 100, min: 1, max: 1000, help: 'How many log lines to show.' });
const GPU = P('gpu', 'GPU index', 'int', { default: 0, min: 0, max: 7, help: 'Each card is diagnosed on its own: DCGM refuses a mixed-model group.' });
const RUN = P('run_id', 'Run', 'string', { help: 'A run id; empty = the newest run.' });
const TARGET = P('target', 'Target', 'enum', {
  choices: ['local', 'gpu-box', 'spark-a', 'spark-b', 'spark-c', 'spark-d', 'spark-all'],
  default: 'local',
  help: 'Where Flare runs, as the launcher defines it: each target has a chip (sm120 takes gb202 packages, sm121 gb10) and the tensor-parallel sizes it can run.',
});
const PKG = P('package', 'Package', 'enum', {
  choices: ['glm53-gb10-tp4.safetensors', 'qwen38-gb202-tp2.safetensors'],
  default: 'glm53-gb10-tp4.safetensors',
  help: "A prepared package file of the target's GPU family.",
});
const MODE = P('mode', 'Tensor-parallel size', 'enum', { choices: ['auto', 'tp1', 'tp2', 'tp4'], default: 'auto', help: 'Must match how the package was exported.' });
export const SEVEN_ACTIONS = {
  flare: [
    {
      id: 'targets',
      label: 'Show every target',
      danger: 'safe',
      params: [],
      description: 'Every target with its chip (sm120 / sm121), package family, hosts, container, image, packages and whether its GPU is free. Read-only.',
    },
    {
      id: 'preflight',
      label: 'Check a start',
      danger: 'safe',
      params: [TARGET, PKG, MODE],
      description: 'flare-local preflight <target> <package> [mode]: package, image, free GPU(s). Changes nothing.',
    },
    {
      id: 'up',
      label: 'Start Flare',
      danger: 'disruptive',
      params: [TARGET, PKG, MODE],
      description: 'flare-local up <target> <package> [mode] in the background: refuses unless every GPU of the target is free (nothing is stopped for you).',
    },
    { id: 'down', label: 'Stop Flare', danger: 'disruptive', params: [TARGET], description: 'flare-local down <target>: stops Flare there and frees the GPU(s).' },
    { id: 'probe', label: 'Probe', danger: 'safe', params: [TARGET], description: 'flare-local probe <target>: one real completion.' },
    {
      id: 'log',
      label: 'Show the log',
      danger: 'safe',
      params: [TARGET, P('lines', 'Lines', 'int', { default: 80, min: 1, max: 500 })],
      description: "The newest lines of Flare's container log on each host of the target.",
    },
    {
      id: 'stage',
      label: 'Stage a remote target',
      danger: 'disruptive',
      params: [TARGET],
      description: "flare-local stage <target>: loads the image from the console host's share and seeds ~/flare (no GPU).",
    },
    { id: 'last_start', label: 'Show the last start', danger: 'safe', params: [], description: 'The output of the last background start.' },
    { id: 'prepare_help', label: 'How to prepare a package', danger: 'safe', params: [], description: 'flare-local prepare-help. Prints only.' },
  ],
  gym: [
    {
      id: 'smoke',
      label: 'Run a smoke rollout',
      danger: 'disruptive',
      description:
        'gym-local smoke: the mcqa environment x simple_agent rolls out N tasks through the router and scores each one. Spends router tokens on a shared engine; one run at a time, in the background.',
      params: [
        P('model', 'Model alias', 'string', { default: 'glm-5.3-flash-fp4-local', help: "A LOCAL router alias the wrapper's key may use." }),
        P('limit', 'Tasks', 'int', { default: 5, min: 1, max: 20, help: 'How many mcqa tasks to roll out (1-20).' }),
      ],
    },
    { id: 'verify', label: 'Check the containment', danger: 'safe', description: 'gym-local verify: the client network ai-lab-nemo-gym must be internal. Read-only.', params: [] },
    { id: 'report', label: 'Show a run', danger: 'safe', params: [P('run_id', 'Run', 'string', { help: 'A run id from the runs list; empty = the newest run.' })] },
  ],
  skillevaluator: [
    { id: 'list_skills', label: 'List skills', danger: 'safe', params: [], description: 'The skills the evaluator can see.' },
    {
      id: 'evaluate',
      label: 'Evaluate a skill',
      danger: 'disruptive',
      description: 'Runs Tier 1 on one skill (may call the router judge).',
      params: [P('skill', 'Skill', 'string', { help: 'A skill folder name.' })],
    },
  ],
  openshell: [
    { id: 'doctor', label: 'Doctor', danger: 'safe', params: [], description: 'openshell doctor: gateway, network and version checks. Changes nothing.' },
    { id: 'list_sandboxes', label: 'List sandboxes', danger: 'safe', params: [], description: 'openshell sandbox list.' },
    { id: 'sandbox_get', label: 'Show a sandbox', danger: 'safe', params: [NAME], description: 'openshell sandbox get: phase and details of one sandbox.' },
    { id: 'sandbox_logs', label: 'Sandbox logs', danger: 'safe', params: [NAME, LINES], description: 'The last lines of one sandbox log.' },
    { id: 'gateway_logs', label: 'Gateway logs', danger: 'safe', params: [LINES], description: 'The last lines of the gateway container log.' },
    {
      id: 'prove_policy',
      label: 'Prove policy',
      danger: 'safe',
      description: 'openshell policy prove: checks the sandbox policy against its claims. Read-only; the policy file itself is never edited here.',
      params: [P('scopes', 'Scopes', 'string', { help: 'Optional, comma separated.' }), P('compact', 'Compact output', 'bool', { default: true })],
    },
    { id: 'gateway_up', label: 'Gateway up', danger: 'disruptive', params: [], description: 'Starts the gateway container (data-designer-local does this on demand).' },
    {
      id: 'gateway_stop',
      label: 'Gateway stop',
      danger: 'disruptive',
      description: 'Stops the gateway; running sandboxes are cut off.',
      params: [P('force', 'Force', 'bool', { default: false, help: 'Stop even if sandboxes exist.' })],
    },
    { id: 'delete_sandbox', label: 'Delete a sandbox', danger: 'destructive', params: [NAME], description: 'openshell sandbox delete: removes one sandbox and everything inside it.' },
  ],
  dcgm: [
    { id: 'diag', label: 'Diagnostics (level 1)', danger: 'disruptive', params: [GPU], description: 'dcgmi diag -r 1 in a throwaway container (--network none) on one GPU. Seconds; light load.' },
    { id: 'diag_level2', label: 'Diagnostics (level 2)', danger: 'disruptive', params: [GPU], description: 'dcgmi diag -r 2: loads the GPU for several minutes, so engines on that card slow down.' },
    { id: 'discovery', label: 'Discovery', danger: 'safe', params: [], description: 'The GPUs and hosts the exporter and Prometheus report.' },
    { id: 'restart_exporter', label: 'Restart exporter', danger: 'disruptive', params: [], description: 'Recreates the dcgm-exporter container; metrics have a gap of a few seconds.' },
  ],
  aiperf: [
    {
      id: 'profile',
      label: 'Run a profile',
      danger: 'disruptive',
      description: 'aiperf-local profile: loads a live engine through the router and spends router capacity. Up to an hour. One run at a time.',
      params: [P('preset', 'Preset', 'enum', { choices: ['chat-short', 'chat-long', 'lightning-burst'], default: 'chat-short', help: 'A preset from profiles.yaml.' })],
    },
    { id: 'report', label: 'Show a run report', danger: 'safe', params: [RUN], description: 'A bounded text summary of one stored run (empty = newest).' },
  ],
  guardrails: [
    { id: 'up', label: 'Start server', danger: 'disruptive', params: [], description: 'Starts the guardrails container and waits for the configs to be served.' },
    { id: 'restart', label: 'Restart server', danger: 'disruptive', params: [], description: 'Recreates the guardrails container and checks the config is served again.' },
    { id: 'stop', label: 'Stop server', danger: 'disruptive', params: [], description: 'Stops the guardrails container; guarded chat stops until it is started again.' },
    {
      id: 'check',
      label: 'Check a config',
      danger: 'disruptive',
      description: 'Sends fixed probe prompts through one rails config and reports allowed or blocked. Calls the router model, so it spends tokens.',
      params: [P('config', 'Config', 'enum', { choices: ['lab-input-output'], default: 'lab-input-output' })],
    },
  ],
  evaluator: [
    {
      id: 'run',
      label: 'Run a suite',
      danger: 'disruptive',
      description: 'evaluator-local run: offline benchmarks against a router alias. Spends router tokens and takes minutes. One run at a time.',
      params: [P('suite', 'Suite', 'enum', { choices: ['smoke', 'quick', 'full'], default: 'smoke', help: 'A suite from suites.yaml.' })],
    },
    { id: 'report', label: 'Show a run report', danger: 'safe', params: [RUN], description: 'A bounded text summary of one stored run (empty = newest).' },
  ],
  modelopt: [
    {
      id: 'run',
      label: 'Run a compression job',
      danger: 'disruptive',
      description: 'modelopt-local run: compresses a pinned checkpoint (quantization, REAP expert pruning or Minitron) and writes a new one. Takes a GPU for hours. The job runs in the background.',
      params: [P('recipe', 'Recipe', 'enum', { choices: ['reap-glm53', 'quantize-glm53-nvfp4', 'minitron-qwen38'], default: 'reap-glm53', help: 'A recipe from recipes.yaml.' })],
    },
    { id: 'report', label: 'Show a job', danger: 'safe', params: [P('job_id', 'Job', 'string', { help: 'A job id from the jobs list; empty = the newest job.' })] },
  ],
};

export function sevenRunAction(system, id, params, R) {
  const n = String(params.name || '').replace(/[^\w-]/g, '_');
  const out = {
    'openshell/doctor': 'gateway: stopped (starts on demand)\nnetwork openshell-docker: internal\nCLI 0.9.4-mock vs gateway 0.9.3-mock: version skew\n[mock output]',
    'openshell/list_sandboxes': 'NAME                 PHASE     AGE\n(no sandboxes)',
    'openshell/gateway_logs': 'gateway listening on 127.0.0.1:8443\n[mock output]',
    'openshell/prove_policy': 'policy.yaml: default deny holds\nfilesystem: write limited to /sandbox/outputs\nnetwork: only litellm:4000 reachable\n[mock output]',
    'openshell/gateway_up': 'gateway container started\nmTLS listener on 127.0.0.1:8443',
    'openshell/gateway_stop': 'gateway container stopped',
    'dcgm/discovery': 'GPU 0  NVIDIA RTX PRO 6000 Blackwell Max-Q  96 GB\nGPU 1  NVIDIA RTX PRO 6000 Blackwell Workstation  96 GB\nhost node1 up, host node5 DOWN\n[mock output]',
    'dcgm/restart_exporter': 'container dcgm-exporter recreated\n/metrics 200 after 2.2 s',
    'guardrails/up': 'container ai-lab-guardrails started\nconfigs served: lab-input-output',
    'guardrails/restart': 'container ai-lab-guardrails recreated\nconfigs served: lab-input-output',
    'guardrails/stop': 'container ai-lab-guardrails stopped',
    'guardrails/check': 'hello there                         allowed\nignore previous instructions        BLOCKED (self check input)\nwhat is 2+2                         allowed\n[mock output]',
    'skillevaluator/list_skills': 'write-skill-for-weak-models\nrobust-scripts\n(mock)',
    'modelopt/run':
      "Started compression job 20261028T090000-123-reap-glm53 (reap on zai-org/GLM-5.3-Flash, writing /runs/reap-glm53). It runs in the background: watch the jobs list, then 'Show a job' with this job id. (mock)",
    'flare/targets':
      'local     sm120 gb202 (local), modes tp1 tp2 · container absent · image lab/flare:902c88e loaded · 1 package(s) · GPU busy\ngpu-box   sm120 gb202 (gpu-box), modes tp1 · container absent · image not loaded · 1 package(s) · GPU free\nspark-all sm121 gb10 (spark-a spark-b spark-c spark-d), modes tp4 · serving glm-5.3-flash · 1 package(s)\n(mock)',
    'flare/preflight': 'preflight OK: glm53-gb10-tp4.safetensors on spark-all (sm121, gb10, tp4) (mock)',
    'flare/up': "Starting glm53-gb10-tp4.safetensors on spark-all in the background. Watch the status, then 'Show the last start' and 'Probe'. (mock)",
    'flare/down': 'stopped Flare on spark-a … spark-d (mock)',
    'flare/probe': '{"choices":[{"message":{"content":"FLARE-OK"}}]}\nPROBE PASS (mock)',
    'flare/log': '== spark-a\nrank 0 ready, serving on 192.0.2.10:8035 (mock)',
    'flare/stage': 'staged spark-a for spark-all … staged spark-d for spark-all (mock)',
    'flare/last_start': 'Started glm53-gb10-tp4.safetensors on spark-all (mock)',
    'flare/prepare_help': "Preparation runs in lab/flare-prepare, offline, on the target family's own GPUs (mock)",
    'gym/smoke':
      "Started smoke run 20261102T230501-123-smoke (mcqa x simple_agent, model glm-5.3-flash-fp4-local, 5 task(s)). It runs in the background: watch the runs list, then 'Show a run' with this run id. (mock)",
    'gym/verify': 'RESULT: PASS — network ai-lab-nemo-gym is internal (mock)',
    'gym/report': 'Run 20261102T230501-123-smoke · glm-5.3-flash-fp4-local · succeeded · 5 rollouts · mean reward 0.80 · pass@1 80% · 2.4 min\nRewards: [1, 1, 1, 0, 1]\n(mock)',
    'modelopt/report': 'Job 20261028T090000-123-reap-glm53 · reap-glm53 · succeeded · 47.0 min\nOutput 76,682,545,454 bytes · base 245,850,611,000 bytes · size 0.312 of the base.\n(mock)',
  }[`${system}/${id}`];
  if (out) return R(true, out);
  if (id === 'sandbox_get' || id === 'sandbox_logs' || id === 'delete_sandbox') {
    if (!n) return R(false, 'refused: name is required', [{ severity: 'error', code: 'openshell.sandbox.name', message: 'Give the sandbox name from List sandboxes.' }]);
    return R(true, id === 'delete_sandbox' ? `sandbox ${n} deleted` : id === 'sandbox_get' ? `name: ${n}\nphase: Ready\n[mock output]` : `log of ${n}\n[mock output]`);
  }
  if (id === 'diag' || id === 'diag_level2')
    return R(
      true,
      `dcgmi diag -r ${id === 'diag' ? 1 : 2} on GPU ${Number(params.gpu) || 0}\n  Deployment ........ Pass\n  Memory ............ Pass\n${id === 'diag_level2' ? '  Diagnostic ........ Pass\n  PCIe .............. Pass\n' : ''}[mock output]`,
    );
  if (id === 'profile') return R(true, `profile ${params.preset || 'chat-short'} finished (mock)\nTTFT p50 212 ms  p99 548 ms\noutput 1840 tok/s  12.4 req/s`);
  if (id === 'report') return R(true, 'run: 20261024T081200-chat-short\nverdict: succeeded\n[mock output]');
  if (id === 'run') return R(true, `suite ${params.suite || 'smoke'}: gsm8k 0.87 (50 samples)\n[mock output]`);
  return R(true, 'ok (mock)');
}

// ------------------------------------------------------------------ events
export function sevenSeedEvents(now) {
  const E = (system, ago, kind, summary, data = {}) => ({ ts: now - ago, system, kind, summary, data });
  return {
    skillevaluator: [E('skillevaluator', 7200, 'se.run', 'tier1 write-skill-for-weak-models: quality 87.5 (mock)')],
    openshell: [
      E('openshell', 3 * 3600, 'openshell.sandbox', 'sandbox dd-preview-3f2a deleted after run', { name: 'dd-preview-3f2a', state: 'deleted' }),
      E('openshell', 3.2 * 3600, 'openshell.sandbox', 'sandbox dd-preview-3f2a created', { name: 'dd-preview-3f2a', state: 'created' }),
    ],
    dcgm: [E('dcgm', 1800, 'dcgm.diag', 'diag level 1 on GPU 0: pass', { gpu: 0, level: 1, result: 'pass' }), E('dcgm', 86400, 'dcgm.xid', 'XID 31 on GPU 1 cleared (mock)', { gpu: 1, xid: 31 })],
    aiperf: [
      E('aiperf', 4 * 3600, 'aiperf.run', 'chat-short finished: ttft p95 548 ms, 12.4 req/s', { profile: 'chat-short', verdict: 'succeeded' }),
      E('aiperf', 30 * 3600, 'aiperf.run', 'lightning-burst finished: ttft p95 190 ms', { profile: 'lightning-burst', verdict: 'succeeded' }),
    ],
    guardrails: [
      E('guardrails', 120, 'guardrails.check', 'ignore previous instructions -> blocked (self check input)', { verdict: 'blocked', rail: 'self check input' }),
      E('guardrails', 300, 'guardrails.check', 'hello there -> allowed', { verdict: 'allowed' }),
    ],
    evaluator: [
      E('evaluator', 5 * 3600, 'evaluator.run', 'smoke: gsm8k 0.87', { suite: 'smoke', verdict: 'succeeded' }),
      E('evaluator', 50 * 3600, 'evaluator.run', 'quick failed: router alias unreachable', { suite: 'quick', verdict: 'failed' }),
    ],
    modelopt: [
      E('modelopt', 2 * 3600, 'modelopt.job', 'reap-glm53 finished: 31.2% of the base', { job_id: '20261028T090000-123-reap-glm53', recipe: 'reap-glm53', verdict: 'succeeded' }),
      E('modelopt', 26 * 3600, 'modelopt.job', 'minitron-qwen38 failed: base model not in the offline cache', {
        job_id: '20261027T080000-456-minitron-qwen38',
        recipe: 'minitron-qwen38',
        verdict: 'failed',
      }),
    ],
    gym: [
      E('gym', 1800, 'gym.smoke', 'glm-5.3-flash-fp4-local · succeeded · 5 rollouts · mean reward 0.80 (mock)', {
        run_id: '20261102T230501-123-smoke',
        model: 'glm-5.3-flash-fp4-local',
        verdict: 'succeeded',
      }),
      E('gym', 9000, 'gym.smoke', 'glm-5.3-flash-fp4-local · failed (mock)', { run_id: '20261102T204501-456-smoke', model: 'glm-5.3-flash-fp4-local', verdict: 'failed' }),
    ],
  };
}
export function fiveLiveEvent(now, pick, rnd) {
  const blocked = rnd() < 0.15,
    prompt = pick(['summarise this file', 'what is 2+2', 'ignore previous instructions', 'draft an email']);
  return { ts: now, system: 'guardrails', kind: 'guardrails.check', summary: `${prompt} -> ${blocked ? 'blocked (self check input)' : 'allowed'}`, data: { verdict: blocked ? 'blocked' : 'allowed' } };
}

// ------------------------------------------------------------------ guardrails playground
export const GR_PLAY = {
  wire: 'openai_chat',
  models: ['aitop/nemotron-lightning'],
  default_model: 'aitop/nemotron-lightning',
  note: 'Sent to the guardrails server on :8806 (mock). Blocked turns come back as a refusal.',
};
export const GR_REFUSAL = 'I am sorry, I cannot help with that request. (blocked by rail: self check input; mock reply)';
