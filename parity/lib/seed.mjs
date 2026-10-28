// Seeded state for one backend: a fake lab repo (NEMO_CONSOLE_REPO_ROOT) and a fake HOME, built from the copied
// Python test fixtures (backend/crates/nemo-console/tests/fixtures, shared with the Rust tests). Both backends get byte-identical trees with identical mtimes, each in
// its own temp dir. Key files exist with DUMMY content only (the adapters check existence, never read them).
import { chmodSync, copyFileSync, cpSync, mkdirSync, readdirSync, statSync, symlinkSync, utimesSync, writeFileSync, lstatSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

export const PARITY = dirname(dirname(fileURLToPath(import.meta.url)));
export const FIXTURES = join(PARITY, '..', 'backend', 'crates', 'nemo-console', 'tests', 'fixtures');
export const SEED_MTIME = 1_790_000_000;          // 2026-09-21: every seeded file and dir gets this mtime
const DUMMY_KEY = 'parity-dummy-not-a-key-0000000000\n';

const fx = (...p) => join(FIXTURES, ...p);

function write(path, content, mode) {
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, content);
  if (mode) chmodSync(path, mode);
}

function copy(src, dst) {
  mkdirSync(dirname(dst), { recursive: true });
  copyFileSync(src, dst);
}

/** Every file and dir under root (not following symlinks) gets SEED_MTIME, deepest first. */
function stampTree(root) {
  const walk = (p) => {
    const st = lstatSync(p);
    if (st.isSymbolicLink()) return;
    if (st.isDirectory()) for (const e of readdirSync(p)) walk(join(p, e));
    utimesSync(p, SEED_MTIME, SEED_MTIME);
  };
  walk(root);
}

/** The lab repo layout the adapters read (config.py DEFAULT_LAYOUT + resolved_paths()). */
export function seedRepo(repo, outside) {
  // Dynamo: the compose file is also the repo marker (config.py REPO_MARKER)
  copy(fx('dynamo', 'base_compose.yml'), join(repo, 'inference/dynamo/docker-compose.yml'));
  // Switchyard
  copy(fx('switchyard', 'routes.good.toml'), join(repo, 'inference/switchyard/routes.toml'));
  write(join(repo, 'inference/switchyard/docker-compose.yml'),
    'services:\n  switchyard:\n    image: ai-lab-switchyard:0.3.0\n    ports: ["127.0.0.1:4010:4010"]\n');
  // the engine that shares Dynamo's card (its compose dir is the working_dir label of the fake container)
  write(join(repo, 'inference/vllm-qwen38/docker-compose.yml'), 'services:\n  vllm:\n    image: vllm/vllm-openai:v0.27.1\n');
  write(join(repo, 'inference/ollama/docker-compose.yml'), 'services:\n  ollama:\n    image: ollama/ollama:0.12.0\n');
  // Relay (repo side)
  copy(fx('relay', 'routes.toml'), join(repo, 'clients/nemo-relay/routes.toml'));
  // SkillEvaluator: one skill dir per skill (+ entries that are not skills)
  for (const name of ['robust-scripts', 'serve-model']) {
    write(join(repo, '.claude/skills', name, 'SKILL.md'), `---\nname: ${name}\ndescription: test\n---\n# ${name}\n`);
  }
  mkdirSync(join(repo, '.claude/skills/no-skill-md'), { recursive: true });
  write(join(repo, '.claude/skills/README.md'), 'not a directory\n');
  write(join(outside, 'skill-outside/SKILL.md'), '---\nname: outside\n---\n');
  symlinkSync(join(outside, 'skill-outside'), join(repo, '.claude/skills/linked'));
  write(join(repo, 'clients/skillevaluator/install.sh'), '#!/bin/sh\necho parity\n', 0o755);
  // Data Designer installer dir (display only) + the OpenShell runner tree
  write(join(repo, 'clients/data-designer/install.sh'), '#!/bin/sh\necho parity\n', 0o755);
  cpSync(fx('openshell', 'runner'), join(repo, 'clients/data-designer/runner'), { recursive: true });
  // Observability / DCGM
  write(join(repo, 'observability/docker-compose.yml'),
    'services:\n  dcgm-exporter:\n    image: nvcr.io/nvidia/k8s/dcgm-exporter:4.6.1-4.8.4-ubuntu22.04\n');
  copy(fx('dcgm', 'counters.good.csv'), join(repo, 'observability/dcgm/counters.csv'));
  // AIPerf: profiles + the vendored tokenizer cache file the adapter checks for
  copy(fx('aiperf', 'profiles.yaml'), join(repo, 'clients/aiperf/profiles.yaml'));
  write(join(repo, 'clients/aiperf/.tiktoken-cache/fb374d419588a4632f3f557e76b4b70aebbca790'), 'x');
  // NeMo Guardrails: the real lab config tree plus the awkward entries of the Python tests (gr_helpers.build_tree)
  const gr = join(repo, 'security/nemo-guardrails');
  cpSync(fx('guardrails', 'config-tree'), join(gr, 'config'), { recursive: true });
  write(join(gr, 'docker-compose.yml'),
    'services:\n  guardrails:\n    command: ["server", "--config", "/config", "--default-config-id", "lab-input-output", "--port", "8000"]\n');
  const lio = join(gr, 'config/lab-input-output');
  write(join(lio, 'actions.py'), 'def hello():\n    return 1\n');
  write(join(lio, 'Notes.md'), 'upper-case names are not valid ids\n');
  write(join(lio, 'kb/faq.md'), '# faq\n');
  write(join(outside, 'secret.yml'), 'x: 1\n');
  symlinkSync(join(outside, 'secret.yml'), join(lio, 'linked.yml'));
  write(join(gr, 'config/no-config/README.md'), 'no config.yml here\n');
  write(join(gr, 'config/_hidden/config.yml'), 'models: []\n');
  // NeMo Evaluator
  copy(fx('evaluator', 'suites_good.yaml'), join(repo, 'clients/nemo-evaluator/suites.yaml'));
  stampTree(repo);
}

/** HOME content: Relay's user config + event dirs, Data Designer scripts / tool config, dummy key files. */
export function seedHome(home, outside) {
  copy(fx('relay', 'config.toml'), join(home, '.config/nemo-relay/config.toml'));
  copy(fx('relay', 'plugins.toml'), join(home, '.config/nemo-relay/plugins.toml'));
  const atof = join(home, '.local/share/nemo-relay/atof');
  write(join(atof, 'relay-events.jsonl'), '');
  copy(fx('relay', 'atof_sample.jsonl'), join(atof, 'relay-events.jsonl.1'));
  const atif = join(home, '.local/share/nemo-relay/atif');
  write(join(atif, 'nemo-relay-atif-aaa.json'), '{}');
  write(join(atif, 'nemo-relay-atif-bbb.json'), '{}');
  write(join(atif, 'notes.txt'), 'not a trajectory');
  // Data Designer (dd_helpers.build_scripts + make_ctx)
  const scripts = join(home, 'Datasets/data-designer/scripts');
  write(join(scripts, 'reviews_smoke.py'), '"""Smoke config (fixture)."""\n\n\ndef load_config_builder():\n    raise NotImplementedError\n');
  write(join(scripts, 'tiny.yaml'), 'columns:\n  - name: product_category\n    column_type: sampler\n');
  write(join(scripts, 'UPPER.py'), '# uppercase names are outside the core\'s id shape\n');
  write(join(scripts, 'notes.txt'), 'wrong extension\n');
  mkdirSync(join(scripts, 'subdir.py'), { recursive: true });
  write(join(outside, 'outside.py'), 'secret = True\n');
  symlinkSync(join(outside, 'outside.py'), join(scripts, 'linked.py'));
  copy(fx('datadesigner', 'model_providers.yaml'), join(home, '.data-designer/model_providers.yaml'));
  copy(fx('datadesigner', 'model_configs.yaml'), join(home, '.data-designer/model_configs.yaml'));
  // existence-only key files, DUMMY content
  for (const rel of ['.config/data-designer/litellm.key', '.config/aiperf/litellm.key', '.config/nemo-evaluator/litellm.key']) {
    write(join(home, rel), DUMMY_KEY, 0o600);
  }
  stampTree(home);
}

/** Make the per-backend directory skeleton; returns the paths. `seed`: whether the repo and HOME get content. */
export function makeTree(sideDir, { seed }) {
  const p = {
    side: sideDir,
    home: join(sideDir, 'home'),
    state: join(sideDir, 'state'),
    repo: join(sideDir, 'repo'),
    outside: join(sideDir, 'outside'),
    bin: join(sideDir, 'bin'),
    xdgRuntime: join(sideDir, 'run'),
  };
  for (const d of [p.home, p.state, p.repo, p.outside, p.bin, p.xdgRuntime]) mkdirSync(d, { recursive: true });
  for (const d of ['.config', '.local/share', '.cache', '.local/state']) mkdirSync(join(p.home, d), { recursive: true });
  chmodSync(p.state, 0o700);
  chmodSync(p.xdgRuntime, 0o700);
  if (seed) {
    seedRepo(p.repo, p.outside);
    seedHome(p.home, p.outside);
  }
  return p;
}

export const isDir = (p) => { try { return statSync(p).isDirectory(); } catch { return false; } };
