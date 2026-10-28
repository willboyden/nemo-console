// Scripted answers of the fake CLIs (see fakes/cli.mjs): respond(ctx) -> {stdout, stderr, code, delayMs, files} or
// null for "unknown argv". ctx: {scenario, fixtures, repo, home, side, state, name, args, argv, stdin, cwd, world}.
// Answers are deterministic: they depend on the argv, the seeded files and a small "world" (fake-world.json in the
// backend's temp dir) that mutating commands change, the way the real machine would:
//   docker compose up/down/stop, docker restart        -> containers start / stop (a restart gets a new StartedAt)
//   openshell sandbox delete, docker network connect   -> the sandbox list / the router pin
// The captured outputs come from the Python tests' fixtures (backend/crates/nemo-console/tests/fixtures); dirs named in argv (-o,
// --artifact-dir, --output-dir, --results-dir, ...) receive the files the real tools write.
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';

const SY = 'ai-lab-switchyard-switchyard-1';
const LIGHTNING = 'ai-lab-vllm-nemotron-lightning-nemotron-lightning-1';
const BOLT = 'ai-lab-vllm-nemotron-bolt-nemotron-bolt-1';
const GLM = 'lil-glm53-tp2';
const FLASHNEXT = 'ai-lab-sglang-flashnext-flashnext-1';
const QWEN38 = 'ai-lab-vllm-qwen38-vllm-1';
const FE = 'ai-lab-dynamo-dynamo-frontend-1';
const WK = 'ai-lab-dynamo-dynamo-worker-1';
const MK = 'ai-lab-dynamo-dynamo-mocker-1';
const SV = 'ai-lab-dynamo-dynamo-sv-frontend-1';
const DCGM = 'ai-lab-observability-dcgm-exporter-1';
const GR = 'ai-lab-nemo-guardrails';
const GR_GW = 'ai-lab-nemo-guardrails-gw';
const GR_BROKER = 'ai-lab-nemo-guardrails-router-broker';
const OS_GW = 'ai-lab-dd-gateway';
const OS_FWD = 'ai-lab-dd-cli-fwd';
const LITELLM = 'ai-lab-litellm-litellm-1';
const UUID = 'GPU-00000000-0000-0000-0000-000000000001';
const CAPTURED_DIR = '/home/user/lab/inference/dynamo';

// docker --format templates the adapters use (verbatim)
const F_SY = '{{.State.Status}}|{{if .State.Health}}{{.State.Health.Status}}{{end}}|{{.Config.Image}}|{{index .Config.Labels "com.docker.compose.project"}}|{{index .Config.Labels "com.docker.compose.service"}}';
const F_GR = '{{.State.Status}}|{{if .State.Health}}{{.State.Health.Status}}{{end}}|{{.Config.Image}}|{{index .Config.Labels "com.docker.compose.project"}}';
const F_GR_ISO = '{{range $k, $v := .NetworkSettings.Networks}}{{$k}} {{end}}|{{json .NetworkSettings.Ports}}';
const F_DCGM = '{{.State.Status}}|{{.State.Running}}|{{.Config.Image}}|{{.State.StartedAt}}|{{.State.ExitCode}}';
const F_DCGM_LABELS = '{{index .Config.Labels "com.docker.compose.project"}}|{{index .Config.Labels "com.docker.compose.service"}}';
const F_OS = '{{.Name}}|{{.State.Status}}|{{.State.ExitCode}}|{{.State.OOMKilled}}|{{.State.StartedAt}}|{{.RestartCount}}|{{.Config.Image}}|{{json .NetworkSettings.Ports}}';
const F_NET = '{{range $k,$v := .NetworkSettings.Networks}}{{$k}}={{$v.IPAddress}} {{end}}';

/** The machine at backend start (full scenario). */
export function initialWorld() {
  const run = (extra = {}) => ({ status: 'running', exitCode: 0, ...extra });
  return {
    seq: 0,
    containers: {
      [SY]: run(), [LIGHTNING]: run(), [GLM]: { status: 'exited', exitCode: 0 }, [QWEN38]: run({ engine: true }),
      [FE]: run(), [WK]: run(), [DCGM]: run(), [GR]: run(), [GR_GW]: run(), [GR_BROKER]: run(),
      [OS_GW]: run(), [OS_FWD]: run(), [LITELLM]: run(),
    },
    sandboxes: true,           // sandbox-list.json (one sandbox) until it is deleted
    litellmPinned: false,
  };
}

const ok = (stdout = '', extra = {}) => ({ code: 0, stdout, ...extra });
const fail = (stderr, code = 1, stdout = '') => ({ code, stderr, stdout });
const noSuch = (names) => fail(names.map((n) => `Error: No such object: ${n}\n`).join(''));
const after = (args, flag) => { const i = args.indexOf(flag); return i >= 0 ? args[i + 1] : undefined; };
const running = (w, n) => w.containers[n]?.status === 'running';
const startedAt = (w, n) => {
  const s = w.containers[n]?.started ?? 0;
  return s ? `2026-10-01T00:00:${String(s).padStart(2, '0')}.000000000Z` : null;
};

export function respond(ctx) {
  const fx = (...p) => readFileSync(join(ctx.fixtures, ...p), 'utf8');
  const H = HANDLERS[ctx.name];
  return H ? H(ctx, ctx.args, fx) : null;
}

// ── dynamo inspect documents ────────────────────────────────────────────────────────────────
function dynamoDocs(ctx, fx) {
  const dir = join(ctx.repo, 'inference', 'dynamo');
  const load = (f) => JSON.parse(fx('dynamo', f).split(CAPTURED_DIR).join(dir))[0];
  const w = ctx.world;
  const docs = {};
  const stamp = (d, n) => {
    const st = w.containers[n];
    d.State = { ...d.State, Status: st.status, Running: st.status === 'running', ExitCode: st.exitCode ?? 0 };
    if (startedAt(w, n)) d.State.StartedAt = startedAt(w, n);
    return d;
  };
  if (w.containers[FE]) docs[FE] = stamp(load('frontend_inspect.json'), FE);
  if (w.containers[WK]) docs[WK] = stamp(load('worker_inspect.json'), WK);
  if (w.containers[MK]) {
    const d = load('worker_inspect.json');
    d.Name = `/${MK}`;
    d.Config.Labels['com.docker.compose.service'] = 'dynamo-mocker';
    docs[MK] = stamp(d, MK);
  }
  if (w.containers[QWEN38]) {
    const d = load('worker_inspect.json');
    const engDir = join(ctx.repo, 'inference', 'vllm-qwen38');
    d.Name = `/${QWEN38}`;
    d.Config.Image = 'vllm/vllm-openai:v0.27.1';
    Object.assign(d.Config.Labels, { 'com.docker.compose.project': 'ai-lab-vllm-qwen38', 'com.docker.compose.service': 'vllm',
      'com.docker.compose.project.working_dir': engDir, 'com.docker.compose.project.config_files': join(engDir, 'docker-compose.yml') });
    d.HostConfig.DeviceRequests[0].DeviceIDs = [UUID];
    docs[QWEN38] = stamp(d, QWEN38);
  }
  return docs;
}

function dockerInspect(ctx, a, fx) {
  const w = ctx.world;
  // engine probe: docker inspect -f {{.State.Status}} NAME
  if (a[1] === '-f' && a[2] === '{{.State.Status}}' && a.length === 4) {
    const c = w.containers[a[3]];
    return c ? ok(`${c.status}\n`) : noSuch([a[3]]);
  }
  // openshell's router pin probe: docker inspect NAME --format F_NET
  if (a.length === 4 && a[2] === '--format' && a[3] === F_NET) {
    if (!w.containers[a[1]]) return noSuch([a[1]]);
    return ok(`ai-lab=172.18.0.5 ${w.litellmPinned ? 'openshell-docker=172.16.0.5 ' : ''}\n`);
  }
  if (a[1] === '--format') {
    const fmt = a[2];
    const names = a.slice(3);
    const missing = names.filter((n) => !w.containers[n]);
    let out = '';
    for (const n of names.filter((x) => w.containers[x])) {
      const st = w.containers[n].status;
      if (fmt === F_SY && n === SY) out += `${st}|${st === 'running' ? 'healthy' : ''}|ai-lab-switchyard:0.3.0|ai-lab-switchyard|switchyard\n`;
      else if (fmt === F_GR && n === GR) out += fx('guardrails', 'inspect_server.txt').replace(/^running/, st);
      else if (fmt === F_GR && n === GR_GW) out += fx('guardrails', 'inspect_gw.txt').replace(/^running/, st);
      else if (fmt === F_GR && n === GR_BROKER) out += fx('guardrails', 'inspect_broker.txt').replace(/^running/, st);
      else if (fmt === F_GR_ISO && n === GR) out += fx('guardrails', 'inspect_isolation.txt');
      else if (fmt === F_DCGM && n === DCGM) out += fx('dcgm', 'inspect.txt').replace(/^running\|true/, st === 'running' ? 'running|true' : `${st}|false`);
      else if (fmt === F_DCGM_LABELS && n === DCGM) out += 'ai-lab-observability|dcgm-exporter\n';
      else if (fmt === F_OS && (n === OS_GW || n === OS_FWD)) {
        const lines = fx('openshell', st === 'running' ? 'docker-inspect-running.txt' : 'docker-inspect-stopped.HAND-WRITTEN.txt').split('\n');
        out += `${lines.find((l) => l.startsWith(`/${n}|`))}\n`;
      } else return null;                       // a format/name pair the table does not know
    }
    if (missing.length) return { code: 1, stdout: out, stderr: missing.map((n) => `Error: No such object: ${n}\n`).join('') };
    return ok(out);
  }
  // dynamo: docker inspect NAME... (JSON documents of the existing ones, rc 1 + stderr for the rest)
  const names = a.slice(1);
  if (!names.length || names.some((n) => n.startsWith('-'))) return null;
  const docs = dynamoDocs(ctx, fx);
  const found = names.filter((n) => docs[n]).map((n) => docs[n]);
  const missing = names.filter((n) => !docs[n]);
  const stdout = `${JSON.stringify(found, null, 4)}\n`;
  return missing.length ? { code: 1, stdout, stderr: missing.map((n) => `Error: No such object: ${n}\n`).join('') } : ok(stdout);
}

function dockerCompose(ctx, a, fx) {
  const w = ctx.world;
  const files = [];
  for (let i = 0; i < a.length; i++) if (a[i] === '-f') files.push(a[i + 1]);
  const projDir = after(a, '--project-directory');
  const tailIdx = (() => { let i = 1; while (i < a.length && a[i].startsWith('-')) { i += a[i] === '--project-directory' || a[i] === '-f' || a[i] === '--profile' ? 2 : 1; } return i; })();
  const tail = a.slice(tailIdx);
  const is = (...t) => tail.length === t.length && t.every((x, i) => x === tail[i]);
  const start = (...ns) => { for (const n of ns) { w.seq += 1; w.containers[n] = { status: 'running', exitCode: 0, started: w.seq }; } ctx.worldChanged = true; };
  const stop = (...ns) => { for (const n of ns) if (w.containers[n]) w.containers[n] = { ...w.containers[n], status: 'exited', exitCode: 0 }; ctx.worldChanged = true; };
  const remove = (...ns) => { for (const n of ns) delete w.containers[n]; ctx.worldChanged = true; };
  const f0 = files[0] || '';
  // Switchyard recreate
  if (f0.endsWith('/inference/switchyard/docker-compose.yml') && is('up', '-d', '--force-recreate', '--no-build')) {
    let routes = '';
    try { routes = readFileSync(join(dirname(f0), 'routes.toml'), 'utf8'); } catch { /* none */ }
    if (routes.includes('parity-recreate-fail')) return fail(`Error response from daemon: failed to create task for container ${SY}: routes.toml rejected (parity fake)\n`);
    start(SY);
    return ok('', { stderr: ` Container ${SY}  Recreate\n Container ${SY}  Recreated\n Container ${SY}  Starting\n Container ${SY}  Started\n` });
  }
  // NeMo Guardrails
  if (f0.endsWith('/security/nemo-guardrails/docker-compose.yml')) {
    if (is('up', '-d', '--no-build')) { start(GR, GR_GW, GR_BROKER); return ok('', { stderr: ` Container ${GR}  Started\n` }); }
    if (is('stop')) { stop(GR, GR_GW, GR_BROKER); return ok('', { stderr: ` Container ${GR}  Stopped\n` }); }
  }
  // OpenShell runner
  if (f0.endsWith('/clients/data-designer/runner/docker-compose.yml')) {
    if (is('up', '-d')) { start(OS_GW, OS_FWD); return ok(`Container ${OS_GW}  Running\nContainer ${OS_FWD}  Running\n`); }
    if (is('stop')) { stop(OS_GW, OS_FWD); return ok('', { stderr: ` Container ${OS_FWD}  Stopped\n Container ${OS_GW}  Stopped\n` }); }
  }
  // the engine that shares Dynamo's card: docker compose -f CFG down (cwd = its working dir)
  if (!projDir && f0.endsWith('/inference/vllm-qwen38/docker-compose.yml') && is('down')) {
    remove(QWEN38);
    return ok('', { stderr: ` Container ${QWEN38}  Removed\n` });
  }
  // Dynamo
  if (projDir && projDir.endsWith('/inference/dynamo')) {
    if (is('config', '--format', 'json')) {
      return ok(`${JSON.stringify({ name: 'ai-lab-dynamo', services: { 'dynamo-worker': { deploy: { resources: { reservations: {
        devices: [{ driver: 'nvidia', device_ids: [UUID], capabilities: ['gpu'] }] } } } } } })}\n`);
    }
    if (is('config', '-q')) {
      const cand = files[files.length - 1];
      let text = '';
      try { text = readFileSync(cand, 'utf8'); } catch { /* gone */ }
      return text.includes('parity-invalid') ? fail('services.dynamo-worker.command must be a list\n') : ok('');
    }
    if (is('up', '-d', 'dynamo-frontend', 'dynamo-worker') || is('up', '-d', '--force-recreate', 'dynamo-frontend', 'dynamo-worker')) {
      start(FE, WK);
      return ok('', { stderr: ` Container ${FE}  Started\n Container ${WK}  Started\n` });
    }
    if (is('down') && a.includes('--profile')) { remove(FE, WK, MK, SV); return ok('', { stderr: ` Container ${WK}  Removed\n Container ${FE}  Removed\n` }); }
    if (is('up', '-d', 'dynamo-frontend', 'dynamo-mocker')) { start(FE, MK); return ok('', { stderr: ` Container ${MK}  Started\n` }); }
  }
  return null;
}

function dockerRun(ctx, a, fx) {
  const ep = after(a, '--entrypoint');
  if (ep === 'switchyard-server') {
    if (ctx.stdin.includes('parity-dryrun-fail')) return fail(fx('switchyard', 'dryrun_badtarget.stderr'), 1, fx('switchyard', 'dryrun_badtarget.stdout'));
    return ok(fx('switchyard', 'dryrun_ok.stdout'), { stderr: fx('switchyard', 'dryrun_ok.stderr') });
  }
  if (ep === '/bin/sh') {
    const script = a[a.length - 1];
    if (script.endsWith('dcgmi discovery -l')) return ok(fx('dcgm', 'discovery.txt'));
    if (/dcgmi diag -r [12] -i \d+ -j$/.test(script)) return ok(fx('dcgm', 'diag_level1_gpu0.json'));
  }
  return null;
}

function dockerExec(ctx, a, fx) {
  const w = ctx.world;
  if (a[1] === SY && a[2] === 'tail' && a[3] === '-n') {
    if (!running(w, SY)) return fail(`Error response from daemon: container ${SY} is not running\n`);
    const n = Number(a[4]);
    const lines = fx('switchyard', 'routing_tail.jsonl').split('\n').filter(Boolean);
    return ok(lines.slice(-n).map((l) => `${l}\n`).join(''));
  }
  if (a[1] === WK && a[2] === 'python3' && a[3] === '-c') {
    if (!running(w, WK)) return fail(`Error response from daemon: No such container: ${WK}\n`);
    return ok(`${fx('dynamo', 'worker_health.json').trim()}\n---METRICS---\n${fx('dynamo', 'worker_metrics.prom')}`);
  }
  return null;
}

function dockerLogs(ctx, a, fx) {
  const name = a[a.length - 1];
  const w = ctx.world;
  if (!w.containers[name]) return fail(`Error response from daemon: No such container: ${name}\n`);
  if (a[1] === '--timestamps') {                                  // dynamo: recent (--tail N) or follow (--since)
    if (a.includes('--since')) return ok('');
    const file = name === FE ? 'frontend_logs.txt' : name === WK ? 'worker_logs.txt' : null;
    if (!file) return ok('');
    const n = Number(after(a, '--tail') || 1e9);
    return ok(fx('dynamo', file).split('\n').filter(Boolean).slice(-n).map((l) => `${l}\n`).join(''));
  }
  if (a[1] === '--since' && name === DCGM) return ok('time=2026-10-24T11:13:31.052Z level=INFO msg="Starting dcgm-exporter" Version=4.8.4\n');
  if (a[1] === '--tail' && name === OS_GW) {
    const n = Number(a[2]);
    return ok(fx('openshell', 'logs.txt').split('\n').filter(Boolean).slice(-n).map((l) => `${l}\n`).join(''));
  }
  return null;
}

const HANDLERS = {
  docker(ctx, a, fx) {
    const w = ctx.world;
    switch (a[0]) {
      case 'inspect': return dockerInspect(ctx, a, fx);
      case 'image':
        if (a[1] === 'inspect' && a[2] === '--format' && a[3] === '{{.Id}}') return ok('sha256:48464e50aa76\n');
        return null;
      case 'compose': return dockerCompose(ctx, a, fx);
      case 'run': return dockerRun(ctx, a, fx);
      case 'exec': return dockerExec(ctx, a, fx);
      case 'logs': return dockerLogs(ctx, a, fx);
      case 'rm': return a[1] === '-f' ? ok(`${a[2]}\n`) : null;
      case 'restart': {
        const name = a[a.length - 1];
        if (!w.containers[name]) return fail(`Error response from daemon: No such container: ${name}\n`);
        w.seq += 1;
        w.containers[name] = { ...w.containers[name], status: 'running', started: w.seq };
        ctx.worldChanged = true;
        return ok(`${name}\n`);
      }
      case 'ps':
        if (a[1] === '--format' && a[2] === '{{.Names}}') return ok(Object.keys(w.containers).filter((n) => running(w, n)).sort().map((n) => `${n}\n`).join(''));
        return null;
      case 'network':
        if (a[1] === 'inspect') {
          // openshell: network inspect openshell-docker --format X ; guardrails: network inspect --format X NET
          const net = a[2] === '--format' ? a[4] : a[2];
          if (net === 'openshell-docker' || net === 'ai-lab-guardrails') return ok('true\n');
          return fail(`Error response from daemon: network ${net} not found\n`);
        }
        if (a[1] === 'disconnect') { w.litellmPinned = false; ctx.worldChanged = true; return ok(''); }
        if (a[1] === 'connect') { w.litellmPinned = true; ctx.worldChanged = true; return ok(''); }
        return null;
      default: return null;
    }
  },

  systemctl(ctx, a, fx) {
    if (a[0] !== '--user') return null;
    if (a[1] === '--timestamp=unix' && a[2] === 'show') return ok(fx('relay', 'systemctl_show_active.txt'));
    if (a[1] === 'restart' && a[2] === 'nemo-relay-gateway.service') return ok('');
    if (a[1] === 'status' && a[2] === '--no-pager') {
      return ok(`● nemo-relay-gateway.service - NeMo Relay gateway (parity fake)\n     Loaded: loaded (/home/user/.config/systemd/user/nemo-relay-gateway.service; enabled)\n     Active: active (running) since Sat 2026-09-24 08:01:17 UTC\n   Main PID: 2247751 (nemo-relay)\n`);
    }
    return null;
  },

  'nemo-relay'(ctx, a, fx) {
    const k = a.join(' ');
    if (k === '--version') return ok(fx('relay', 'version.txt'));
    if (k === 'plugins list --json') return ok(fx('relay', 'plugins_list.json'));
    if (k === 'agents --json') return ok(fx('relay', 'agents.json'));
    if (k === 'doctor --offline --json') return ok(fx('relay', 'doctor_offline.json'));
    if (a.slice(0, 4).join(' ') === 'plugins validate --json --' && a.length === 5) {
      return a[4] === 'nvidia.switchyard' ? ok(fx('relay', 'plugins_validate.json')) : fail('', 1, fx('relay', 'plugins_validate_notfound.json'));
    }
    return null;
  },

  'guardrails-local'(ctx, a, fx) {
    if (a.length === 1 && a[0] === '--version') return ok(fx('guardrails', 'version.txt'));
    if (a[0] === 'validate' && a.length === 2) {
      let cfg = '';
      try { cfg = readFileSync(join(a[1], 'config.yml'), 'utf8') + readFileSync(join(a[1], 'prompts.yml'), 'utf8'); } catch { /* none */ }
      if (cfg.includes('parity-invalid')) return fail(fx('guardrails', 'validate_flow.stderr'), 1, fx('guardrails', 'validate_flow.stdout'));
      return ok(fx('guardrails', 'validate_ok.stdout'), { stderr: fx('guardrails', 'validate_ok.stderr') });
    }
    return null;
  },

  openshell(ctx, a, fx) {
    const w = ctx.world;
    const k = a.join(' ');
    const up = running(w, OS_GW);
    if (k === '--version') return ok(fx('openshell', 'version.txt'));
    if (k === 'doctor check') return ok(fx('openshell', 'doctor-check.txt'));
    if (a[0] === 'policy' && a[1] === 'prove') {
      return a.includes('--compact') ? fail('', 1, fx('openshell', 'prove-compact-findings.raw.txt')) : ok(fx('openshell', 'prove-clean.raw.txt'));
    }
    if (a[0] !== '-g' || a[1] !== 'openshell') return null;
    const r = a.slice(2);
    const rk = r.join(' ');
    if (rk === 'status') return ok(fx('openshell', up ? 'status-connected.raw.txt' : 'status-disconnected.HAND-WRITTEN.raw.txt'));
    if (!up && ['sandbox', 'provider', 'logs'].includes(r[0])) return fail('Error: transport error: connection refused (os error 111)\n');
    if (rk === 'sandbox list --limit 200 -o json') return ok(fx('openshell', w.sandboxes ? 'sandbox-list.json' : 'sandbox-list-empty.json'));
    if (rk === 'provider list -o json') return ok(fx('openshell', 'provider-list.json'));
    if (r[0] === 'sandbox' && r[1] === 'get' && r.length === 3) {
      return r[2] === 'nc-adapter-probe' && w.sandboxes ? ok(fx('openshell', 'sandbox-get.raw.txt')) : fail(fx('openshell', 'sandbox-get-missing.raw.txt'));
    }
    if (r[0] === 'logs' && r.length === 4 && r[2] === '-n') return ok(fx('openshell', 'logs.txt').split('\n').slice(-Number(r[3]) - 1).join('\n'));
    if (r[0] === 'sandbox' && r[1] === 'delete' && r.length === 3) {
      if (r[2] !== 'nc-adapter-probe' || !w.sandboxes) return fail(`Error: sandbox ${r[2]} not found\n`);
      w.sandboxes = false;
      ctx.worldChanged = true;
      return ok(`Deleted sandbox ${r[2]}\n`);
    }
    return null;
  },

  'aiperf-local'(ctx, a, fx) {
    if (a.length === 1 && a[0] === '--version') return ok(fx('aiperf', 'version.txt'));
    if (a[0] === 'profile' && a.includes('--artifact-dir')) {
      const dir = after(a, '--artifact-dir');
      if (after(a, '--model') === 'no-such-model-xyz') {
        return { code: 1, stdout: '', stderr: fx('aiperf', 'all-failed.txt'), files: [{ path: join(dir, 'profile_export.jsonl'), copyFrom: join(ctx.fixtures, 'aiperf', 'failed-records.jsonl') }] };
      }
      return { code: 0, stdout: fx('aiperf', 'smoke-console.txt'), files: [{ path: join(dir, 'profile_export_aiperf.json'), copyFrom: join(ctx.fixtures, 'aiperf', 'profile_export_aiperf.json') }] };
    }
    return null;
  },

  'skillevaluator-local'(ctx, a, fx) { return skilleval(ctx, a, fx); },
  skillevaluator(ctx, a, fx) { return skilleval(ctx, a, fx); },

  'data-designer-local'(ctx, a, fx) {
    const F = (...p) => join(ctx.fixtures, 'datadesigner', ...p);
    if (a.length === 1 && a[0] === '--version') return ok('0.9.3\n');
    if (a[0] === 'check-models' && a.length === 2) return ok('All 2 models reachable.\nlab-router: ok\nlab-embed: ok\n');
    if (a[0] === 'validate' && a.length === 2) {
      let text = '';
      try { text = readFileSync(a[1], 'utf8'); } catch { /* gone */ }
      return text.includes('parity-invalid') ? fail('Config error at line 3\n  unknown column type: nope\n') : ok('Config is valid.\n');
    }
    if (a[0] === 'preview' && a.includes('-o')) {
      const out = join(after(a, '-o'), 'preview_results_20261023_222454');
      return { code: 0, stdout: 'preview progress lines\n', files: [
        { path: join(out, 'dataset.parquet'), copyFrom: F('preview-artifacts', 'dataset.parquet') },
        { path: join(out, 'report.html'), copyFrom: F('preview-artifacts', 'report.html') },
        ...['record_0.html', 'record_1.html', 'sample_records_browser.html'].map((f) => ({ path: join(out, 'sample_records', f), copyFrom: F('sample_records', f) })),
      ] };
    }
    if (a[0] === 'create' && a.includes('--artifact-path')) {
      const out = join(after(a, '--artifact-path'), after(a, '--dataset-name'));
      const md = JSON.parse(fx('datadesigner', 'create-artifacts', 'metadata.json'));
      md.file_paths = { 'parquet-files': ['parquet-files/batch_00000.parquet'] };
      return { code: 0, stdout: 'create progress lines\n', files: [
        { path: join(out, 'parquet-files', 'batch_00000.parquet'), copyFrom: F('parquet-files', 'batch_00000.parquet') },
        { path: join(out, 'metadata.json'), content: JSON.stringify(md) },
        { path: join(out, 'builder_config.json'), copyFrom: F('create-artifacts', 'builder_config.json') },
      ] };
    }
    return null;
  },

  'data-designer-export'(ctx, a, fx) {
    if (a.length !== 2) return null;
    const rows = fx('datadesigner', 'dd_smoke.jsonl').split('\n').filter(Boolean).map((l) => JSON.parse(l));
    const out = a[1];
    let content;
    if (out.endsWith('.jsonl')) content = rows.map((r) => `${JSON.stringify(r)}\n`).join('');
    else if (out.endsWith('.json')) content = JSON.stringify(rows);
    else if (out.endsWith('.csv')) {
      const cols = Object.keys(rows[0]).sort();
      const q = (v) => { const s = String(v ?? ''); return /[",\n]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s; };
      content = `${cols.join(',')}\r\n${rows.map((r) => cols.map((c) => q(r[c])).join(',')).join('\r\n')}\r\n`;
    } else content = 'PARQUET';
    return { code: 0, stdout: '', files: [{ path: out, content }] };
  },

  'evaluator-local'(ctx, a, fx) {
    if (a.length === 1 && a[0] === '--version') return ok(fx('evaluator', 'version.txt'));
    if (a.join(' ') === 'cached --json') return ok(fx('evaluator', 'cached_all.json'));
    if (a[0] === 'suites' && a[1] === '--file' && a.length === 3) {
      let text = '';
      try { text = readFileSync(a[2], 'utf8'); } catch { /* gone */ }
      return text.includes('parity-invalid') ? fail('evaluator-local: invalid suites file:\n  boom\n') : ok('smoke-lightning  aitop/nemotron-lightning  gsm8kx10\n');
    }
    if (a[0] === 'run' && a.includes('--output-dir')) {
      const suite = after(a, '--suite');
      const doc = JSON.parse(fx('evaluator', suite === 'knowledge-lightning' ? 'summary_mmlu.json' : 'summary_smoke.json'));
      doc.suite = suite;
      return { code: 0, stdout: fx('evaluator', 'nel_tail_smoke.txt'), files: [
        { path: join(after(a, '--output-dir'), 'summary.json'), content: JSON.stringify(doc) },
        { path: join(after(a, '--output-dir'), 'gsm8k', 'results.jsonl'), content: '{"prompt": "Q", "model_response": "A"}\n' },
      ] };
    }
    return null;
  },
};

function skilleval(ctx, a, fx) {
  if (a.length === 1 && a[0] === '--version') return ok('skillevaluator, version 0.5.0\n');
  if (a.length === 1 && a[0] === 'doctor') return ok('CLI package pass skillevaluator 0.5.0\nPublic LLM provider pass openai-compatible\n');
  if (a[0] === 'tier1' && a.includes('-o')) {
    const out = after(a, '-o');
    return { code: 1, stdout: 'progress lines\n', stderr: 'Error: Tier 1 checks failed or were incomplete\n', files: [
      { path: join(out, 'skillevaluator-tier1.json'), copyFrom: join(ctx.fixtures, 'skillevaluator', 'tier1_incomplete.json') },
      { path: join(out, 'skillevaluator-tier1.md'), content: '# SkillEvaluator Validation Report\n\n**Status:** INCOMPLETE\n' },
    ] };
  }
  if (a[0] === 'tier2' && a.includes('-o')) {
    return { code: 0, stdout: 'progress lines\n', files: [
      { path: join(after(a, '-o'), 'skillevaluator-tier2.json'), copyFrom: join(ctx.fixtures, 'skillevaluator', 'tier2_passed.json') }] };
  }
  if (a[0] === 'tier3' && a.includes('--results-dir')) {
    const out = join(after(a, '--results-dir'), 'calculator', '20261017_091635_7_46d5ccd742f5');
    const job = '20261017-051633-16fc9065';
    const stdout = JSON.stringify({ job, job_dir: `/home/user/.local/share/skilleval-mcp/jobs/cli/${job}`, container_exit: 0,
      summary: { skill: 'calculator', tiers: { 3: { execution_status: 'succeeded', scored_attempts: 4, expected_attempts: 4 } } } }, null, 2);
    return { code: 0, stdout: `${stdout}\n`, stderr: `launch: job ${job} (calculator, tier3)\n`, files: [
      { path: join(out, 'result.json'), copyFrom: join(ctx.fixtures, 'skillevaluator', 'tier3_result.json') },
      { path: join(out, 'report.html'), content: '<!doctype html><title>report</title>\n' },
    ] };
  }
  return null;
}
