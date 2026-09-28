# Nemo Console — architecture & contract (v1)

Monitor, configure and interact with **NeMo Switchyard** (tier routing), **NeMo Relay** (agent gateway + observability)
and **NVIDIA Dynamo** (serving frontend + workers) from one local UI. Contract v1 — LOCKED. Changes after fan-out are
additive only, appended to §9 and relayed to affected agents immediately.

## 1. Decisions (ADR summary; full text in `docs/adr/`)

| # | Decision | Why |
|---|---|---|
| 1 | Hexagonal core + `SystemAdapter` port (`adapters/base.py`, LOCKED); one adapter per system | New system = one adapter; the UI renders from descriptors |
| 2 | Host-run FastAPI on `127.0.0.1:8793`, not a container | Controls rootless Docker + a systemd user unit; mounting the socket into a network service is root-equivalent |
| 3 | All process control through one `CommandRunner` (argv list, binary allowlist `docker`/`systemctl`/`nemo-relay`, timeout, output cap, redaction) | One audited choke point; adapters cannot shell out |
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
Frontend: `node --test tests/*.test.mjs` + headless-Firefox screenshots of every view (see `headless-ui-verify`).
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
