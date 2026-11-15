# Nemo Console — architecture & contract (v1)

> **0.4.0: the backend is Rust.** The contract below (API, settings, adapters, security controls) is unchanged; it was
> written for the Python backend of 0.1-0.3 and its file names map one to one: `adapters/<system>.py` ->
> `backend/crates/nemo-console/src/adapters/<system>.rs`, `api/routers/*.py` + `api/security.py` -> `api/*.rs`,
> `services/*.py` -> `services/*.rs`, `domain/models.py` -> `domain/models.rs`, `config.py` / `container.py` /
> `main.py` -> `config.rs` / `container.rs` / `main.rs` + `server.rs`; FastAPI + uvicorn -> axum + hyper on tokio,
> pydantic -> hand-written validators with pydantic's messages (`services/pyvalidate.rs`). The port's contract,
> its deviations and the parity evidence are in `backend/PORTING.md`; the decision is ADR 0016.

Monitor, configure and interact with **NeMo Switchyard** (tier routing), **NeMo Relay** (agent gateway + observability)
and **NVIDIA Dynamo** (serving frontend + workers) from one local UI. Contract v1 — LOCKED. Changes after fan-out are
additive only, appended to §9 and relayed to affected agents immediately.

## 1. Decisions (ADR summary; full text in `docs/adr/`)

| # | Decision | Why |
|---|---|---|
| 1 | Hexagonal core + `SystemAdapter` port (`adapters/base.rs`, LOCKED); one adapter per system | New system = one adapter; the UI renders from descriptors |
| 2 | Host-run server on `127.0.0.1:8793`, not a container (FastAPI to 0.3, a Rust binary since 0.4: ADR 0016) | Controls rootless Docker + a systemd user unit; mounting the socket into a network service is root-equivalent |
| 3 | All process control through one `CommandRunner` (argv list, binary allowlist `docker`/`systemctl`/`nemo-relay`/`skillevaluator`/`skillevaluator-local` — extended by 9.7 and 9.8 —, timeout, output cap, redaction) | One audited choke point; adapters cannot shell out |
| 4 | Config files: core does compare-and-swap (sha256), backup, atomic write, audit; adapter only validates + restarts | The dangerous IO exists once |
| 5 | Config apply = validate → diff → confirm → backup → write → restart → health-check → **auto-rollback on unhealthy** | A bad routes.toml must not leave the router down |
| 6 | Validators that are only a proxy say so (`authoritative=false`) | Relay's plugin schema differs from `switchyard-server`'s (measured; see clients/nemo-relay/routes.toml header) |
| 7 | Trust/policy files (`~/.config/nemo-relay/plugins.toml`, `config.toml`) are **read-only** in v1 | They hold the signature-enforcement key and attestation mode; editing is a human security decision (AGENTS.md rule 4) |
| 8 | Dynamo tunables = structured param catalog → generated compose **override** in the console state dir; repo compose never edited | Verified against the pinned image `--help`; reversible by deleting one file |
| 9 | Dynamo lifecycle by `docker compose` directly, **never `scripts/use-model.sh`** | use-model.sh also stops the other card's engine (measured 2026-09-23). Preflight names what shares the card and requires explicit confirm |
| 10 | Events carry **metadata only** (route, tier, tokens, latency, event type) — never prompt/completion text | ATOF/ATIF and routing logs can contain conversation content. The one exception is the Relay trace explorer's two admin-only content routes (addendum 9.12, ADR-0021) |
| 11 | Playground chat is proxied, streamed, **never persisted**; credential resolved server-side by env NAME | Secrets never touch disk or the browser |
| 12 | SQLite (WAL) for metrics rollups, config revisions, audit, settings; SSE for live streams | Single node, zero ops |
| 13 | Zero-build frontend with zero runtime npm dependencies (ES modules + web components, SVG charts), strict CSP | No supply-chain surface (same as Engine Console) |
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
(0.1-0.3, the Python build; since 0.4 the backend is `backend/` = a Cargo workspace, see backend/PORTING.md §3)
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

Backend (0.4+): `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test --locked` fully offline (fakes for runner + http; adapter fixtures are **captured real output**, not invented), `cargo deny`, `cargo audit`, the crate-age cooldown, and the parity harness against the last Python backend (`make parity`). (0.1-0.3: `uv sync --frozen`, `ruff`, `mypy --strict`, `pytest -q`.)
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
* Card model = settings (`config.rs`, `DEFAULT_DYNAMO_*`): `dynamo_exclusive_engines` (cannot run beside the worker; default the
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
* It is a CLI, not a service: `SystemId` gains `"skillevaluator"`, adapter `adapters/skillevaluator.rs`. Status = launcher found
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
  background task owned by the adapter (asyncio in 0.3, tokio in 0.4), then returns ok with the run id. A second request while it runs is refused
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
* `SystemId` gains `"datadesigner"`; the adapter is `adapters/datadesigner.rs` (`DataDesignerAdapter`, label
  "Data Designer"), registered in `adapters/mod.rs`. It follows the SkillEvaluator adapter structure: `_run`
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
  `GET /api/v1/systems/datadesigner/runs/{run_id}/records?limit=50&offset=0` (admin-only since 9.13) → `{run_id, total, columns, rows,
  truncated}` — reads records.jsonl ONLY (never parquet); values pass through as-is, no string over 2 KiB
  (truncated, no marker fields); total from run.json's records when present, else the counted lines. A live create
  run answers an empty records page (not 404). `GET /api/v1/systems/datadesigner/runs/{run_id}/files` →
  `{run_id, files: [{path, bytes}]}` (rel paths, regular files only) — nothing is served in v1: the UI shows
  paths; the user opens them on disk. Other systems → 404 on both.

**9.8 (2026-10-24): five more systems — `openshell`, `dcgm`, `aiperf`, `guardrails`, `evaluator`.** Additive; nothing
above changes. Contract: feature "nvidia-five" (2026-10-24).
* **Core (orchestrator, done before fan-out):** `SystemId` gains the five ids; `adapters/mod.rs` registers
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

**10.9 (2026-10-29): one more system — `modelopt`.** Additive; nothing above changes. Contract: ADR-0017.
* **Core:** `SystemId` gains `modelopt`; `adapters/mod.rs` registers `modelopt.ModelOptAdapter` ("Model Optimizer").
  Runner allowlist gains `modelopt-local` (ONLY `modelopt-local run` gets the 600 s cap; the wrapper returns at once, so the
  cap only bounds the start). `ctx.paths` gains `modelopt_dir|modelopt_recipes`; `DEFAULT_LAYOUT` gains
  `("modelopt", "clients/modelopt")`. No env is forwarded and no key is read: the wrapper owns its own container and its
  own store, and the console never holds a credential for it.
* **The wrapper contract:** `modelopt-local --version` prints one line (e.g. `0.47.0`); `modelopt-local run --job-id ID
  --recipe NAME --run-dir DIR` starts the job in the BACKGROUND and returns at once, leaving the verdict in
  `DIR/run.json` (`job_id`, `recipe`, `method`, `base_model`, `output`, `verdict` in `running|succeeded|failed|error`,
  `ts`, `duration_ms`, `metrics{output_bytes, base_bytes, ratio}`, `error`). The console reads each job dir
  `<state_dir>/modelopt/jobs/<job_id>/` read-only, size-capped, and never writes a job's files or stops a job.
* **Metric key prefix:** `mo.` — `mo.jobs`, `mo.jobs_by_verdict{verdict}` (one point per verdict),
  `mo.live_running`, `mo.output_bytes`, `mo.compression_ratio`, `mo.duration_s`. Every number is read from a finished
  job's `run.json`; nothing is estimated. Event kind: `modelopt.job` (data: `job_id`, `recipe`, `verdict`).
* **Config:** `clients/modelopt/recipes.yaml` is EDITABLE and authoritative. Its schema is the lab tool's
  (`clients/modelopt/src/modelopt_core.py` `validate_recipes`), and the console enforces exactly the same rules:
  `version` must be 1; 1-64 recipes, names match `[a-z0-9][a-z0-9-]{0,39}`; a recipe needs `method` in
  `reap|quantize|minitron`, `base_model` (a Hugging Face repo id, `org/name`) and `output` (a path under `/runs/`);
  optional `description`, `preset`, `config` (strings) and `sparsity` (a number, 0 <= s < 1); any other key is refused
  with its line number. Like the tool, the console runs nothing from a file with any error.
* **Actions:** `run` (DISRUPTIVE: it takes a GPU for hours; the wrapper returns at once and the job keeps running
  after the tab closes) and `report` (SAFE: one job's `run.json`, redacted). No stop, no cancel: the console does not own
  the container's lifecycle. No playground.

**10.10 (2026-11-03): one more system — `gym` (NVIDIA NeMo Gym).** Additive; nothing above changes. Contract: ADR-0018.
* **Core:** `SystemId` gains `gym`; `adapters/mod.rs` registers `gym::GymAdapter` ("NeMo Gym"). Runner allowlist gains
  `gym-local` (`gym-local smoke` gets a 3600 s cap; every other subcommand keeps 600 s). `ctx.paths` gains `gym_dir`;
  `DEFAULT_LAYOUT` gains `("gym", "clients/nemo-gym")`. No env is forwarded and no key is read: the wrapper's broker
  holds the router key, and the console reports only whether the key file exists (`[set]`/`[missing]`).
* **The wrapper contract:** `gym-local status` (read-only: compose ps, the network's `Internal` flag via `verify`, the
  image, the key file's presence) exits 0 whenever Docker answers; the console parses the broker row
  (`nemo-gym-router-broker   Up ...|Exited ...`), `RESULT: PASS|FAIL — network ai-lab-nemo-gym ... Internal=<v>`,
  `image: lab/nemo-gym:0.7.0 <id>|not built` and `key: <path> [set, N chars]|missing`, cached 15 s. `gym-local verify`
  exits 0 when the network is internal. `gym-local smoke --model ALIAS --limit N` runs in the FOREGROUND and prints
  `rollouts: N   with a reward: M   rewards: [..]   mean: X`, an `aggregate: {..}` line and a final `RESULT:` line.
* **Background run (the console owns it, like AIPerf):** `smoke` writes `<state_dir>/gym/runs/<run_id>/run.json` (0600:
  `run_id`, `model`, `limit`, `ts`, `verdict` in `running|succeeded|failed|error|interrupted`, `duration_ms`,
  `metrics{rollouts, rewarded, mean_reward, pass_at_1}`, `rewards[]`, `error`); one run at a time, newest 50 kept. A
  run a restart left at `running` reads as `interrupted`. Rollout text never leaves the wrapper's volume.
* **Health:** down when the wrapper does not answer or the client network exists but is NOT internal
  (`gym.not_contained`); degraded when the image or the key file is missing; a network not created yet and a stopped
  broker are normal. Components stay within the contract's kinds (`process` gym-local with its image, `container` the
  broker); the network and image facts are in the summary and the problems.
* **Metric key prefix:** `gym.` — `gym.runs`, `gym.runs_by_verdict{verdict}`, `gym.live_running`, and from the newest
  successful run `gym.mean_reward`, `gym.rollouts`, `gym.pass_at_1`, `gym.duration_s`. Event kind: `gym.smoke`.
* **Config:** none (the Gym environment config is the lab wrapper's). **Actions:** `smoke` (DISRUPTIVE: router tokens
  on a shared engine; model alias validated, 1-20 tasks), `verify` (SAFE) and `report` (SAFE). No playground.

**9.9 (2026-10-24): shutdown ends SSE streams.** Additive. On SIGTERM/SIGINT the server (0.3: `main.ConsoleServer`, a
`uvicorn.Server` subclass; 0.4: `server.rs`) calls the events service's `begin_shutdown` before its own exit handling: every open
event stream sends `event: shutdown` (`data: {}`) and ends, releasing its slot, so the process exits at once (a
browser's stream would otherwise hold it). `shutdown_grace_s` (setting, int, default 5, below `nemo-console stop`'s
10 s) is the graceful-shutdown backstop (uvicorn's `timeout_graceful_shutdown` in 0.3). Clients treat the end like any other stream
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
* **Settings** (`config.rs`, environment or the user env file only, read at start, never through the API):
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

**9.12 (2026-11-14): Relay trace explorer — sessions, traces, trajectories, analytics and capture coverage.**
Additive; no existing route, model or metric key changes. Read-only views over what NeMo Relay captured on this
machine. Decision record: ADR 0021.

* **Sources** (all under `ctx.paths`, read-only, `O_NOFOLLOW`, regular files only, contained in their directory):
  `relay_atof_dir` (files matching `^.+\.jsonl(\.\d+)?(\.gz)?$`: the live file, plain rotations, gzip rotations),
  `relay_atif_dir` (`*.json`), `relay_plugins` (already known), and the new optional `relay_hermes_dir`
  (`~/.local/share/nemo-relay/hermes`; trajectories in `<agent>/atif/*.json`, `<agent>` matching
  `^[a-z][a-z0-9-]{0,40}$`). A missing directory is an empty source, never an error.
* **Trace index** (`adapters/relay_traces.rs`, new): built and refreshed by ONE background task, never on the request
  path; requests read a snapshot. In memory only (nothing is written to disk). It pairs ATOF `scope` start/end events by
  `uuid` into spans, keeps non-chunk `mark` events, and groups spans into traces (a trace = a root span and its
  descendants; the root is the span whose parent is null or not in the index) and sessions (below).
  * It stores **metadata only**: identifier-shaped strings (the adapter's `IDENT` rule), numbers, booleans and
    timestamps. No prompt, message, tool argument, tool result or header value (except the allowlisted identifiers
    named below) is ever held in the index. Content is read from the source file on demand (see the content route).
  * `llm.chunk` marks are not stored one by one: per parent LLM span the index keeps their count, the largest gap
    between two chunks and at most 64 evenly thinned offsets (ms from the span start).
  * Unpaired scopes are data: a start without an end is a span with `status = "open"`; an end without a start is
    kept with `start_ts = null`.
  * Refresh is incremental: per file it remembers (device, inode, size, a hash of the first 4 KiB) and the consumed
    offset; a file that grew is read from that offset, a file that shrank (logrotate `copytruncate`) is re-read from
    0, a rotated copy with a known first-4-KiB hash and size is not read again. gzip rotations are stream-decompressed.
  * Bounds (settings, environment or the user env file only, validated at start):
    `relay_trace_scan_bytes` (uncompressed bytes one full build may read, newest file first; default 16 GiB, min 16
    MiB, max 256 GiB), `relay_trace_max_spans` (default 300000, 1000..5000000; the oldest traces are dropped first),
    `relay_trace_max_line_bytes` (default 64 MiB, 64 KiB..1 GiB; a longer line is skipped and counted),
    `relay_trace_max_atif_bytes` (default 256 MiB per trajectory file), `relay_trace_refresh_s` (default 5, 2..3600),
    `relay_trace_index` (`on` default | `off`: no task; see "Index off" below),
    `relay_trace_content` (`admin` default | `off`).
  * Session key of a trace, first match: the client session id taken from the first LLM request's headers
    (`x-opencode-session-id`, `x-session-id`, `x-claude-code-session-id`), else Relay's `metadata.session_id`, else the
    trace id. `client` is derived from the `user-agent` product token (`opencode`, `claude-cli`, …; identifier-shaped,
    else null). `kind` is `metadata.agent_kind` (`gateway`, `claude-code`, `codex`, …; else `unknown`).
* **Routes** (all `GET`, under `/api/v1/systems/{id}`; any system other than `relay` answers 404 `not_found`, as the
  Data Designer run routes do). `viewer` unless stated. Timestamps are epoch seconds (float). Absent data is `null`,
  never 0. Common query: `window` = `1h|6h|24h|7d|30d|all` (default `24h`), `anchor` = `latest|now` (default
  `latest`: the window ends at the newest indexed event, so an idle lab still shows its last activity); every answer
  that takes them returns `range: {from, to}`. Invalid query values are 422 `invalid_request`.

  | Path | Returns |
  |---|---|
  | `/traces/index` | `IndexState` |
  | `/traces` | `{range, total, offset, limit, items: TraceSummary[], facets}`. Filters: `client`, `model`, `kind`, `status` (`ok\|error\|open`), `session`, `q` (substring of trace id, session key, model or root name; <= 80 chars of the `IDENT` alphabet), `min_ms`. `sort` = `start\|duration\|tokens\|llm_calls` (default `start`), `order` = `asc\|desc` (default `desc`), `limit` 1..200 (default 50), `offset` |
  | `/traces/{trace_id}` | `{trace: TraceSummary, spans: Span[], marks: Mark[], truncated}` (at most 5000 spans and 5000 marks, earliest first); 404 unknown, 422 if the id is not UUID-shaped |
  | `/traces/{trace_id}/spans/{span_id}/content` | **admin**. `SpanContent`. 403 `content_disabled` when `relay_trace_content = off`; 404 unknown; 409 `content_unavailable` when the source line can no longer be read. Query: `max_chars` 1000..200000 (default 32768, per text), `messages` = `tail\|head` (default `tail`) |
  | `/sessions` | `{range, total, offset, limit, items: SessionSummary[]}`. Filters `client`, `kind`, `q`; `sort` = `last\|traces\|tokens` (default `last`), `order`, `limit`, `offset` |
  | `/sessions/{session_key}` | `{session: SessionSummary, traces: TraceSummary[] (<= 500, by start), calls: SessionCall[] (<= 2000, by time)}`; 404 unknown |
  | `/analytics` | `Analytics` for the window; filters `client`, `model`, `kind`; `buckets` 12..240 (default 60) |
  | `/trajectories` | `{total, offset, limit, items: TrajectorySummary[]}`; filters `source`, `q`, `trace` (a trace id: only the trajectories linked to that trace; 422 if not UUID-shaped); newest first; `limit` 1..200 (default 50) |
  | `/trajectories/{id}` | `{trajectory: TrajectorySummary, steps: TrajectoryStep[] (<= 2000), truncated}`; query `source` when the id is not unique; 404 unknown (or when its `<agent>` or `atif` directory is now a symlink); 409 `content_unavailable` when the file changed since it was indexed (device, inode, size or mtime) |
  | `/trajectories/{id}/steps/{step_id}/content` | **admin**, same switch and errors as span content; takes `source` like the route above. `StepContent` |
  | `/capture` | `Capture` (which capture levels are configured, and what was observed for each) |

  Shapes (every key is always present; unknown values are `null`):
  * Pinned details (2026-11-14, after the first frontend pass asked):
    * `facets` = `{client: [{value, count}], model: [..], kind: [..], status: [..]}`: the keys are the filter
      names; each list is counted over the window BEFORE any filter (so choosing one value does not empty the other
      lists), sorted by count descending then value, at most 50 entries. A null client or kind is counted under
      `value: null`.
    * `Span.flags` are the ATOF `attributes` flags (`streaming`, `parallel`, `remote`, `stateful`, `relocatable`,
      or any other identifier-shaped flag). `Span.role` is `session`, `turn`, `subagent` or null
      (`metadata.nemo_relay_scope_role`). `Span.subtype` is `category_profile.subtype` or null.
    * `heatmap.latency.counts[t][b]`: first index time bucket (`ts[t]`), second index duration bucket
      (`edges_ms[b]`..`edges_ms[b+1]`). `heatmap.activity.counts[d][h]`: first index day, second hour (UTC).
    * `Capture.levels[].detail` and `.observed` are objects with these keys (unknown values null; `observed` is `{}`
      for a level the console cannot observe):
      `atof`: detail `{sinks: [{type, file, mode}]}`, observed `{last_ts, events, bytes, files}`;
      `atif`: detail `{agent_name}`, observed `{last_ts, trajectories, bytes}`;
      `otel_traces`: detail `{endpoints: [{projection, target, transport, service_name, reachable}]}`;
      `otel_logs`: detail `{minimum_severity, derived, endpoints: [{target, transport, reachable}]}`;
      `otel_metrics`: detail `{export_interval_ms, derived, endpoints: [{target, transport, reachable}]}`
      (`derived` = true when the section has no explicit endpoints, so Relay exports the signal once per trace
      endpoint: with more than one trace endpoint that is a duplicate export and a hint says so);
      `payloads`: detail `{mode: "full" | "turn_filtered"}`; `redaction`: detail `{detectors: [..]}`;
      `resource_metrics`: detail `{polling, interval_ms}`; `pricing`: detail `{}`.
      `target` is `host:port`. `Capture.projections` = `{gen_ai, openinference, full}` booleans.
    * A 403 on a content route is `content_disabled` when the switch is off; any other 403 is the role check.
    * Index off (`relay_trace_index = off`; amended 2026-11-14 to match the implementation): `/traces/index` answers
      `state: "disabled"`; `/traces`, `/sessions`, `/analytics`, `/trajectories` and `/capture` answer 200 with their
      usual shape, empty, plus one extra key `state: "disabled"` (the only answers that carry `state`; it is absent
      while the index is on); the detail routes answer 404 `not_found` with a message naming `relay_trace_index =
      off`; the content routes answer 403 `content_disabled` when content is also off, else 404 like the details.
    * Content scrubbing = the adapter's `sk-`/Bearer patterns, its `key = "value"` line rule (run with the linear
      `regex` engine), plus a content-only pass for `key: value`, `"key": "value"`
      (also escaped inside JSON strings) and `KEY=value` pairs whose key names a secret (`api_key`, `secret`,
      `password`, `token`, `private_key`, `access_key`, `credential`, `auth`, ...; values of 8+ chars; config
      placeholders such as `${...}`, `{env:...}`, `{file:...}` are kept). Added 2026-11-14: tool results can
      carry configuration text whose credential pairs `scrub()` does not match.
    * Settings are read from the environment as `NEMO_CONSOLE_<NAME IN UPPER CASE>`, for example
      `NEMO_CONSOLE_RELAY_TRACE_INDEX=off` and `NEMO_CONSOLE_RELAY_TRACE_CONTENT=off`.
  * `IndexState`: `{state: "building"|"ready"|"empty"|"disabled", generation, last_refresh_ts, build_ms,
    files: [{name, bytes, scanned_bytes, compressed, events, skipped_lines, state: "indexed"|"partial"|"skipped"|"error",
    first_ts, last_ts}], events, spans, open_spans, marks, chunk_marks, traces, sessions, trajectories,
    coverage: {from_ts, to_ts}, truncated, limits: {scan_bytes, max_spans, max_line_bytes}, content: "admin"|"off",
    problems: [{code, message}]}`. File names only, never directories.
  * `TraceSummary`: `{trace_id, root_name, kind, client, session_key, relay_session_id, start_ts, end_ts, duration_ms,
    status: "ok"|"error"|"open", spans, llm_calls, tool_calls, tool_errors, marks, subagents, models: [..<=8],
    input_tokens, output_tokens, cached_tokens, reasoning_tokens, total_tokens, max_input_tokens, ttfc_ms,
    routing: {algorithm, route, decision, served_model, overhead_ms, errors} | null,
    error: {type, phase} | null, has_trajectory}`. A trace is `error` when any of its spans is; `open` when its root
    has no end and nothing failed.
  * `Span`: `{id, parent_id, name, category, subtype, role, flags: [..], start_ts, end_ts, duration_ms, status, depth,
    turn_index, llm: Llm | null, tool: {name, call_id, type, mcp} | null, error: {type, status_code} | null,
    content_available, size_bytes}` with `Llm = {model, served_model, provider, api, stream, input_tokens,
    output_tokens, cached_tokens, cache_write_tokens, reasoning_tokens, total_tokens, ttfc_ms, tokens_per_s,
    finish_reason, messages, tool_defs, tool_calls_requested, requested_tools: [..<=16 names], max_tokens,
    reasoning_effort, chunks, chunk_offsets_ms: [..<=64], max_chunk_gap_ms, correlation}`. `category` is the ATOF
    category; `api` is the scope name (`openai.chat_completions`, `anthropic.messages`, …); `tokens_per_s` =
    output tokens / (duration - time to first chunk) when both are known and positive.
  * `Mark`: `{id, parent_id, ts, name, category, schema, severity, data}`; `data` holds only allowlisted scalar
    fields (the ones `parse_atof_line` already allows, plus `selected_model`, `decision`, `route`, `tier`,
    `hook_event_name`, `tool_name`) and, for a metric mark, `measurements: [{name, kind, value, unit, attributes}]`
    (<= 8, attributes <= 8 identifier pairs).
  * `SpanContent`: `{span_id, category, source: {file, compressed}, request: {model, params, system: Part[],
    messages: [{role, parts: Part[]}], tools: [{name, description}], messages_total, messages_returned} | null,
    response: {model, finish_reason, parts: Part[], usage} | null, tool: {arguments: Part | null, result: Part | null}
    | null, headers: {name: value}, truncated, redactions}` with `Part = {type: "text"|"reasoning"|"tool_call"|
    "tool_result"|"image"|"other", text, name, call_id, chars, truncated}`. `params` are scalars only. `headers` is an
    exact allowlist of 18 names (`user-agent`, `content-type`, `traceparent`, `tracestate`, `x-opencode-client`, the
    session headers and named `x-nemo-relay-*` id headers such as `-session-id`, `-turn-id`, `-request-id`; no
    wildcard), and a value is shown only when it is identifier-shaped (user-agent: printable ASCII, <= 256 chars) and
    the scrubber changes nothing in it. Every text goes through the content scrubbing below; `redactions` counts what it
    replaced. At most 400 messages; the whole answer <= 4 MiB, a hard bound (texts are cut, then every string, then the
    longest lists are halved; `truncated = true`). Images and any block of an unknown type are reported as a part with
    its type and `chars` only, never its data.
  * `SessionSummary`: `{session_key, client, kind, first_ts, last_ts, span_ms, traces, llm_calls, tool_calls, errors,
    models: [..], input_tokens, output_tokens, cached_tokens, max_input_tokens}`.
    `SessionCall`: `{ts, trace_id, span_id, model, input_tokens, cached_tokens, output_tokens, reasoning_tokens,
    duration_ms, ttfc_ms, status}`.
  * `Analytics`: `{range, bucket_s, totals: {traces, sessions, llm_calls, tool_calls, errors, error_rate,
    input_tokens, output_tokens, cached_tokens, reasoning_tokens, cache_hit_ratio, p50_ms, p95_ms, p99_ms,
    ttfc_p50_ms, ttfc_p95_ms, tokens_per_s_p50}, series: {ts: [..], traces: [..], llm_calls: [..], errors: [..],
    input_tokens: [..], output_tokens: [..], cached_tokens: [..], p50_ms: [..], p95_ms: [..], ttfc_p50_ms: [..],
    by_model: [{model, llm_calls: [..], tokens: [..]}]}, by_model: [{model, llm_calls, errors, input_tokens,
    output_tokens, cached_tokens, reasoning_tokens, p50_ms, p95_ms, ttfc_p50_ms, ttfc_p95_ms, tokens_per_s_p50,
    tokens_per_s_p95}], by_client: [{client, traces, llm_calls, tokens, errors}], by_kind: [{kind, traces, llm_calls,
    tokens, errors}], histograms: {duration_ms, ttfc_ms, input_tokens, output_tokens, tokens_per_s, chunk_gap_ms}
    (each `{edges: [..n+1], counts: [..n]}`), heatmap: {activity: {days: [..<=31 "YYYY-MM-DD" UTC], hours: [0..23],
    counts: [[..24]..]}, latency: {ts: [..], edges_ms: [..], counts: [[..]..]}}, tools: [{name, calls, errors, p50_ms,
    p95_ms}], tool_requests: [{name, count}], routing: {requests, by_algorithm: [{algorithm, count}], decisions:
    [{route, decision, served_model, count}], judge: {calls, errors, p50_ms, p95_ms}, overhead_p50_ms,
    overhead_p95_ms, failures: [{kind, phase, count}], flows: [{source, target, value}]}, errors: {by_type: [{type,
    count}], by_status: [{status, count}], finish_reasons: [{reason, count}], recent: [{ts, trace_id, span_id, model,
    type}]}, scatter: [{trace_id, span_id, ts, duration_ms, input_tokens, output_tokens, model, status}]}`.
    Durations and percentiles are over LLM spans unless the key says otherwise (`totals.p*_ms` and
    `series.p*_ms`: LLM span duration). `series.by_model` and `by_model` list the 8 busiest models, the rest as
    `other`. `tools`, `tool_requests` <= 30 rows, `errors.recent` <= 20, `scatter` <= 600 evenly sampled LLM spans.
    Percentiles of an empty set are `null`. `routing.flows` are the edges route -> decision -> served model.
  * `TrajectorySummary`: `{id, source, file, bytes, mtime, schema_version, agent: {name, version, model}, session_id,
    steps, tool_calls, prompt_tokens, completion_tokens, cached_tokens, first_ts, last_ts, trace_id}`. `source` is
    `gateway` for `relay_atif_dir`, else the Hermes agent directory name. `trace_id` links to an indexed trace when
    a step's `extra.ancestry` names one of its spans. `id` is the first UUID among `trajectory_id`, `session_id` and
    the file name; when there is none (Hermes writes `IDENT`-shaped ids such as `<name>_<hex>_<date>_<time>`), it is
    DERIVED: the first 16 bytes of SHA-256 over `"nemo-console/atif-id\0" + source + "\0" + trajectory_id (else
    session_id)`, with the UUIDv8 version and variant bits set, so it is stable across restarts and the routes keep
    one id shape; the original id stays visible as `session_id`. A file with neither is counted as not readable.
    (Amended 2026-11-14 after the first run on real captures: all Hermes trajectories were being skipped.)
    `TrajectoryStep`: `{step_id, ts, source, model, message_chars, reasoning_chars, tool_calls: [{id, name,
    arguments_chars}], observations, metrics: {prompt_tokens, completion_tokens, cached_tokens, reasoning_tokens},
    duration_ms, span_id, status}`. `StepContent`: `{step_id, message: Part | null, reasoning: Part | null,
    tool_calls: [{id, name, arguments: Part}], observations: [{call_id, content: Part}], truncated, redactions}`.
  * `Capture`: `{relay_version, levels: [{id, label, enabled, state: "active"|"idle"|"stale"|"off"|"unknown",
    detail, observed}], projections: {gen_ai, openinference, full}, hints: [{code, message}]}` with level ids
    `atof`, `atif`, `otel_traces`, `otel_logs`, `otel_metrics`, `payloads`, `redaction`, `resource_metrics`,
    `pricing`. `detail` comes from `plugins.toml` (exporter kinds, projection per trace endpoint, `host:port` of an
    endpoint, never its path, query, user info or headers); `observed` from the index (`last_ts`, counts, bytes).
    `state`: `off` when not configured; `active` when something was written in the last 15 minutes, `idle` within 7
    days, `stale` beyond; `unknown` when the console cannot observe that level (OTLP exports). An OTLP endpoint on a
    loopback host also gets `reachable` (one cached HTTP GET, 300 ms timeout, at most once a minute; anything that
    answers counts). `hints` name what a reader of the traces is missing (for example only the `gen_ai` projection is
    exported, marks are not exported as logs, payloads are turn-filtered).
* **Security.** The index and every viewer route are metadata-only, as `parse_atof_line` is. Content routes are
  admin-only, can be switched off, scrub secrets and cap sizes. Like every other GET they write no audit row; the
  access log line carries the route label only (no ids). Ids in paths are
  validated before use (UUID shape for trace, span and trajectory ids; `IDENT` for session keys; a decimal for step
  ids). No route takes a file name or path. The frontend renders every string with `textContent`; nothing from a
  capture is ever parsed as HTML. Threat model rows I-9 / R-25.
* **Frontend.** The Relay page gains the tabs `traces`, `sessions`, `analytics`, `trajectories` and `capture`
  (after `status`, before `config`). Charts are dependency-free SVG built by pure geometry functions in
  `frontend/js/viz/` (unit-tested in Node) and rendered with `dom.js`; no library, no canvas, no inline style or
  script (the CSP is unchanged). Deep links: `#/relay?tab=traces&trace=<id>&span=<id>`,
  `#/relay?tab=sessions&session=<key>`, `#/relay?tab=trajectories&id=<id>`. The mock server serves every route above
  with deterministic data, and the browser suite visits every new tab and one detail view of each.

**9.13 (2026-11-14): visualizations for every system, wave 1 — series history, run history, shared charts.**
Additive except one role change (Data Designer records become admin-only). Later waves (per-system views) build on it
and get their own addenda.

* **Route `GET /api/v1/systems/{id}/metrics/series`** (viewer; every system; Rust-only, parity deviation D-36). The
  existing `/metrics/history` route is unchanged. Query: `keys` = a comma-separated list of 1..12 canonical metric keys
  (each matching the existing `KEY_RE`; a bare key also matches its label variants), `window` = `15m|1h|24h|7d`
  (default `1h`), `split` = `0|1` (default `0`). Answer:
  `{window, step_s, from, to, series: [{key, label: {..} | null, points: [[ts, avg, max], ..]}], truncated}`.
  * `split=0`: one series per requested key, label variants summed per bucket (as `/metrics/history` does), `label`
    null. `split=1`: one series per stored series name (label variant), `label` = its labels parsed into an object
    (`sy.model.calls{model="x"}` -> `{"model": "x"}`), at most 32 series per key (the rest dropped, `truncated: true`),
    ordered by key then label.
  * Buckets: `15m` = the in-memory ring, raw points (`step_s` = the poll interval, `max` = the value itself); `1h` and
    `24h` = the 1-minute rollups (`step_s` 60); `7d` = the rollups regrouped into 10-minute buckets (`step_s` 600; avg
    weighted by each minute's sample count `n`, max = max of maxes). Timestamps are bucket starts, epoch seconds.
    Points are in time order; a key with no data is a series with `points: []` (never an error). Values are numbers;
    no gap filling (the client draws gaps).
  * 422 `invalid_request` for an unknown window, more than 12 keys, an empty or malformed key, `split` not 0/1.
  * Rollups are kept 8 days (`ROLLUP_KEEP_S`), so `7d` is always covered once the console has run that long.
  * Pinned details (2026-11-14, from the build): `keys` is split on commas outside braces (a labelled key keeps its
    commas); with `split=1` an unlabelled stored name gets `label: {}`, and the 32 kept are the first by name (not the
    busiest; the client picks what to draw); with `split=0` the bucket `max` is the sum of the variants' maxes (an upper
    bound of the true max of the sum); a key with no data is one series with `label: null` and `points: []`; series of a
    key that have no point in the window are left out; `to` is now, `from` the first bucket's start (a 10-minute
    boundary for `7d`); `step_s` is a number; non-finite values are dropped; a duplicated key is answered twice.
* **Role change: `GET /systems/{id}/runs/{run_id}/records` is admin** (was viewer). Data Designer rows are model-written
  content, like Relay span content (ADR-0021); viewers keep the run list, run detail and artifact list (paths and
  sizes). The role test lists it among the admin GETs. A viewer gets 403 `forbidden` (the role check). Parity:
  deviation D-37 (the Python reference served it to viewers). Threat model: E-7 says the records are admin-only.
  A later wave adds an aggregates-only column profile for viewers.
* **Frontend (no new dependency, CSP unchanged):**
  * `frontend/js/viz/*` and the reusable chart cards in `frontend/js/rt/charts.js` become a shared toolkit any view may
    import (move shared helpers out of `rt/` into `frontend/js/viz/` or a new `frontend/js/charts/` folder if they are
    Relay-specific in name only; keep Relay working unchanged).
  * Generic system pages (`components/generic-system.js`): (1) a counter series is drawn as a per-second (or
    per-minute, label says which) rate, never as a rising total: a descriptor entry may say `rate: true`, and keys
    ending in `_total` are rates by default; resets (a negative delta) start a new segment; (2) the window picker gains
    `7d`; (3) a "show max" toggle draws the bucket max as a thin line or band above the average where `step_s` > the
    poll interval; (4) charts fetch through the new series route (one request per chart, `split=1` where the
    descriptor asks for one line per label, e.g. per GPU), with a series picker when there are more than 4 lines
    (default: the 4 busiest, the rest toggled on by the user; the choice is kept in the URL hash).
  * **Run history component** (`frontend/js/components/run-history.js` or similar), configured by a descriptor per
    system, built only from `GET /systems/{id}/events?limit=500` (event `data`), so no backend change: `{kind,
    id, label, verdict, numbers: [{field, label, unit, better: "lower"|"higher"}], groupBy: [field..], filters:
    [field..]}`. It shows: (a) a verdict timeline (one mark per run, coloured by verdict, hover = summary, click =
    select); (b) a trend chart of one chosen number across runs (points coloured by a chosen group field, e.g. model or
    preset; line per group); (c) A/B compare: pick two runs, a table of every configured number with the difference and
    a better/worse mark; (d) filters by the descriptor's fields (enum dropdowns built from the values seen). Run ids
    and field values are rendered with textContent; the URL hash keeps the selection (`run=`, `vs=`, `metric=`,
    `group=`, filters).
  * Wired into the eight run-based pages with descriptors read from their adapters' event data: aiperf (`aiperf.run`),
    evaluator (`evaluator.run`, per-benchmark scores flattened as `bench:<name>` numbers), gym (`gym.smoke`),
    modelopt (`modelopt.job`), skillevaluator (`skilleval.run`), datadesigner (`dd.run`), dcgm (`dcgm.diag`), guardrails
    (`guardrails.check`, per-probe pass as numbers). A page whose events lack a field shows `—`, never 0.
  * The mock server serves `/metrics/series` and realistic run events for all eight systems; the browser suite visits
    every page that gains a chart, the run history with one A/B compare, and the 7d window.

**9.14 (2026-11-14): visualizations, wave 2 — GPU telemetry (DCGM), Switchyard, Dynamo.** Additive (new metric keys,
one new route, new tabs/cards); no existing key, route or shape changes. Builds on 9.13 (series route, shared charts).

* **New metric keys** (all go through the existing poller, ring and rollups, so the series route serves them; label
  sets as listed; counters end in `_total` so the frontend draws them as rates; a value the source does not report is
  simply absent, never 0; label values from upstreams are cleaned to `[A-Za-z0-9_.:/-]`, at most 64 characters;
  non-finite values and series names longer than the key bound give no point; a series idle for 30 minutes leaves the
  ring and frees its slot under the 256-series cap, its rollups stay):
  * DCGM, labels `{gpu, model}` like the existing per-GPU keys, from families the exporter already serves
    (`observability/dcgm/counters.csv`): `dcgm.fb_reserved_b`, `dcgm.mem_temp_c`, `dcgm.mem_clock_mhz`,
    `dcgm.enc_util`, `dcgm.dec_util`, `dcgm.power_limit_w` (`DCGM_FI_DEV_POWER_MGMT_LIMIT`, else
    `ENFORCED_POWER_LIMIT`), `dcgm.energy_mj_total` (`TOTAL_ENERGY_CONSUMPTION`, mJ, counter),
    `dcgm.violation_us_total{reason=power|thermal|board|low_util|reliability|sync_boost}` (the `*_VIOLATION` counters),
    `dcgm.remapped_rows{kind=correctable|uncorrectable}`, `dcgm.row_remap_failure`. Existing keys unchanged.
  * Switchyard, from `/v1/stats` and `/metrics` the adapter already fetches: `sy.model.latency_ms{model, q=p50|p99}`
    (model call latency), `sy.model.errors_total{model}`, `sy.model.cache_hit_ratio{model}`,
    `sy.model.max_context_tokens{model}`, `sy.routing_overhead_ms{q=p50|p99}`,
    `sy.fallbacks_total{reason}` (`context_window`, `unavailable`, ...), `sy.decisions_total{source, target}`
    (stage-router decision source -> target name), `sy.client_responses_total{outcome}`. A `/v1/stats` reset (recreate,
    `reset_stats`) is a counter reset; the frontend already starts a new segment.
  * Dynamo, from the frontend and worker `/metrics` the adapter already scrapes:
    `dyn.ttft_window_ms{q=p50|p95|p99}` and `dyn.itl_window_ms{q}` (quantiles of the requests that finished SINCE THE
    PREVIOUS POLL — only the background poller's read moves the window; API/UI reads compare with the previous poll — from histogram bucket deltas; absent when no request finished; the existing since-start
    `dyn.ttft_ms` / `dyn.itl_ms` stay), `dyn.phase_ms{phase=queue|prefill|decode|inference, q=p50|p95}` (worker
    histograms, windowed the same way), `dyn.preemptions_total`, `dyn.capacity{kind=context_length|kv_blocks|
    max_num_seqs|max_batched_tokens}` (gauges, as reported), `dyn.spec_accept_pos{pos}` (acceptance rate per draft
    position when the worker reports it).
* **Route `GET /api/v1/systems/dcgm/fleet`** (viewer; any other system 404; Rust-only, parity deviation D-38): per-GPU
  history of every DCGM host Prometheus scrapes, so the fleet heatmap covers remote hosts. Query `metric` =
  `util|temp|power|fb_used` and `window` = `1h|24h|7d` (default `24h`). The server builds the PromQL from a fixed table
  (no client-supplied expression; ADR-0014): the DCGM family for the metric, restricted to the configured
  `dcgm_prometheus_jobs`, `query_range` with step 60 / 300 / 1800 s. Answer `{metric, unit, window, step_s, from, to,
  rows: [{host, job, gpu, model, points: [[ts, value], ..]}], truncated}`: at most 64 rows (ordered host, gpu),
  `host` as the adapter already names hosts for `dcgm.host_up`, no GPU UUID or serial (E-8). Prometheus down -> 200 with
  `rows: []` and a `problem: {code, message}` field; 422 for an unknown metric or window. Cached for 30 s per
  (metric, window); a Prometheus failure is answered from memory for 5 s for every (metric, window); one Prometheus
  request at a time; answers over 16 MiB are refused. One call per heatmap.
* **Frontend** (shared toolkit of 9.13; textContent only; CSP unchanged):
  * **GPU telemetry page:** a Fleet tab with a heatmap (rows = host / GPU, columns = time, colour = the chosen metric,
    a legend with the scale, hover = value and time, a metric and window picker, URL state) from `/dcgm/fleet`; on
    Status: a "really busy?" card per local GPU (util next to graphics, tensor and DRAM activity and SM clock), a
    memory card (used / reserved / free stacked per GPU), power against its limit, energy over the window (from the
    counter), throttle-violation rates by reason, PCIe tx/rx and replay rate, remapped rows; an XID and diagnostics
    timeline (swim lane per GPU from `dcgm.xid` / `dcgm.diag` events, click = details).
  * **Switchyard page:** a decision-flow card (decision source -> target model, with the routes listed beside it: the decision counters
    carry no route label, so route -> source cannot be joined; widths = request share over
    the window, from `sy.route.calls` and `sy.decisions_total` rates; a flows chart exists in the toolkit), per-model
    latency (p50 / p99 bars, error rate, cache-hit ratio, largest context), a heatmap of call rate per model over time
    (series route, `split=1`), fallbacks and routing overhead (tiles + lines), client outcomes; filters by route /
    model on the share cards (frontend filtering of what is served).
  * **Dynamo page:** latency for the chosen window from the windowed keys (labelled as such; the since-start ones
    labelled "since start"), a latency breakdown (stacked queue -> prefill -> decode, p50 and p95), a load chart
    (running, waiting, preemption rate, KV usage), output tokens per second (rate of `dyn.tokens{kind=completion}`),
    prefix-cache hit ratio, a capacity panel, spec-decode acceptance by position when present, and an Events tab
    (the existing `dynamo.log` events with level and component filters; the log lines are redacted text, R-5).
  * Mock server: the new keys with plausible values for all three systems, the fleet route (several hosts incl. a down
    one), Dynamo log events; the browser suite visits the new tabs and cards.
* Pinned details (2026-11-14, from the build): Dynamo output tokens are the existing `dyn.tokens{kind=completion}`;
  the fleet route's path is `/systems/{system_id}/fleet` (404 for every system but dcgm), its values use the per-GPU key
  units (util as a 0..1 ratio, bytes, °C, W), `metric` is required, `problem` is absent on success (codes
  `dcgm.prometheus.unreachable`, `dcgm.prometheus.bad_answer`, `dcgm.fleet.no_jobs`), failures are cached 5 s, and the
  PromQL groups by `job, host, gpu, modelName` only (no UUID or serial can come back); `dcgm.mem_temp_c` is absent on
  boards that report 0 (no sensor); `*_VIOLATION` counters are nanoseconds in the exporter, scaled to microseconds;
  windowed Dynamo quantiles use the previous POLL as baseline (adapters gain an optional `poll_metrics()`, default
  `metrics()`, which the poller calls; only it moves a baseline), and are absent on the first poll, after a reset or
  when nothing finished;
  `dyn.capacity` is the largest across models; `dyn.spec_accept_pos` is since the worker started; Switchyard picks
  `/v1/stats` first and `/metrics` per key. New keys are appended after the existing ones, and idle series are evicted after 30 minutes, so churn cannot fill
  the per-system 256-series cap for good.

**9.15 (2026-11-14): visualizations, wave 3 — AIPerf, NeMo Evaluator, NeMo Gym, Model Optimizer, SkillEvaluator, Data
Designer.** Additive (three per-run routes, wider event data, new cards); no existing route or shape changes except
the event `data` additions below (new keys only).

* **Route `GET /api/v1/systems/{id}/runs/{run_id}/summary`** (viewer; numbers and identifiers only; Rust-only, parity
  deviation D-40). Systems: `aiperf`, `skillevaluator`; every other system 404 `not_found`; unknown run 404; run id
  validated as the adapters already validate run ids; files read with the existing hardened reader (no symlinks, size
  caps, I-7).
  * `aiperf`: `{run_id, preset, model, verdict, concurrency, requests, isl, osl, duration_s, metrics: {<name>: {unit,
    avg, min, max, std, p1, p5, p10, p25, p50, p75, p90, p95, p99}}, http_phases_ms: {dns, connecting, sending,
    waiting, receiving} | null}` from `profile_export_aiperf.json` (every numeric metric it has, at most 100; absent
    statistics are null; strings other than units and names dropped).
  * `skillevaluator`: `{run_id, skill, tiers, verdict, tier1: {validators: [{name, passed, status, findings: {critical,
    high, medium, low, info}}], rubric: [{id, importance, score, pass, judge_pass}], dimensions: {<name>: number}} |
    null, tier3: {agents: [{name, model, skill_lift, with_skill, without_skill, metrics: [{name, with_skill,
    without_skill, delta}]}]} | null}`. No finding message, rubric criterion text, notes, conclusions or
    recommendations (they are skill- or model-influenced text; E-5/E-6).
* **Route `GET /api/v1/systems/aiperf/runs/{run_id}/requests`** (viewer; Rust-only, D-40): one row per request from
  `profile_export.jsonl`, numbers only: `{run_id, total, returned, truncated, rows: [{i, start_ms, end_ms, latency_ms,
  ttft_ms, itl_avg_ms, input_tokens, output_tokens, worker, error_code}]}` (`start_ms` relative to the first request;
  `worker` a small integer index assigned by first appearance; `error_code` an integer or null; no request,
  conversation or worker ids, no error text). At most 5000 rows (evenly sampled when there are more), file size cap as
  for the records reader.
* **Route `GET /api/v1/systems/datadesigner/runs/{run_id}/profile`** (viewer; Rust-only, D-40): per-column aggregates
  of `records.jsonl`: `{run_id, rows, columns: [{name, types: {string, number, bool, null, object, array}, non_null,
  distinct, distinct_capped, length: {min, p50, p95, max} | null, numeric: {min, p50, p95, max} | null, top: [{value,
  count}] | null}]}`. `top` (the 10 most frequent values of a column with at most 50 distinct values, each value cut
  to 64 characters) is filled ONLY for an admin; for a viewer it is null and the answer says `values_hidden: true` —
  the values are model-written content (9.13). `numeric` (min, p50, p95, max: nearest-rank, so real cell values) is
  admin-only too, null for a viewer; `length` is shown to both. Column names are shown to both but must be identifiers
  (`[A-Za-z_][A-Za-z0-9_]{0,63}`); any other column is dropped and `truncated` is set. At most 200 columns and 2000
  rows; the read is capped in bytes and per line (the records file on disk is not itself byte-capped), and any cap that
  stops the read sets `truncated`. Amended 2026-11-14 after the pre-merge audit (viewers get lengths, counts and
  shapes, never cell values; an oversized run.json or Tier 1 file — over 1 MiB, far above the few KB seen in
  practice — reads as absent).
* **Event `data` additions** (new keys only; parity D-41 pins the exact added keys): `evaluator.run` benchmarks gain
  `ci_low`, `ci_high`, `tokens`, `elapsed_s`, `failure_rate`; `gym.smoke` gains `rewards` (the per-task rewards array,
  at most 20 numbers).
* **Frontend** (shared toolkit; textContent only; CSP unchanged; viewers never see a value the API withholds):
  * **AIPerf:** in the run history, a run detail: percentile curves (p1..p99) of TTFT, ITL and latency for the selected
    run and, when two are selected, both overlaid (A/B); an HTTP-phase stacked bar; a per-request view (latency
    histogram, a request timeline as swim lanes by worker, error codes as bars); a throughput-vs-latency scatter over
    runs (one point per run, colour by concurrency). Every chart says the numbers include the router hop.
  * **Evaluator:** scores with confidence-interval bars per benchmark (A/B), a model x benchmark heatmap (latest score
    per pair), cost vs score (tokens or time vs score) and failure-rate bars.
  * **Gym:** the per-task reward distribution of a run (strip or histogram, A/B overlay), rewarded / rollouts stacked
    bars per run, mean reward and pass@1 trends by model.
  * **Model Optimizer:** job timeline (duration bars coloured by verdict and method), base vs output size per job
    (ratio label), age of a running job (a stuck job stands out).
  * **SkillEvaluator:** a validator x run (or x skill) heatmap of status and finding severity, rubric bars (score and
    pass), the Tier 3 lift as a dumbbell per agent and per metric (with vs without the skill), labelled "a skill can
    influence its own Tier 3 score".
  * **Data Designer:** a column profile on the Runs tab for the selected run: type mix, null rate, distinct count,
    length distribution, numeric range, and for admins the top values; plus a run timeline (duration by script,
    verdict colours, requested vs produced records).
  * Mock server: realistic answers for the three routes and the new event keys; the browser suite visits every new
    card.
* Pinned details (2026-11-14, from the build): a bad run id is 422 `invalid_request` on the three new routes (the
  older `/records` and `/files` keep their 404); the profile always carries `values_hidden` and a `truncated` flag
  (rows over 2000, columns over 200, or column names that fail the check), `types.null` counts explicit nulls and
  missing keys, `distinct` is counted up to 1000 per column (`distinct_capped`), lengths are measured as `/records`
  shows values (at most 2048 characters), at most 64 MiB of `records.jsonl` is read, and top values are not even
  collected for a viewer; request rows give `error_code` 0 for a failed request with no usable code (null = success),
  number workers by first appearance over the whole file (at most 1024), and convert ns/us/ms/s (any other unit is
  null); the AIPerf summary drops `count`/`sum`, takes each HTTP phase's average, and answers `metrics: {}` for a run
  without a profile file. After the pre-merge audit (2026-11-14): at most 2 of these reads run at once on the
  blocking pool; every JSON document (run.json, the AIPerf summary, the Tier 1 file) is parsed only if it is at most
  1 MiB and 100,000 values (else it reads as absent: `metrics: {}` / `tier1: null`); a request line over 256 KiB or
  100,000 values is not parsed and is counted in a new `skipped` key (and sets `truncated`), every line counts toward
  the 200,000-line cap; a profile line over 1 MiB is skipped and sets `truncated`; the profile answer gains
  `columns_dropped` (columns that are not identifiers or come after the 200th); hard links are refused and every file
  is opened relative to the run directory (`openat`, `O_NOFOLLOW`). Peak memory per request measured at 5-37 MiB
  (`tests/runviz_memory.rs`).

**9.16 (2026-11-14): visualizations, wave 4 — NeMo Guardrails, OpenShell, NVIDIA Flare, Overview, Audit.** Additive
(new routes, persisted OpenShell events, new cards); no existing route or shape changes except the additive audit
query parameters below. Same rules as 9.13-9.15: viewers never see content, every string identifier-checked and
capped, files read without following links and with byte/line caps, parsing off the async workers, bounded memory.

* **Guardrails — `GET /api/v1/systems/guardrails/rails`** (viewer; Rust-only D-42): the rail pipeline of each served
  config, parsed from the config files the adapter already reads: `{configs: [{id, main_model, input: [flow, ..],
  output: [flow, ..], streaming: {enabled, chunk_size, context_size} | null, retrieval: [flow, ..]}], problems}`.
  Flow names identifier-checked (letters, digits, space, `_-.`, at most 80 chars; others dropped and counted); no
  prompt text, no `.co` content. Config ids as the adapter already lists them.
* **OpenShell — persisted sandbox events:** the adapter's `openshell.sandbox` lifecycle events (created / phase /
  deleted) are also appended to `<state>/openshell/events.jsonl` (0600, at most 5000 lines: older lines dropped by a
  rewrite when it grows past 6000; one writer), and `recent_events` reads them after a restart. Only the fields the
  ring already has (name, id, phase, action, previous_phase, ts).
* **OpenShell — `GET /api/v1/systems/openshell/decisions?sandbox=<name>&limit=<1..2000>`** (viewer; D-42): the
  policy decisions in a sandbox's log (the OCSF lines `openshell logs` prints): `{sandbox, rows: [{ts, category,
  verdict: "allowed"|"denied", kind, target}], truncated}`. `sandbox` must be a name in the current sandbox list
  (else 404); `category`/`kind` from fixed allowlists of the OCSF fields seen in the fixtures (others "other");
  `target` only when it is host:port or a path-free identifier (at most 120 chars, else null). No free text, no
  command line, no file contents. The CLI call reuses the adapter's existing `sandbox_logs` runner path with its
  timeout and redaction; at most 2000 rows; cached 10 s per sandbox.
* **Audit — `GET /api/v1/audit` gains optional `since`, `until` (epoch seconds), `actor`, `status`
  (`ok|error`)** (still admin; additive; parity D-43 if the corpus notices), and **`GET /api/v1/audit/summary?window=
  24h|7d|30d`** (admin; Rust-only D-42): `{window, by_day: [{day, system, count}], by_actor: [{actor, system, count}],
  errors: [{ts_bucket, count}], denied_total}` from the audit table (actor = the key's alias as stored; no params).
* **Frontend** (shared toolkit; textContent only; CSP unchanged):
  * **Guardrails:** a probe matrix over time (rows = the three probes, columns = check runs, cell = expected vs
    actual, a latency strip) from the existing check events; the rail pipeline diagram per config (input flows -> main
    model -> output flows, streaming settings) from `/rails`; an up/down strip from `gr.up` history.
  * **OpenShell:** sandbox lifecycle swim lanes (one lane per sandbox, coloured phase segments, create -> ready ->
    delete) from the persisted events, a phase-count stacked area from `os.sandboxes_by_phase` history, and a
    per-sandbox policy-decision timeline (allowed vs denied by category) from `/decisions`. Read-only: no new controls
    that change policy; delete stays the existing destructive action.
  * **Flare:** a target grid (target x host x chip x state: serving / loading / busy / absent, package counts) from
    status, tokens/s and request rate per target (rates of the existing counters, series route split by target),
    prefill vs generated vs cache-reused stacked bars, active vs pending per target.
  * **Overview:** a sparkline of one key metric per system tile (series route, 1h), problem counts by severity across
    systems, and an up/down strip for systems that have a `*.up`-style metric.
  * **Audit:** actions per day stacked by system, an actor x system heatmap, an error timeline, time-range, actor,
    status filters (URL state) on the log.
  * Mock server: every new route and the persisted OpenShell events, generic names; the browser suite visits every
    new card.
* Pinned details (2026-11-14, from the build): the three routes are templated (`/systems/{system_id}/rails`,
  `/decisions`; 404 for every other system). Rails: read from `config.yml`, else `config.yaml`; flow order kept; the
  YAML is composed but never constructed (aliases are not expanded: no alias bomb), merge keys are not applied;
  problems are `{config, code, count}` with fixed codes (no free text); an unreadable config is listed with empty
  lists. Decisions: a row only when the token after the level is ALLOWED/ALLOW/DENIED/DENY/BLOCKED, `target` only the
  next token when it passes the check; rows oldest first (the newest `limit`, default 500); one `openshell logs` call at
  a time for this route (`-n 2000`, the action's 30 s timeout, 512,000-byte cap, redaction); a failed call is 200 with
  `rows: []` and a fixed `problem`. Persisted events: re-checked on write and on read; after a restart the newest 200
  refill the ring and the sandboxes they leave alive become the baseline (no burst of "present" events; a sandbox
  deleted while the console was down is reported once). Audit summary: default `7d`, extra keys `from`, `to`,
  `bucket_s` (3600 / 21600 / 86400), `truncated`; `denied_total` counts audit rows with status 401/403 (requests
  refused before a handler are not audit rows); an unknown stored system is `"other"`, an actor that fails the check
  is null.
* After the pre-merge audit (2026-11-14): decisions `target` is only `host:port` (a DNS name, IPv4 or [IPv6] address
  and a port 1-65535; the token after the verdict, else the token after ` -> `; at most 120 characters), else null;
  output that reaches the CLI cap (1 MiB for this route) is parsed up to its last complete line and sets `truncated`
  (the log is chronological, so a capped answer holds the OLDER part of the requested lines),
  any other CLI failure gives `rows: []` and `problem`; cached 10 s per sandbox, at most 32 sandboxes. Persisted
  events: a time from 0 to now + 1 day; the rewrite keeps the newest 5000 valid lines, re-serialised; one ordered
  writer; stale temp files reaped on load; reads through a bounded ring. Rails: the main model and flow names must come
  out of `redact()` unchanged; a key past the first 256 entries of a mapping is not found; at most 32 distinct
  `models` items are looked at; at most 4096 directory entries are listed for config ids (beyond that, which ids appear
  is not defined). The Audit summary's `denied_total` is labelled for what it is: audited calls answered
  401/403 (requests refused before a handler are not audited).

**9.17 (2026-11-15): Relay runtime — gateway resources, plugin routing rates, runtime diagnostics.** Additive (new
metric keys on the existing Relay poll, a new admin route, new fields on the capture answer, two new Capture-tab
cards); no existing route or shape changes. Relay 0.11 brought both sources: a `resource_metrics` plugin component
(enabled here for the gateway PROCESS only, one sample a minute; samples reach Prometheus through the OTLP metrics
endpoint as `nemo_relay_resource_*{job="nemo-relay"}`, re-exported every 15 s between samples, and ATOF as
`nemo.relay.resource_metrics` marks of about 17 KB) and runtime diagnostics (bounded counters and recent events in the
gateway's memory, read only with the account's private gateway credential over Relay's TLS tunnel:
`nemo-relay diagnostics`; plain HTTP gets 426).

* **New Relay metric keys** (the adapter's `metrics()`; gauges stored and rolled up like every key, so the existing
  `GET /systems/relay/metrics/series` serves 15m-7d with no new route). Read from Prometheus with FIXED expressions
  only (no PromQL from a request): 4 queries (the pressure and routing rates share queries through `label_replace`
  and `or`), run concurrently, 1.5 s timeout and a 4 MiB answer cap each, and only when `urls.prometheus` is set and
  is a loopback http(s) URL without user info (the default settings have it; tests without it stay hermetic). The
  points are cached 15 s with one refresh at a time, so the 2 s poll and `GET /metrics` do not re-ask Prometheus. Where a
  gauge has several series the largest value is used; the p95 accepts only a label-less answer. A key is emitted only
  for a finite value >= 0; a failed or empty query omits its keys and never fails the snapshot. Labels are never
  forwarded (the filesystem label holds a home path). Keys and units:
  `relay.gw_cpu` cores (`..._cpu_consumption_rate_logical_processors`), `relay.gw_rss` B (`..._memory_resident_kibibytes`
  x 1024), `relay.gw_mem_accounted` B (`..._memory_environment_accounted_kibibytes` x 1024, the unit's cgroup),
  `relay.gw_threads` 1, `relay.gw_fds` 1 (`..._process_open_file_descriptor_count_file_descriptors`),
  `relay.gw_disk_read` / `relay.gw_disk_write` B/s (`..._disk_{read,write}_throughput_bytes_per_second`),
  `relay.atof_fs_free` B (min over series of `..._disk_filesystem_available_capacity_bytes`),
  `relay.atof_fs_used` ratio 0..1 (1 - available/total, same series), `relay.gw_cpu_pressure` / `relay.gw_mem_pressure`
  ms/s (`rate(..._{cpu,memory}_pressure_some_stall_time_milliseconds[5m])`), `relay.gw_limit_events` 1
  (sum of `..._limit_event_count_events`, cumulative since the gateway started), and for the Switchyard plugin inside
  the gateway: `relay.route_rps` req/s (`sum(rate(switchyard_routing_requests_total{job="nemo-relay"}[5m]))`),
  `relay.route_failures` req/s (same for `switchyard_routing_failures_total`, 0 when that counter does not exist yet
  but the requests counter does), `relay.route_tokens` tok/s (same for `switchyard_routing_llm_tokens_total`),
  `relay.route_overhead_p95` ms (`histogram_quantile(0.95, sum by (le)
  (rate(switchyard_routing_overhead_milliseconds_bucket{job="nemo-relay"}[5m])))`, falling back to the older
  `switchyard_routing_overhead_ms_bucket` name with `or`; absent while idle, when Prometheus answers NaN). There is
  deliberately NO sample-age key: Prometheus re-stamps the last value at every 15 s export, so an age read there
  cannot go stale; freshness comes from the ATOF marks (next point). Prometheus down = these keys absent,
  `available` unchanged.
* **Capture answer** (`GET /systems/{id}/capture`, Relay, viewer) gains `resources: {configured, enabled, scope,
  interval_s, gpu, network}` parsed from `plugins.toml`'s `resource_metrics` component (all six null when the file
  cannot be read; a component with `enabled = false` counts as not configured; `scope` identifier-checked, else null;
  `interval_s` = `polling.interval_millis`/1000 when polling is enabled, else null) and the level row id
  `resource_metrics` (it replaces 9.12's guessed row of that id; `detail` keeps its pinned shape `{polling,
  interval_ms}`, `observed` is `{last_ts}`, group "other"): enabled = configured and polling on; state `active` when
  the newest `nemo.relay.resource_metrics` mark in the ATOF index is no older than 3 x interval (60 s assumed when no
  interval is configured), `stale` when older, `idle` when enabled with none seen, `off` when not enabled, `unknown`
  when `plugins.toml` cannot be read or the index is off. When off, one hint with code `relay.resources.off`. The
  trace index keeps these marks out of traces, sessions, analytics, mark and orphan counts (they hang under the
  runtime root), keeps only the newest mark's time (no payload), and leaves them out of its reuse stamp. The ATOF and
  Payloads rows keep judging freshness by traffic only: resource marks alone never make them `active`.
* **`GET /api/v1/systems/{id}/diagnostics`**, Relay only (an unknown id 404s first, as on every system route; for a
  known id the role is checked before the system: ADMIN, else 403; other known systems 404; Rust-only D-44). A
  request carrying `Sec-Fetch-Site: cross-site` or `same-site`, or an `Origin` that does not match the console's own,
  is refused with 403 before anything runs (under loopback trust a GET from another page would otherwise start
  processes). The relay URL must be loopback http(s) without user info, else `reason: error` and nothing runs. Runs,
  through the adapter's existing command runner (same binary resolution as `plugins validate`, argv only, no shell,
  the runner's cleaned environment with the console's HOME so the CLI finds the account's gateway credential):
  `nemo-relay diagnostics --gateway-url=<relay url> --max-response-bytes 1048576`, then while the page carries a
  `continuation` object and fewer than 4 pages were read, the same with `--query {"continuation":<the object as
  received, compact>}` (never sent when over 3 KiB raw or 4096 URI-encoded bytes, never the same one twice), then
  `--memory --max-response-bytes 262144` when the snapshot returned JSON. 15 s per call, one 2 MiB output budget
  shared by all calls (no call with under 64 KiB left), the whole read under a 30 s deadline; JSON parsed off the
  async workers; cached 30 s (a failure 5 s), one read at a time (single flight). Pages are merged: totals, times,
  version and `loss` from page 1; components from all pages grouped by key; events deduplicated by sequence, the
  newest 256 kept. Answer:
  `{available, reason, relay_version, captured_at, elapsed_s, totals: {recorded, successes, failures, unspecified,
  evictions}, last_failure_at, components: [{key, instances, successes, failures, occurrences, last_failure_at}],
  events: [{seq, at, component, code, failure_class, severity, outcome, count}], loss: {<name>: n}, retention:
  {events_retained, oldest_seq, latest_seq, truncated}, memory: {reserved_bytes, events, event_capacity, detail_bytes,
  detail_capacity_bytes} | null}`. `retention.truncated` is true when events remained unread (the last page read was
  still truncated, a later page failed, the deadline or the page limit was hit, or a continuation was refused).
  `reason` is null or one of `disabled` (the CLI says inspection is unavailable: diagnostics or their HTTP read
  switched off on the gateway, or the account's credential or tunnel failed), `unreachable`, `unsupported_version`
  (no `diagnostics` subcommand, Relay < 0.11), `error`; with a reason every other field is null/empty. A failed memory
  call leaves `memory: null` with the rest available; a failed later page keeps the earlier pages. Numbers come from
  Relay's decimal strings (saturated or larger than 2^53 -> 2^53). Components: keys identifier-checked
  `[a-z0-9_.-]{1,96}` (others dropped and counted in `loss.console_dropped`, which is always present), at most 64,
  sorted by failures then key. Events newest first; `component`, `code`, `failure_class`, `severity`, `outcome`
  identifier-checked (else `"other"`); timestamps RFC 3339 as Relay prints them (checked, else null). `loss` only from
  an allowlist of Relay's `coverage.loss` names. NEVER forwarded: `detail` (Relay: "can contain private data that has
  not been sanitized"), `correlation`, `origin`, ownership ids, instance ids, process ids, `context`, the continuation
  object. Nothing of the CLI's own output is echoed.
* **Capture tab**: a "Gateway resources" card (every role): window 1h/24h/7d (default 24h, kept in the URL as
  `res_window`) through the existing series route; charts for CPU, memory (RSS + cgroup), threads + file descriptors,
  disk read/write, filesystem of the event files (free, used share), pressure and limit events, routing requests +
  failures, routing overhead p95, tokens/s; axis labels and tiles keep enough precision for tiny values (millicores);
  a latest-values strip from the metrics snapshot; a freshness chip from the `resource_metrics` row's
  `observed.last_ts`; when the capture answer says resources are off, an empty state with the plugins.toml component
  to add. A "Runtime diagnostics" card, built only for admins (not merely disabled; nothing is fetched for a viewer):
  totals tiles, a component table (failures first), failures by component as bars, recent events (code, component,
  severity, outcome, count, time) with an outcome filter and a note when `retention.truncated`, loss counters
  (non-zero highlighted), memory meters (event slots and detail pool used of capacity; reserved bytes as a figure),
  fixed texts per `reason`, a refresh button, auto-refresh every 30 s while the tab is visible.
