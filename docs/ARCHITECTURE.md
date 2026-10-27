# Nemo Console — architecture & contract (v1)

Monitor, configure and interact with **NeMo Switchyard** (tier routing), **NeMo Relay** (agent gateway + observability)
and **NVIDIA Dynamo** (serving frontend + workers) from one local UI. Contract v1 — LOCKED. Changes after fan-out are
additive only, appended to §9 and relayed to affected agents immediately.

## 1. Decisions (ADR summary; full text in `docs/adr/`)

| # | Decision | Why |
|---|---|---|
| 1 | Hexagonal core + `SystemAdapter` port (`adapters/base.py`, LOCKED); one adapter per system | New system = one adapter; the UI renders from descriptors |
| 2 | Host-run FastAPI on `127.0.0.1:8793`, not a container | Controls rootless Docker + a systemd user unit; mounting the socket into a network service is root-equivalent |
| 3 | All process control through one `CommandRunner` (argv list, binary allowlist `docker`/`systemctl`/`nemo-relay`/`skillevaluator`/`skillevaluator-local` — extended by 9.7 and 9.8 —, timeout, output cap, redaction) | One audited choke point; adapters cannot shell out |
| 4 | Config files: core does compare-and-swap (sha256), backup, atomic write, audit; adapter only validates + restarts | The dangerous IO exists once |
| 5 | Config apply = validate → diff → confirm → backup → write → restart → health-check → **auto-rollback on unhealthy** | A bad routes.toml must not leave the router down |
| 6 | Validators that are only a proxy say so (`authoritative=false`) | Relay's plugin schema differs from `switchyard-server`'s (measured; see clients/nemo-relay/routes.toml header) |
| 7 | Trust/policy files (`~/.config/nemo-relay/plugins.toml`, `config.toml`) are **read-only** in v1 | They hold the signature-enforcement key and attestation mode; editing is a human security decision (AGENTS.md rule 4) |
| 8 | Dynamo tunables = structured param catalog → generated compose **override** in the console state dir; repo compose never edited | Verified against the pinned image `--help`; reversible by deleting one file |
| 9 | Dynamo lifecycle by `docker compose` directly, **never `scripts/use-model.sh`** | use-model.sh also stops the other card's engine (measured 2026-09-23). Preflight names what shares the card and requires explicit confirm |
| 10 | Events carry **metadata only** (route, tier, tokens, latency, event type) — never prompt/completion text | ATOF/ATIF and routing logs can contain conversation content |
| 11 | Playground chat is proxied, streamed, **never persisted**; credential resolved server-side by env NAME | Secrets never touch disk or the browser |
| 12 | SQLite (WAL) for metrics rollups, config revisions, audit, settings; SSE for live streams | Single node, zero ops |
| 13 | Zero-build, zero-npm frontend (ES modules + web components, SVG charts), strict CSP | No supply-chain surface (same as Engine Console) |
| 14 | Security baseline = a localhost-service hardening checklist (ADR-0014), applied in full | Loopback is not a trust boundary for a browser |
| 15 | NeMo Guardrails rails configs are editable, but a candidate that shrinks the rails is refused unless it carries `# nemo-console: allow-weaken-rails` (ADR-0015, exception to 0007) | Rails tuning is routine; silent weakening is not |

## 2. What each system exposes (measured 2026-09-24 unless noted)

| System | Endpoint / artifact | Notes |
|---|---|---|
| Switchyard 0.3.0 | `http://127.0.0.1:4010` `/health` `/v1/models` `/v1/stats` (in-memory, resets on restart) `/v1/stats/reset` `/v1/routing/session-stats?session_id=` `/metrics` (Prometheus, `switchyard_*`) `/v1/chat/completions` `/v1/messages`; durable routing log `routing.jsonl` inside container volume `switchyard-log` at `/var/log/switchyard/`; `--dry-run` validates a config | `forward_auth`: callers send their own bearer; `/health` `/metrics` `/v1/models` are open. Container `ai-lab-switchyard-switchyard-1`, compose `inference/switchyard/`. Config: `inference/switchyard/routes.toml` |
| Relay 0.9.2 | persistent gateway `http://127.0.0.1:4040` (systemd user unit `nemo-relay-gateway.service`; `/health` is 404, `/v1/*` needs a bearer); CLI `nemo-relay doctor --offline`, `agents --json`, `plugins validate`; files `~/.config/nemo-relay/{config,plugins}.toml`, ATOF `~/.local/share/nemo-relay/atof/`, ATIF `~/.local/share/nemo-relay/atif/`; routes for the native plugin `clients/nemo-relay/routes.toml` | Gateway takes no caller bearer for clients but holds a scoped LiteLLM virtual key (`ops/nemo-relay-gateway.env` — the console NEVER reads it) |
| Dynamo 1.5.0 | frontend `http://127.0.0.1:8020` (`/health` `/v1/models` `/metrics` `/v1/chat/completions`), worker metrics on the ai-lab bus `:8081`, containers `ai-lab-dynamo-dynamo-{frontend,worker}-1`, opt-in `--profile mock` mocker; compose `inference/dynamo/`; **shares the Workstation card with `vllm-qwen38`** | Cold start ~4 min. Adapters must handle "down" and "loading" |

## 3. Layout — one owner per path

```
clients/nemo-console/                       (its own git repo, ignored by the parent)
  Makefile README.md LICENSE .gitignore     orchestrator (README finalised by docs agent)
  docs/ARCHITECTURE.md                      orchestrator (this file, LOCKED)
  docs/adr/  docs/THREAT-MODEL.md docs/OPERATIONS.md   docs-adr-writer
  deploy/                                   orchestrator (systemd user unit template, opt-in)
  backend/pyproject.toml uv.lock            orchestrator
  backend/src/nemo_console/
    domain/models.py                        orchestrator  LOCKED
    adapters/base.py __init__.py            orchestrator  LOCKED
    adapters/switchyard.py                  engine-adapter-specialist #1
    adapters/relay.py                       engine-adapter-specialist #2
    adapters/dynamo.py                      engine-adapter-specialist #3
    config.py container.py main.py          fastapi-core-engineer
    api/  services/  migrations/  security  fastapi-core-engineer
  backend/tests/core/                       fastapi-core-engineer
  backend/tests/switchyard/ relay/ dynamo/  the matching adapter agent (fixtures = captured real output)
  frontend/                                 zero-build-frontend-engineer (incl. dev/mock-server against §4)
```
No agent edits a path it does not own. Need a change elsewhere → report it, don't do it.

## 4. HTTP API (all under `/api/v1`, JSON, bodies ≤ 1 MiB)

Auth per §6. `viewer` may GET; everything else needs `admin`. Every non-GET needs header `X-Console: 1`.

| Method & path | Returns / does |
|---|---|
| `GET /health` | `{status, version, trust: "loopback-admin"\|"bearer", systems: {id: Health}}` |
| `GET /overview` | `{systems: SystemStatus[], topology: Topology, ts}` |
| `GET /topology` | `Topology` (nodes + edges, health from the adapters' statuses + litellm/engine probes) |
| `GET /systems` | `[{id,label,health}]` |
| `GET /systems/{id}/status` | `SystemStatus` |
| `GET /systems/{id}/metrics` | `MetricsSnapshot` (live) |
| `GET /systems/{id}/metrics/history?window=15m\|1h\|24h&key=` | `{key, points:[[ts,value]]}` (ring buffer + SQLite rollup) |
| `GET /systems/{id}/events?limit=` | `SystemEvent[]` |
| `GET /systems/{id}/events/stream` | SSE of `SystemEvent`; ≤ 8 concurrent, idle timeout 5 min |
| `GET /systems/{id}/config` | `ConfigFileSpec[]` |
| `GET /systems/{id}/config/{file}` | `ConfigFile` |
| `POST /systems/{id}/config/{file}/validate` | body `{content}` → `ValidationResult` (never writes) |
| `POST /systems/{id}/config/{file}/apply` | body `{content, base_sha256, restart, confirm:true}` → `ApplyResult`. 409 if `base_sha256` is stale; 403 if not editable; auto-rollback on unhealthy |
| `GET /systems/{id}/config/{file}/history` | `ConfigRevision[]` |
| `POST /systems/{id}/config/{file}/rollback` | body `{revision_id, confirm:true}` → `ApplyResult` |
| `GET /systems/{id}/params` | `{catalog: ParamSpec[], values: {}}` (empty catalog when unsupported) |
| `POST /systems/{id}/params/preview` | body `{values}` → `{rendered: str, warnings: str[]}` |
| `PUT /systems/{id}/params` | body `{values, confirm:true}` → `ApplyResult` |
| `GET /systems/{id}/actions` | `ActionSpec[]` |
| `POST /systems/{id}/actions/{action}` | body `{params, confirm}` → `ActionResult`; `disruptive`/`destructive` need `confirm:true` |
| `GET /systems/{id}/playground` | `PlaygroundSpec \| null` |
| `POST /systems/{id}/playground/chat` | body `{model, messages, max_tokens?≤4096, stream}` → SSE passthrough; response adds header `X-Served-By-Route`/tier when the system reports it. Not persisted |
| `GET /routing/summary?window=` | Switchyard-derived: `{by_route, by_model, by_tier, tokens, requests, errors}` from the durable routing log |
| `GET /audit?limit=&system=` | admin-only, append-only record of every mutating call, params redacted |
| `GET /settings` `PUT /settings` | urls, state-dir, intervals; token status as `[set, N chars]` only |

Errors: `{error:{code,message}}` with 400/401/403/404/409/413/421/422/429/502/503. A down system is **200 with health=down**, not a 5xx.

## 5. Canonical metric keys (adapters map from `/metrics`, `/v1/stats`, files; omit what cannot be read)

`switchyard`: `sy.requests_total` `sy.errors_total` `sy.in_flight` `sy.tokens{kind=prompt|completion|reasoning|cached}` `sy.model.calls{model}` `sy.route.calls{route}` `sy.latency_ms{q=p50|p95}` (only if a histogram exists)
`relay`: `relay.gateway_up` (0/1) `relay.plugin_loaded` `relay.atof_files` `relay.atof_bytes` `relay.atif_trajectories` `relay.exporters_enabled`
`dynamo`: `dyn.up` `dyn.models` `dyn.requests_total` `dyn.in_flight` `dyn.ttft_ms{q}` `dyn.itl_ms{q}` `dyn.tokens{kind}` `dyn.kv_cache_usage` `dyn.worker_up`
Prefix is the adapter's; the core stores whatever it returns (bounded label cardinality).

## 6. Security baseline (non-negotiable; see `harden-localhost-service`)

* Bind `127.0.0.1` only. `Host` allowlist (421), same-origin-or-absent `Origin` on unsafe methods (403), `X-Console: 1` on non-GET, no CORS.
* Bearer key (generated first start, stored hashed, 0600 `fchmod`) required off-loopback; roles admin/viewer; role dependency on **every** router + a test that enumerates OpenAPI routes.
* Headers on ALL responses (incl. errors/static): strict CSP (`default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; frame-ancestors 'none'`), `nosniff`, `Referrer-Policy: no-referrer`, `X-Frame-Options: DENY`, `Permissions-Policy`, `Cache-Control: no-store` on `/api`. No Swagger/ReDoc. Static allowlist only.
* argv lists only; identifiers validated with `fullmatch`; container ops label/name-verified via `inspect` before stop/rm/logs; fixed literals for every docker flag.
* Config writes: allowlisted absolute paths fixed at startup, not symlinks, size cap 256 KiB, refuse content that looks like a secret (`sk-…`, `Bearer …`, `api_key = "<literal>"`; env-var NAMES are fine).
* Secrets by env-var NAME; never returned, logged, audited or put in argv. `[set, N chars]` only.
* Forbidden reads: `~/.ssh ~/.aws ~/.config/gcloud ~/.gnupg ~/.netrc`, `ops/*.env`, any `.env`. The console never opens `ops/nemo-relay-gateway.env`.
* Bounds: body caps incl. chunked, SSE connection cap + idle timeout, playground `max_tokens` clamp, log/file read caps, bounded label cardinality.
* No outbound network except configured loopback / `ai-lab` targets. No external hosts, no CDN.
* Observability: OTLP to `localhost:4317`, `/metrics` for the console itself, structured JSON logs.

## 7. Actions (`ActionSpec`; danger levels)

`switchyard`: `restart` (disruptive) · `reset_stats` (destructive: zeroes in-memory counters) · `dry_run_config` (safe)
`relay`: `gateway_restart` (disruptive) · `gateway_status` (safe) · `doctor_offline` (safe) · `agents_list` (safe) · `plugins_validate` (safe)
`dynamo`: `up` / `down` / `restart` (disruptive) · `up_mock` (safe, CPU only) · `preflight` (safe): reports what shares the Workstation card
  (`ai-lab-vllm-qwen38-vllm-1`) and that `down`/`up` of Dynamo **does not** touch other cards. `up` while the sharing engine is running is refused
  unless params carry `stop_sharing_engine=true`, which stops exactly that one container (name-verified) and nothing else. It also notes
  that router aliases `qwen3.8-27b-1m` and the Switchyard capable tier go dark while Dynamo holds the card.

## 8. Quality bar / gates

Backend: `uv sync --frozen`, `ruff check`, `mypy --strict src`, `pytest -q` fully offline (fakes for runner + http; adapter fixtures are **captured real output**, not invented).
Frontend: `node --test tests/*.test.mjs` + headless-Firefox screenshots of every view (see `headless-ui-verify`), and since
addendum 9.10 the browser suite `frontend/e2e/run.mjs` (`make e2e`).
Ship gate: literal clean clone outside the tree, all of the above, added-lines leak scan, then CI. Every agent reports what it did **not** verify.
Docs mark each claim implemented / verified-live / unverified.

## 9. Addenda (additive only)

**9.1 (2026-09-24, orchestrator).** Dynamo is LIVE for the whole build: frontend `ai-lab-dynamo-dynamo-frontend-1` (:8020, healthy) and worker
`ai-lab-dynamo-dynamo-worker-1` serving `unsloth/Qwen3.8-27B-NVFP4`. At the user's request `vllm-qwen38` was stopped (swapped by hand, `sglang-flashnext`
untouched), so `qwen3.8-27b-1m` and the Switchyard capable tier are dark. Dynamo adapter fixtures are captured from this live stack.
**Agents must NOT run disruptive actions (up/down/restart/stop/recreate) against the live stack while building** — read-only probes only
(`curl`, `docker inspect/logs/ps`, `--help`). The orchestrator performs live action verification once, after the build. Tests use fakes.

**9.2 (2026-09-24, orchestrator).** Default port is **8793** (was 8792; Agent Hub `hubctl` owns 8792, something else holds 8795). Overridable via `NEMO_CONSOLE_PORT`.
Core-defined `ctx.paths` keys (adapters accept these): `repo state switchyard_dir switchyard_routes switchyard_compose relay_routes relay_dir relay_config relay_plugins relay_atof_dir relay_atif_dir dynamo_dir dynamo_compose`;
`ctx.urls`: `switchyard relay dynamo dynamo_worker_metrics litellm`; `ctx.secret_env`: logical name -> env var NAME (default `switchyard` -> `LITELLM_API_KEY`).
Core rules: `X-Console: 1` required on every non-GET even with a bearer; a validator returning ok=false blocks apply (422) even when authoritative=false;
core computes the config sha256 from raw bytes; `atomic_write` replaces the inode, so an adapter's `post_apply` must RECREATE (not just restart) a container that bind-mounts the file.
Dynamo "loading" = `health=degraded` with summary starting "loading" (no new Component state).

**9.3 (2026-09-24, orchestrator, after the independent audit).** Supersedes the earlier text where they differ:
* §6 auth: `trust_loopback` now defaults to **False**. Unauthenticated callers are role `public` (401 on everything but `/health` and static files); the UI prompts for a bearer. Trust is forced off when `host` is non-loopback or `allowed_hosts` is set.
* §4 `/health` gains additive `role` (`admin|viewer|public`) and `trust_notice`. `/settings` shows `state_dir` `~`-relative to non-admins.
* §6 audit: only authenticated, authorised mutating calls are recorded; denials are counted (`nemo_console_denied_requests_total`) and kept in a 100-entry ring.
* §1 #5/#4: rollback re-runs validation and the secret scan on the target revision. §1 #8: the hand-edited Dynamo override is a closed allowlist.
* Route validators (Switchyard, Relay) ERROR on a `base_url` host that is neither loopback nor a bare single-label service name, and on userinfo.
* §1 #10 deviation: Dynamo events are redacted log lines, not metadata-only (ADR-0010).
* §7 Dynamo `preflight` output ends with `shares_card:` / `router_aliases_dark:` lines. New core problem `switchyard.tier.capable_dark` (setting `engine_probes`). Default action timeout 900 s; `action_timeouts` overrides per action (`dynamo.up`).

**9.4 (2026-10-06): Dynamo is opt-in; the card is shared with the default model, not with `vllm-qwen38` alone.** Supersedes §2 (Dynamo
row: "shares the Workstation card with `vllm-qwen38`") and §7 where they differ. Premise change on the author's deployment: since
2026-09-26 the default model is `qwen3.8-flash-next` (SGLang, tensor-parallel 2, ~60 GB of BOTH cards) plus the always-on
`nemotron-lightning` (~29 GB of the Workstation card); Dynamo (worker 0.5 x the Workstation card) and the 27B are opt-in and off.
* `Health` gains `"off"` (additive Literal value): opt-in and stopped on purpose. Dynamo reports it when the setting `dynamo_opt_in`
  (default true) is on and NO frontend, worker or mocker container is running or restarting, with one `info` problem `dynamo.off`
  whose fix is `dynamo_start`. Anything running or restarting (including a crash-looping worker) keeps the error-level report.
  `/health` `status` ignores systems that are `off`. The UI renders `off` as a neutral badge (○, dashed node/tile), never red.
* `_state` reads `Restarting` before `Running` (Docker sets both during a restart back-off), so a crash loop is `failed`, not `running`.
* Card model = settings (`config.py`, `DEFAULT_DYNAMO_*`): `dynamo_exclusive_engines` (cannot run beside the worker; default the
  flashnext and vllm-qwen38 containers), `dynamo_coexisting_engines` (allowed; default lightning, ollama), `dynamo_ignored_containers`
  (monitoring; default the dcgm-exporter). Any other running container on the worker's GPU UUID is "unknown" and blocks `up` too.
  Handed to the adapter as a `CardPolicy` by the container (`_configure_dynamo`; injected adapters keep their own).
* `dynamo.card.contended` = worker running or restarting AND an exclusive engine running on the worker's card (GPU UUID or all
  GPUs); fix: the console `down`. `dynamo.holds_card` = worker running and no exclusive engine running; message = the
  `dynamo_dark_while_held` list, fix = `default_model_start`. Every start fix (`dynamo.frontend.down`, `dynamo.worker.absent`,
  `dynamo.off`, `dynamo.up.exclusive_engine`) is `dynamo_start`; no bare `docker compose up -d` remains. Both commands are DISPLAY
  ONLY: the console never runs `scripts/use-model.sh` (ADR-0009 stands for what the console executes).
* `up`: refused with `dynamo.up.exclusive_engine` (fix `dynamo_start`) while a non-stoppable exclusive engine runs, even with
  `stop_sharing_engine=true`; refused with `dynamo.up.card_in_use` naming any unknown container (fix: identify it; never
  `docker stop`). Coexisting and ignored containers do not block. `stop_sharing_engine` stops only the fixed `STOPPABLE_ENGINE`
  (the 27B, label-verified, compose `down` in its own project), never the default model.
* Preflight tail: `shares_card:` (one line per exclusive engine pinned to the card, running or stopped, and per unknown running
  container), NEW `stoppable:` (the one container the param may stop, or `none`), `router_aliases_dark:` (`none` or the dark list).
  New preflight problem `dynamo.card.unknown`.
* Topology: `vllm-qwen38` and Dynamo stopped = `off`; new node `vllm-lightning` (efficient tier); the clients -> Switchyard edge is
  labelled `default` (opencode's default model is a Switchyard route) and is active while Switchyard is up or degraded.
* Switchyard `/v1/stats` `models[*].calls` counts SUCCESSES (calls + errors = attempts). `switchyard.capable_tier.upstream_errors`:
  errors >= successes (and > 0), or errors rose since the last poll while successes did not.

**9.5 (2026-10-06): a fourth system, `skillevaluator` (NVIDIA SkillEvaluator).** Additive; nothing above changes.
* It is a CLI, not a service: `SystemId` gains `"skillevaluator"`, adapter `adapters/skillevaluator.py`. Status = launcher found
  (`skillevaluator-local` if on PATH, else `skillevaluator`; `--version` cached 300 s), the router's `/health/liveliness`, and the
  newest stored run. Health: `down` when no launcher, `degraded` when the router is down, else `up`.
* Runner: both launcher names join the bare-name allowlist. `SKILL_EVAL_*` / `SKILLEVAL_*` (+ `LOCAL_AI_LAB`, `LITELLM_URL`) are passed
  to those two binaries ONLY. With the wrapper the console holds no provider key at all (the wrapper reads its own 0600 key file).
* `ctx.paths` gains `skills_dir` = `<repo>/.claude/skills` (one directory per skill with a `SKILL.md`; symlinks and other names are
  never offered). Actions: `evaluate` (skill enum × tiers `static` | `tier1` | `tier1+2`), `evaluate_all_static`, `report`, `doctor`;
  all `safe`, admin. Tier 3 is not offered: its stock sandboxes run with `network_mode = "public"`.
* Verdict = the report JSON's `overall_status`, never the exit code (1 for failed AND incomplete, reports still written).
  Runs live in `<state>/skillevaluator/runs/<UTC stamp>-<skill>/` (0700; `run.json` 0600, metadata + redacted error text only),
  newest 200 kept. Events: kind `skilleval.run`, one per run. Metric keys: `se.skills`, `se.runs`, `se.last_quality_score`,
  `se.last_judge_score`, `se.runs_by_verdict{verdict=passed|failed|incomplete|error}`. No config files (configuration is the launcher's environment).

**9.6 (2026-10-17): SkillEvaluator Tier 3; the Dynamo compose files in use; the engine layout of 2026-10-17.** Supersedes
§7 and 9.4/9.5 where they differ. Premise change on the author's deployment: the resident layout is Nemotron 3.5 Super
VL on Dynamo (started from `inference/dynamo/docker-compose.yml` + `docker-compose.super-vl.yml`, worker on BOTH cards),
`nemotron-lightning` (vLLM TP2, both cards) and `chatterbox-tts` (Workstation card). The flashnext and 27B containers no
longer exist there; Bolt and GLM (`lil-glm53-tp2`) are opt-in and exclusive with Super VL.
* SkillEvaluator action `evaluate_live` ("Tier 3: live agent evaluation"), danger `disruptive` (it spends tokens and runs
  an agent, so it needs `confirm:true`), admin. Params: `skill` (the evaluate enum), `attempts` int 1-5 (default 1),
  `agent_model` and `judge_model` strings (default `nemotron-lightning` / `nemotron-lightning-judge`; adapter-checked
  against the playground's model-id shape), `dataset` enum `auto|existing|generate`, `skip_baseline` bool. Only with the
  `skillevaluator-local` launcher (its runner container is the containment); plain `skillevaluator` is refused with
  `skillevaluator.live.needs_wrapper`. argv (no shell): `skillevaluator-local tier3 <resolved skill dir> --n-attempts N
  --agent-model opencode=<agent_model> --judge-model <judge_model> --dataset <d> [--skip-baseline] --results-dir <run dir>
  --quiet` (`--quiet` keeps stdout to the final summary JSON so an hour of progress lines cannot trip the output cap).
* It never runs inside the HTTP request: the action writes `<run dir>/run.json` (`verdict: "running"`) and starts ONE
  asyncio task owned by the adapter, then returns ok with the run id. A second request while it runs is refused
  (`skillevaluator.live.busy`). On completion the task writes the verdict (result.json `execution_status`:
  `succeeded|failed|unknown|skipped|not_applicable`; `error` when no result.json), Skill Lift per agent
  (`agents.X.lift.overall.delta` plus the per-metric deltas), scored/expected attempts, duration, and at most 8
  conclusions/recommendations (redacted, capped; kept out of event data). result.json is read no-follow, regular file,
  8 MiB cap. Cancelled (console stopping): `verdict: "interrupted"`. A run.json at `running` that no task of this process
  owns (the console restarted) READS as `interrupted`; the report then also shows a result.json that landed later.
* Runner: the cap stays 600 s except `skillevaluator`/`skillevaluator-local` with subcommand `tier3`: 4800 s
  (`MAX_LIVE_EVAL_TIMEOUT`; the launcher stops a job by 120 + 900 + 3600 s). On a timeout or a cancellation the runner
  stops the child's whole process group; for the two SkillEvaluator launchers it sends SIGINT first and waits up to 5 s
  (the lab launcher `docker kill`s its job container on SIGINT), then SIGKILL. Allowlist unchanged.
* Metrics gain `se.live_running` (0/1, emitted once any run exists) and `se.last_skill_lift`; `se.runs_by_verdict` gains
  `succeeded` and `interrupted`. Events: a live run is one `skilleval.run` event per (run, verdict), so its start and its
  end both stream. Problem `skillevaluator.tier2.no_embeddings` (warn; fix: the lab's `inference/ollama` compose up, else
  "start the embeddings model ...") when a `tier1+2` run's Tier 2 lacked embeddings. The install fix is the lab's pinned
  `clients/skillevaluator/install.sh` when the repo has it, else `uv tool install ... @7304d76...` (v0.5.0), never unpinned.
* Dynamo: up/down/restart/up_mock/`config -q`/Params use exactly the compose files of the running containers' label
  `com.docker.compose.project.config_files` (project label `ai-lab-dynamo` only), plus the console override only on the
  plain base. Only with no Dynamo container does the configured base apply. The label is untrusted: absolute paths,
  each resolving (symlinks followed) to a `.yml`/`.yaml` regular file inside the configured compose directory or to the
  console's own override, at most 8, `working_dir` equal to that directory, and all Dynamo containers agreeing; else
  lifecycle actions and Params are refused (`dynamo.compose.untrusted`), never answered with the base file.
* With any repo override file in use (`dynamo.compose.override`, info): Params apply, the console override's validation
  (so its apply and rollback) and `up_mock` are refused with the reason; start fixes become the console's own `up`;
  `dynamo.structural_tag.off` / `parser.dialect` / `router.kv` are not raised (measured on the base model, fixed via
  Params). The version is the first Dynamo image tag (worker, then `ai-lab-dynamo-dynamo-sv-frontend-1`, then the
  frontend); a digest or a non-Dynamo image (the nginx gateway) is never a version. The sv-frontend is a component and a
  log source (component `frontend`; a non-Dynamo `dynamo-frontend` logs as `gateway`). The worker's GPUs are every
  `device_ids` entry of `dynamo-worker` in `docker compose <files> config --format json` (cached per file set + mtimes,
  300 s), else the base file (base stack only), else the container's DeviceRequests; sharers are checked on ANY of them.
  `dynamo.holds_card` is info, raised only when `dynamo_dark_while_held` is non-empty.
* Defaults (all settings): `dynamo_exclusive_engines` Bolt, GLM, flashnext, 27B; `dynamo_coexisting_engines` lightning,
  ollama, chatterbox; `default_model_start` is the switch script WITHOUT an alias (it only lists engines);
  `engine_probes` add `nemotron-3.5-super-vl` (the Dynamo worker), `nemotron-3.5-bolt`, `glm-5.3-flash-next-local`;
  every `engine_start` command says what it stops. `EngineProbe` gains state `absent` (no such container): a capable
  target that is absent is `switchyard.tier.capable_not_deployed` (info, does not degrade Switchyard); only a stopped one
  is `capable_dark`. New setting `topology_engines` (list of `{id,label,container,opt_in}`, replaces the default) drives
  the engine nodes; an absent container is drawn `off` with "(not deployed)".
* Playgrounds: Switchyard and Relay default to route `super-lightning` when it exists (what LiteLLM's `switchyard` alias
  forwards to), else the first route. `playground_key_env` defaults to `LITELLM_API_KEY` for `relay` as well.
* Relay ATOF: gzipped rotations (`<file>.N.gz`) are read for `recent_events` (no-follow, regular, 64 MiB compressed and
  64 MiB decompressed caps; over a cap the file is skipped); `follow_events` tails plain files only.

**9.7 (2026-10-23): a fifth system, `datadesigner` (NVIDIA NeMo Data Designer, the CLI as a managed system).** Additive;
nothing above changes. Contract: feature "datadesigner" (2026-10-23).
* `SystemId` gains `"datadesigner"`; the adapter is `adapters/datadesigner.py` (`DataDesignerAdapter`, label
  "Data Designer"), registered in `adapters/__init__.py`. It follows the SkillEvaluator adapter structure: `_run`
  wrapper (runner exceptions → `CommandResult(127)`), TTL-cached launcher probe (`data-designer-local --version`,
  300 s), run dirs `<state_dir>/datadesigner/runs/<run_id>/run.json` (0700 dirs, 0600 run.json), `follow_events`
  polling 2 s keyed (run_id, verdict), `KEEP_RUNS=100`, redaction everywhere, failure is data.
* Runner: allowlist gains `data-designer-local` and `data-designer-export`. ONLY `data-designer-local create ...`
  gets the long cap `DD_CREATE = 3600 s` (`max_timeout`); preview/validate/check-models/export stay at 600 s.
  `data-designer-local` joins the SIGINT-first graceful stop set (the wrapper cleans up its own children). The
  wrapper needs NO env of ours: it injects its own credentials from its own 0600 key files (the console never sees
  a key), so no env prefixes are forwarded to it.
* Settings/paths: `dd_scripts_dir` (default `~/Datasets/data-designer/scripts`), `dd_datasets_root` (default
  `~/Datasets`) — both created 0700 at startup (`ensure_user_dirs`, lifespan + `run()`); `dd_config_dir`
  (`~/.data-designer`) is the TOOL's own dir and is never created by the console (the adapter reports
  `dd.no_config_dir` when missing). `dd_key_file` (`~/.config/data-designer/litellm.key`) is checked for
  EXISTENCE only (`dd.no_key` warn, fix: `clients/data-designer/mint-key.sh`); its contents are never read.
* status(): launcher component (version or "absent", health down when absent), router component
  (`GET <litellm>/health/liveliness`, degraded when down), a live create component while one runs. Problems:
  `dd.not_installed` (error; fix: `clients/data-designer/install.sh` by repo path when available, else the
  pinned-command text), `dd.router_down` (warn), `dd.no_scripts` (warn: dir missing or empty), `dd.no_key` (warn),
  `dd.no_config_dir` (warn). The tool's `model_providers.yaml` / `model_configs.yaml` are parsed read-only with a
  never-raising reader (missing/broken → zero entries); `api_key` fields are NEVER surfaced. Endpoints:
  `router (generation)` (bearer). Metrics: `dd.scripts`, `dd.runs`, `dd.live_running`, `dd.last_records`,
  `dd.runs_by_verdict{verdict=succeeded|failed|error|interrupted}`. Events: one `dd.run` per (run, verdict),
  metadata only (script, mode, verdict, num_records, dataset_name, destination, format, duration_ms).
* Configure: one ConfigFileSpec per script file in `scripts_dir` (id = filename; cap 200; regular files, no
  symlinks; extension allowlist .py/.yaml/.yml/.json and, because the core's `valid_id` caps ids at 64 lowercase
  chars, the allowlist regex is the intersection: `[a-z0-9][a-z0-9._-]{0,52}\.(py|yaml|yml|json)`). The core's
  CAS/backup/revision machinery applies unchanged. `validate_config` writes the candidate to
  `<state_dir>/datadesigner/tmp/<file_id>.<rand>` (deleted in `finally`), runs `data-designer-local validate`
  (540 s), and is authoritative; its note says that validation EXECUTES the script locally (the tool's design).
  `post_apply` is always healthy (nothing to restart).
* Actions: `preview` (disruptive — it executes the config script and spends router tokens, so it needs
  `confirm:true`; admin; `data-designer-local preview <script> -n N --save-results -o <run_dir>`; then the
  newest `preview_results_*`'s dataset.parquet is converted via `data-designer-export` and the first 2000 lines
  are kept as `<run_dir>/records.jsonl`), `create` (disruptive, ONE background run at a time, the SkillEvaluator
  Tier 3 `_LiveRun` pattern; `data-designer-local create <script> --num-records N --dataset-name NAME
  --artifact-path <run_dir> --no-tui`, 4 MiB stdout cap; afterwards metadata.json → parquet batches are converted
  to `<run_dir>/records.jsonl` (2000-line cap) and the user copy is written under `<datasets_root>/<subdir>`
  — parquet: the batch files copied; csv/jsonl: streams; json: jsonl lines framed into one array. All destination
  paths are resolved + containment-checked under the datasets root BEFORE the run starts. Cancel on console stop →
  verdict `interrupted`), `report` (safe; bounded text + a ≤5-row markdown table, cells ≤120 chars, redacted),
  Runner (2026-10-24, lab): every script-executing invocation runs INSIDE an OpenShell sandbox
  (one throwaway sandbox per run; Landlock FS — only the run dir writable, the baked tool config
  read-only; network default-deny, only the lab router's /v1/**; the router key held by the
  OpenShell gateway credential store and injected per-request — the wrapper is key-free). The
  wrapper's CLI surface is unchanged, so this adapter needed no behavioral change.
  `check_models` (disruptive — it executes the config script and makes real model calls, so it needs
  `confirm:true`; admin; `data-designer-local check-models <script>` — the CLI's own --help requires a config
  source argument, so the action takes the script enum), `show_config` (safe; read-only provider/alias dump).
* API (additive, viewer GET, headers/auth identical to the other observe endpoints):
  `GET /api/v1/systems/datadesigner/runs/{run_id}/records?limit=50&offset=0` → `{run_id, total, columns, rows,
  truncated}` — reads records.jsonl ONLY (never parquet); values pass through as-is, no string over 2 KiB
  (truncated, no marker fields); total from run.json's records when present, else the counted lines. A live create
  run answers an empty records page (not 404). `GET /api/v1/systems/datadesigner/runs/{run_id}/files` →
  `{run_id, files: [{path, bytes}]}` (rel paths, regular files only) — nothing is served in v1: the UI shows
  paths; the user opens them on disk. Other systems → 404 on both.

**9.8 (2026-10-24): five more systems — `openshell`, `dcgm`, `aiperf`, `guardrails`, `evaluator`.** Additive; nothing
above changes. Contract: feature "nvidia-five" (2026-10-24).
* **Core (orchestrator, done before fan-out):** `SystemId` gains the five ids; `adapters/__init__.py` registers
  `openshell.OpenShellAdapter` ("OpenShell"), `dcgm.DcgmAdapter` ("DCGM"), `aiperf.AiPerfAdapter` ("AIPerf"),
  `guardrails.GuardrailsAdapter` ("NeMo Guardrails"), `evaluator.EvaluatorAdapter` ("NeMo Evaluator"). Runner allowlist gains
  `openshell`, `aiperf-local`, `guardrails-local`, `evaluator-local`; ONLY `aiperf-local profile` (3600 s) and
  `evaluator-local run` (4800 s) get long caps; both wrappers are SIGINT-first. No env is forwarded to any of them: each lab
  wrapper reads its OWN 0600 router key file (the console never holds a key). `DEFAULT_URLS` gains `dcgm`
  (`http://127.0.0.1:9400`), `prometheus` (`http://127.0.0.1:9090`), `guardrails` (`http://127.0.0.1:8806`). `ctx.paths` gains
  `openshell_runner_dir|openshell_gateway_config|openshell_policy|openshell_compose`, `observability_dir|observability_compose|
  dcgm_counters`, `aiperf_dir|aiperf_profiles|aiperf_key_file`, `guardrails_dir|guardrails_compose|guardrails_config_dir`,
  `evaluator_dir|evaluator_suites|evaluator_key_file` (key files: EXISTENCE only). Settings gain `dcgm_exporter_container`,
  `dcgm_diag_image` (the FULL DCGM image, tag + digest — the exporter image has no diagnostic plugins), `dcgm_prometheus_jobs`,
  `openshell_containers`, `guardrails_container`. Every adapter follows the SkillEvaluator/Data Designer structure: `_run`
  wrapper (runner exceptions → `CommandResult(127)`), TTL-cached version probes, failure is data, redaction everywhere,
  bounded label cardinality, run dirs `<state_dir>/<system>/runs/<run_id>/run.json` (0700/0600) for CLI runs.
* **Metric key prefixes:** `os.` (openshell) · `dcgm.` · `ap.` (aiperf) · `gr.` (guardrails) · `ev.` (evaluator). Event kinds:
  `openshell.sandbox`, `dcgm.xid` / `dcgm.diag`, `aiperf.run`, `guardrails.check`, `evaluator.run`.
* **openshell** — the OpenShell gateway that contains Data Designer runs (`clients/data-designer/runner`). Status from
  `openshell status` / `--version` (ANSI stripped) + `docker inspect` of `openshell_containers`; the gateway is started on
  demand by `data-designer-local`, so stopped = `off`, not `down`. Security checks are problems, not prose: the sandbox
  network `openshell-docker` must be `--internal` (error if not). Config files `gateway.toml`, `policy.yaml` and the
  provider profile are READ-ONLY (ADR 0007: a web console never loosens a sandbox's trust policy); `openshell policy prove`
  is offered as an action instead. Actions include sandbox list/delete (delete = destructive) and gateway up/stop.
* **dcgm** — this host's dcgm-exporter (`urls.dcgm`) plus every DCGM host Prometheus scrapes (`urls.prometheus`,
  `dcgm_prometheus_jobs`; loopback only — the console never contacts a fabric address). Problem `dcgm.exporter.vulnerable`
  (error) when the running exporter image is older than 4.8.3 (CVE-2026-47483, `/debug/pprof`), and a direct probe of
  `/debug/pprof/` (must not answer 200). Config: `observability/dcgm/counters.csv` is EDITABLE (validator = field-name and
  type check against the field lists shipped in the exporter image; `authoritative=False`, say so); the exporter re-reads
  the file (directory mount), so `post_apply` health-checks `/metrics` and returns `unhealthy` (→ core rollback) if it stops
  answering or the configured families vanish; `restart=True` restarts the exporter container. Diagnostics run in a
  throwaway container from `dcgm_diag_image` (`--network none`, per GPU index — the two cards are different models and
  DCGM refuses a heterogeneous group): level 1 and level 2 both `disruptive` after the 2026-10-24 audit (a privileged
  container; level 2 loads the GPU for minutes), `discovery` `safe`; level 3 is not offered. The in-container command is a CONSTANT per level with only a validated integer GPU index substituted.
* **aiperf** — NVIDIA AIPerf (lab: `clients/aiperf/`, wrapper `aiperf-local`, own router key alias `aiperf`). Named load
  presets in `clients/aiperf/profiles.yaml` (EDITABLE; own schema, authoritative). `profile` action is `disruptive` (it
  loads a live engine and spends router capacity), one run at a time, artifacts in the run dir; metrics from the newest
  run (`ap.ttft_ms{q}`, `ap.itl_ms{q}`, `ap.output_tps`, `ap.request_tps`, `ap.runs`).
* **guardrails** — a NeMo Guardrails server (lab: `security/nemo-guardrails/`, container `guardrails_container`, loopback
  `:8806`, own router key alias `guardrails`, NO internet egress, no runtime model downloads). Rails configs under
  `security/nemo-guardrails/config/<id>/` are EDITABLE; validation loads the candidate with the pinned nemoguardrails in a
  `--network none` container (`guardrails-local validate`); `post_apply` restarts the server and checks the config is
  served, else `unhealthy` (→ rollback). Playground only if the server's chat endpoint accepts the core's OpenAI body;
  otherwise a `check` action sends fixed probe prompts and reports allowed/blocked.
* **evaluator** — NVIDIA NeMo Evaluator (`nel`; lab: `clients/nemo-evaluator/`, wrapper `evaluator-local`, own router key
  alias `nemo-evaluator`). Datasets are prefetched once by the human (`evaluator-local prefetch`, the only step with
  egress); runs are OFFLINE (`HF_*_OFFLINE=1`) against router aliases, only benchmarks that need no Docker sandbox and no
  judge. Named suites in `clients/nemo-evaluator/suites.yaml` (EDITABLE; own schema). `run` is `disruptive` (router
  tokens), one at a time; scores become `ev.score{bench,model}`.
* **Ownership (fan-out 2026-10-24):** one adapter agent per system owns `adapters/<system>.py`, `backend/tests/<system>/`
  and its lab directory (`clients/aiperf/`, `security/nemo-guardrails/`, `clients/nemo-evaluator/`; openshell and dcgm own
  no lab directory — `observability/dcgm/counters.csv` and the exporter bump are the orchestrator's). The frontend agent
  owns `frontend/` (one view per system, routes, i18n, mock data). Core files, docs and every other path: orchestrator.

**9.9 (2026-10-24): shutdown ends SSE streams.** Additive. On SIGTERM/SIGINT the server (`main.ConsoleServer`, a
`uvicorn.Server` subclass) calls `services.events.begin_shutdown()` before uvicorn's own exit handling: every open
event stream sends `event: shutdown` (`data: {}`) and ends, releasing its slot, so the process exits at once (a
browser's stream would otherwise hold it). `shutdown_grace_s` (setting, int, default 5, below `nemo-console stop`'s
10 s) is passed to uvicorn as `timeout_graceful_shutdown` as a backstop. Clients treat the end like any other stream
end and reconnect with backoff.

**9.10 (2026-10-24): frontend files are revalidated; a browser suite gates every push.** Additive. Every static
frontend response carries `Cache-Control: no-cache` and an ETag; a matching `If-None-Match` gets 304 (`main._file_response`).
API responses keep `no-store`. Reason: without the header browsers cached the ES modules heuristically and, after
0.3.0, paired new views with old shared modules (a missing export fails the whole import). `frontend/e2e/run.mjs`
(zero dependencies: Node 22's WebSocket speaks the DevTools protocol to a headless Chrome) starts the real backend
with a throwaway HOME, state and an empty repo root (what CI sees), signs in with the bootstrap key through `sessionStorage`, opens every route parsed
from `js/routes.js` and every `[role=tab]`, and fails on an error or not-found state, an uncaught exception, a
console error, an API 5xx, or a frontend file that does not load or lacks `no-cache`. A 404 for a config file is a
state ("not on this machine"), not an error, and an adapter's `ValueError` in a params preview is a 422. It runs in `make all`, in the CI job `e2e`, and in the git
`pre-push` hook through `scripts/e2e-gate.sh`, which records a pass as the git tree ids of `backend/` and `frontend/`
inside `.git` and refuses a pushed commit or tag without one.

**9.11 (2026-10-26): deployment identities and the repo layout are settings; an opt-in active capable-tier probe.**
Additive; defaults reproduce the earlier constants exactly.
* **Settings** (`config.py`, environment or the user env file only, read at start, never through the API):
  `switchyard_container`, `switchyard_compose_project`, `switchyard_compose_service`, `switchyard_image`,
  `switchyard_log_path`, `relay_unit`, `dynamo_compose_project`, `dynamo_services` and `dynamo_containers` (role ->
  name for `frontend`, `worker`, `mocker`, `sv_frontend`; merged over the defaults), `guardrails_compose_project`,
  `dcgm_compose_project`, and `layout` (key -> directory under `repo_root`; `resolved_paths()` and the repo-root marker
  follow it). Each value is validated with a full match (container, compose project, compose service, `.service` unit,
  absolute in-container path, relative plain-segment directory); an invalid value stops the start-up.
* **Hand-off:** as for the card policy (9.4) the container sets them on the registry-built adapters after
  construction (`_configure_switchyard`, `_configure_relay`, `_configure_dynamo` with a `DynamoNames` value,
  `_configure_guardrails`, `_configure_dcgm`); module constants stay the defaults, so injected adapters (tests) are
  unchanged. Lab helper hints (installers, key minters, the Ollama directory) look under the layout directory first.
* **Not settings, on purpose:** Dynamo's `STOPPABLE_*` (the one container `up` may stop; a setting must not widen what
  the console stops), OpenShell's sandbox network and router address (they must equal the lab wrapper's), the router
  liveness path.
* **Active probe** (`services/engines.CapableProbe`, setting `capable_probe`, default false): for each capable-tier alias
  whose container is running, one `POST {litellm}/v1/chat/completions` (`max_tokens` 1, `temperature` 0, LiteLLM
  `cache.no-cache`), credential from the env var NAMED by `capable_probe_key_env` (registered with the redactor; same
  naming rule as `playground_key_env`). Background task, one request at a time, at most once per
  `capable_probe_interval_s` (>= 300) per alias, `capable_probe_timeout_s` per request, one retry after
  `capable_probe_retry_s`. Status reads cached results only. Verdicts: ok, broken (timeout, transport error, HTTP >= 400
  other than 401/403, no `choices`), denied (401/403), unconfigured (env var unset: nothing is sent). New problem codes
  `switchyard.tier.capable_broken` (warn, degrades Switchyard, summary prefix "capable tier not answering"),
  `switchyard.tier.probe_unconfigured` and `switchyard.tier.probe_denied` (info). Only the verdict and a status are kept
  and logged.
