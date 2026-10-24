# Changelog

Format: [Keep a Changelog](https://keepachangelog.com/). Versions follow SemVer. Claims use the labels
implemented / verified-live / unverified defined in the README.

## [Unreleased]

### Added

- **NVIDIA Data Designer** (`datadesigner`) as a fifth system: the console drives the pinned
  `data-designer-local` CLI (0.9.3, installed by the lab's `clients/data-designer/install.sh`) the way it drives
  SkillEvaluator — on demand, keeping the reports. Status shows the launcher version, the LiteLLM router the
  generation goes through, the tool's own model providers/aliases (parsed read-only from `~/.data-designer/*.yaml`,
  `api_key` fields never surfaced) and one live create run at a time. Dataset config scripts (`.py/.yaml/.yml/.json`
  under the scripts dir, default `~/Datasets/data-designer/scripts`) are config files: edit, validate, diff, apply,
  revision history and rollback through the core's compare-and-swap machinery — validation runs
  `data-designer-local validate` and **executes the script locally** (the tool's design; stated in the UI). Actions:
  `preview` (sync, sample records + `records.jsonl` capped at 2,000), `create` (background, one at a time; full dataset
  plus a converted copy — parquet/csv/jsonl/json — under the datasets root, destination resolved and
  containment-checked before the run starts), `report`, `check_models`, `show_config`. Every script-executing action
  (`preview`, `create`, `check_models`) is `disruptive` — admin and `confirm:true`, because validate/preview/create
  EXECUTE the config script locally (the tool's design; stated in the UI and the threat model, E-7/R-19). Runs land in
  the state dir
  (newest 100) as `dd.run` metadata-only events feeding `dd.*` metrics; two additive viewer endpoints
  (`GET .../runs/{id}/records`, `GET .../runs/{id}/files`) power the UI's paginated records table. The runner
  allowlist gains `data-designer-local` and `data-designer-export` (a fixed-script conversion wrapper); only
  `data-designer-local create` gets the long timeout (3,600 s), everything else keeps 600 s. The wrapper injects its
  own credentials (`LITELLM_DD_KEY` from a 0600 key file), so the console never sees a key.
  Verified live on 2026-10-24 against the real CLI: status (0.9.3, 15 aliases), script validate through the API,
  `preview` (2 records through the router on `glm-5.3-flash-next-local`), `create` with a csv copy under the datasets
  root, the records/files endpoints, and the UI page (status, actions, runs list) screenshotted at 1440/884 px.
  Unverified live: run detail's records table in the browser (mock-verified only), console-stop cancel of a create.
- New lab client `clients/data-designer/` in the parent repo: pinned install (`uv venv` 3.13 +
  `data-designer==0.9.3`), the policy wrapper (`data-designer-local`, telemetry forced off), the `data-designer-export`
  converter, a router key minter (key_alias `data-designer`), and the vendored NVIDIA agent skill at
  `.claude/skills/data-designer` (see the parent repo).

## [0.2.0] - 2026-10-17

### Added

- **SkillEvaluator Tier 3** (live agent evaluation, "Skill Lift"): action `evaluate_live` (`disruptive`: admin and
  confirm, because it spends router tokens and runs an agent). It runs `skillevaluator-local tier3 SKILL --n-attempts N
  --agent-model opencode=ALIAS --judge-model ALIAS --dataset auto|existing|generate [--skip-baseline] --results-dir
  <run dir> --quiet` in the background (one run at a time; a second request is refused), returns at once with the run
  id, and fills `run.json` from `running` to the verdict (result.json `execution_status`), the Skill Lift per agent and
  per metric, scored/expected attempts, duration and the first conclusions/recommendations. A console stop marks the run
  `interrupted` (the runner sends the launcher SIGINT first); a run left `running` by a restarted console reads as
  `interrupted`. Plain `skillevaluator` is refused for Tier 3 (its stock sandboxes have open network). The runner allows
  4800 s for this one call; every other command keeps the 600 s cap. New metrics `se.live_running`, `se.last_skill_lift`;
  `se.runs_by_verdict` gains `succeeded` and `interrupted`; the Skills page shows both.
  Verified live on 2026-10-17 by calling the adapter in-process (the console's own code and settings, a scratch
  state dir) on the author's deployment: `robust-scripts`, opencode on `nemotron-lightning`, judge
  `nemotron-lightning-judge`: succeeded, 2 of 2 attempts scored, Skill Lift +0.240 stored in `run.json`, a second
  request refused as busy. **Through the HTTP API and the UI it is unverified.**
- `topology_engines` setting (the engine nodes of the topology) and `switchyard.tier.capable_not_deployed`.
- A fourth system, **NVIDIA SkillEvaluator** (`skillevaluator`): a Skills page that evaluates the agent skills under
  `<repo>/.claude/skills` with SkillEvaluator 0.5.0. Tiers `static` (offline), `tier1` (adds the LLM judge) and `tier1+2`
  (adds embedding deduplication); Tier 3 came later, through the wrapper only (entry above). Every run keeps its
  JSON + Markdown report in the state dir (newest 200), shows up as a `skilleval.run` event, and feeds `se.*` metrics.
  The console runs `skillevaluator-local` when it is on PATH (a wrapper that reads its own key file, so the console never
  holds a provider key), else `skillevaluator` with `SKILL_EVAL_*` from the console's environment; those variables reach
  the SkillEvaluator binaries only. Verified live on the author's deployment: status, a `tier1+2` evaluation through the
  API (judge and embeddings through LiteLLM, 10 s) and headless screenshots of the page at 1440 and 600 px.

### Fixed

- Dynamo lifecycle against a stack started from more than one compose file. The live stack on the author's deployment
  runs from `docker-compose.yml` + `docker-compose.super-vl.yml` (Super VL: nginx gateway at `dynamo-frontend`, the real
  frontend `dynamo-sv-frontend`, worker on both GPUs), but the console knew only the base file: its `up`/`restart` would
  have replaced Super VL with the 27B stack, its `down` would have orphaned `dynamo-sv-frontend`, and Params would have
  rewritten the worker. Up/down/restart/config checks now use exactly the files in the containers' compose label
  (untrusted input: each must resolve inside the configured compose directory, `working_dir` must match, the containers
  must agree; otherwise everything refuses, with no fallback to the base). With an override file in use, Params and the
  console override are refused with the reason. The worker's GPUs come from the merged `docker compose config` (both
  cards for Super VL). Verified live read-only on 2026-10-17 (status, preflight, the refusals); the lifecycle calls with
  those files are fakes only.
- The Dynamo version showed the nginx gateway's image digest; it is now the first Dynamo image tag (worker first), never
  a digest (`1.6.0-nemotron-3.5-super-vl-dev.1`, read live). The real frontend behind the gateway is a component and a
  log source.
- Fixes that would tear down the running layout. `dynamo.holds_card` advised `scripts/use-model.sh qwen3.8-flash-next`
  (it stops Super VL); it is now info and its fix lists the engines only. Engine start commands say what they stop.
  A capable tier whose container does not exist (flashnext, on 2026-10-17) no longer degrades Switchyard; it is the info
  problem `switchyard.tier.capable_not_deployed`. The topology draws missing engines `off` "(not deployed)".
- The Switchyard and Relay playgrounds defaulted to `auto-local`, which escalates to an engine that is no longer
  deployed; the default is now `super-lightning` when it exists (what LiteLLM's `switchyard` alias forwards to), else the
  first route. The Relay playground sent no key (401 from the gateway); `playground_key_env` now maps `relay` to
  `LITELLM_API_KEY` by default.
- Relay events skipped gzipped ATOF rotations (`relay-events.jsonl.2.gz`); they are read with size caps (verified on the
  real file, read-only).
- SkillEvaluator's install fix suggested an unpinned `git+https://github.com/NVIDIA/SkillEvaluator.git`; it is now the
  lab's pinned installer when the repo has it, else the same command pinned to `7304d76` (v0.5.0). A `tier1+2` run whose
  Tier 2 lacked embeddings now says so (`skillevaluator.tier2.no_embeddings`, fix: start the lab's Ollama).
- The Relay liveness probe no longer floods Langfuse. It polled `GET /v1/models` on the gateway, which forwards
  it to LiteLLM; without a bearer every poll was a 401 that LiteLLM logged to Langfuse as an ERROR trace
  (about 43,000 a day at the 2 s poll, 98% of all traces on the author's deployment). Liveness now uses the
  gateway's own `/health` (404 = up) and the upstream check uses LiteLLM's unauthenticated `/health/liveliness`,
  which leaves no trace (verified live). New problem code `relay.gateway.error` for a 5xx from the gateway
  itself; `relay.gateway.upstream_error` now means LiteLLM is not answering. The URL comes from the existing
  `litellm` setting.
- A Dynamo that is off on purpose is no longer an error. On the author's deployment Dynamo is opt-in (the default model
  holds the card), yet a stopped Dynamo reported `health=down` with error-level `dynamo.frontend.down` /
  `dynamo.worker.stopped`, which kept `/api/v1/health` `degraded` for good. With the new setting `dynamo_opt_in` (default
  true) and no Dynamo container running or restarting, Dynamo is `health=off` with one info problem `dynamo.off`, and
  `/health` ignores systems that are `off`. Anything running or restarting keeps the error-level report.
- A crash-looping Dynamo container was read as running (Docker sets `Running` during a restart back-off); `Restarting` now
  wins, so the loop shows as a failed worker ("restarting (crash loop)").
- Wrong start and restore commands in Dynamo fixes. Every start fix was a bare `cd inference/dynamo && docker compose up -d`,
  which beside the default model crash-loops the worker (it happened on 2026-09-28); it is now the setting `dynamo_start`
  (default `scripts/use-model.sh qwen3.8-27b-dynamo`). `dynamo.holds_card` advised restoring the 27B and named
  `qwen3.8-27b-1m` as dark; its fix is now `default_model_start` (default `scripts/use-model.sh qwen3.8-flash-next`) and
  it names what is really dark (setting `dynamo_dark_while_held`: the flashnext aliases, the auto-local and
  auto-flashnext capable tiers, `direct-flashnext`). Both commands are display only.
- Dynamo `up` beside harmless containers. Any running container on the worker's GPU refused `up` (lightning, ollama, even
  the GPU exporter), and the fix said `docker stop <name>`. Coexisting and ignored containers no longer block; an
  exclusive engine (the default model) refuses `up` with `dynamo_start` as the fix, never a `docker stop`; an unknown
  container still refuses (safe default) but is named with an identify-first fix. `dynamo.card.contended` now fires for
  ANY exclusive engine on the card (it only knew `vllm-qwen38`), also while the worker is crash-looping.
- `switchyard.capable_tier.upstream_errors` missed a capable tier that never succeeded. Switchyard's `/v1/stats`
  `calls` counts successes only (calls + errors = attempts: 56 + 5 = 61 `total_requests` read live 2026-10-06), so
  `calls > 0 and errors * 2 >= calls` never flagged `cluster/glm-5.3-flash` at 0 calls and 2 errors. It now fires when
  errors >= successes (errors > 0), or when errors rose since the last poll and successes did not; the message says
  "N errors, M successes".
- Topology drew the opt-in `vllm-qwen38` (and a stopped Dynamo) red; opt-in engines are now `off`.
- Relay playground fallback route ids gain `direct-flashnext` (it is in the deployment's `routes.toml`).

### Changed

- Engine-layout defaults follow the author's deployment of 2026-10-17 (ARCHITECTURE 9.6): exclusive with Dynamo = Bolt,
  GLM, flashnext, the 27B; coexisting = lightning, ollama, chatterbox-tts; new engine probes for Super VL, Bolt and GLM.
  All are settings.
- The runner stops a cancelled command's whole process group (it used to leave it to asyncio), with SIGINT and a 5 s
  grace first for the two SkillEvaluator launchers.
- `dynamo.structural_tag.off`, `dynamo.parser.dialect` and `dynamo.router.kv` are raised for the base stack only.
- `Health` gains the value `"off"` (additive; ARCHITECTURE 9.4). The UI shows it as a neutral badge (○, dashed tile and
  node), never red; the topology legend says so.
- Dynamo card model is settings-driven: `dynamo_exclusive_engines`, `dynamo_coexisting_engines`,
  `dynamo_ignored_containers` (defaults: the author's flashnext + 27B, lightning + ollama, the dcgm exporter) replace the
  single `SHARING_ENGINE` constant. `stop_sharing_engine` stops only the 27B (`STOPPABLE_ENGINE`), never the default
  model; its help, the `up`/`down` descriptions and the Lifecycle confirm texts say so. The confirm dialog lists every
  container on the card and warns when `up` will be refused.
- Dynamo preflight output gains a `stoppable:` line (the one container the param may stop, or `none`) and the problem
  `dynamo.card.unknown`; `router_aliases_dark: none` no longer shows as an alias called "none" in the UI.
- Topology: a node for the always-on lightning engine (the efficient tier of every route); the clients -> Switchyard edge
  is labelled `default` (opencode's default model is a Switchyard route) and stays active while Switchyard is degraded.
- The UI mock data uses the deployment's capable targets (`flash-next-auto` for Switchyard `auto-local`, `flash-next` for
  Relay) and the mock server has a Dynamo `off` scenario.
- ADR-0009 addendum: the card is shared with the TP2 default model and lightning, not with `vllm-qwen38` alone.

## [0.1.2] - 2026-09-28

### Added

- `nemo-console open` / `nemo-console stop` and `make install-desktop`: a desktop launcher (Linux). `open` starts the
  console in the background if nothing is serving, waits for `/api/v1/health`, then opens the UI. It uses loopback
  addresses only, and opens a page only when every listener on the port belongs to the current user and the answer
  looks like Nemo Console (a same-user process can still impersonate it: THREAT-MODEL S-6). The server runs with
  `python -I` so the launch directory is not on its import path. `stop` signals only the process it started. The
  launcher never handles keys.
- An app icon, also served as the UI's favicon (`frontend/favicon.svg`).

## [0.1.1] - 2026-09-28

First public release. 0.1.0 was an internal build and was never published; this release starts a fresh
history.

### Added

- `SECURITY.md` (private vulnerability reporting through GitHub) and a CI workflow: ruff, mypy --strict, pytest and
  the frontend `node --test` suite, with actions pinned to commit SHAs.
- Package metadata (licence, author, project URLs) and a non-affiliation notice in the README.

### Changed

- `switchyard.tier.capable_dark` matches only the `capable_target` models of `stage_router` routes in the parsed
  `routes.toml`. It used to match the alias anywhere in the file text, so an engine referenced only by a passthrough
  (`direct-27b`) or a target table kept Switchyard `degraded` while that engine was stopped on purpose. The message
  names the routes; an unparseable file yields no problem.
- The `capable_dark` fix is the container's start command from the new `engine_start` setting (`ENGINE_START`) when
  one is set, else the old compose-directory guess. Defaults for the author's deployment:
  `inference/sglang-flashnext/flashnext.sh up` and `scripts/use-model.sh qwen3.8-27b-1m` (a bare compose up is wrong
  for both).
- Default `engine_probes` adds `qwen3.8-flash-next` -> `ai-lab-sglang-flashnext-flashnext-1` (the deployment's default
  engine and capable tier since 2026-09-26); the 27B entry stays.
- Topology shows the flashnext engine (SGLang TP2, one rank on each card) beside `vllm-qwen38`.
- The Dynamo `dynamo.holds_card` fix restores the 27B with `scripts/use-model.sh qwen3.8-27b-1m` instead of a bare
  `docker compose up -d`.
- Test fixtures captured from the author's workstation are sanitised: home directories, host paths, GPU UUIDs, MAC
  and LAN addresses are neutral placeholders. Parsers are exercised on the same structure.
- Problem fixes and test skips no longer name the author's deployment repository.
- `LICENSE` names the actual copyright holder.

## [0.1.0] - 2026-09-24

Internal build, not published. Built against NeMo Switchyard 0.3.0, NeMo Relay 0.9.2 and NVIDIA Dynamo 1.5.0.

### Added

- Hexagonal core with a locked `SystemAdapter` port and three adapters (Switchyard, Relay, Dynamo).
- Loopback-bound FastAPI service (default `127.0.0.1:8793`) and a zero-build, zero-npm browser UI with a
  strict CSP: Overview, Switchyard, Relay, Dynamo, Audit and Settings views.
- Observe: status with actionable problems, canonical metrics, 15m/1h/24h history (in-memory ring plus SQLite
  rollups), events (list and SSE), routing summary, topology.
- Configure: core-owned config store (SHA-256 compare-and-swap, backup, atomic write, revisions, rollback,
  auto-rollback on failed health check); Switchyard `routes.toml` with authoritative dry-run validation; Relay
  `routes.toml` with a declared non-authoritative validator; Relay `plugins.toml` and `config.toml` read-only;
  Dynamo parameter catalog rendering a generated compose override.
- Act: danger-levelled actions with mandatory confirmation; Dynamo preflight/up/down/restart/up_mock with
  shared-GPU awareness and an opt-in, label-verified stop of the sharing engine.
- Interact: streamed, non-persisted playground with a server-side credential resolved from an env-var name.
- Security: Host allowlist, Origin and `X-Console` CSRF checks, hashed bearer keys with admin/viewer roles,
  loopback trust (switchable), forwarded-header distrust, body/SSE caps, single argv-only command runner with a
  three-binary allowlist, central redaction, append-only audit, security headers on every response.
- `nemo-console mint-key [admin|viewer]`; first-start admin key file (0600).
- Documentation: README, 14 ADRs, threat model, operations guide, feature matrix.
- Opt-in systemd user unit template in `deploy/`.

### Changed during the release cycle (docs updated to match the final code)

- `trust_loopback` defaults to **false**: an unauthenticated caller is role `public` and receives 401 everywhere except
  `GET /api/v1/health`. Loopback trust is forced off for a non-loopback host or when `allowed_hosts` is set
  (`/health.trust_notice` says why). The UI prompts for a bearer.
- Audit records only authenticated, authorised mutating calls. Denials are counted in
  `nemo_console_denied_requests_total{status}`, kept in a 100-entry ring, and logged at most once a minute.
- Rollback re-runs the validator and secret scan on the target revision (422 before writing).
- Dynamo hand-edited override is a closed allowlist (no single-dash/unknown flags, `$` only when byte-identical to a
  base-compose token, model flags equal to the base, env keys must exist in the base, fail-closed without a base).
- Switchyard and Relay routes validators error on `base_url` hosts that are not loopback and not a bare single-label
  service name, and on userinfo.
- New status problem `switchyard.tier.capable_dark` (setting `engine_probes`), making Switchyard `degraded` with summary
  prefix "capable tier dark"; shares one cached `docker inspect` probe with the topology view. Dynamo preflight emits
  `shares_card:` and `router_aliases_dark:` lines.
- Preview `warnings` are wired; default action timeout is 900 s with per-action overrides (`action_timeouts`);
  `reset_stats` needs no credential and the `switchyard.auth` problem code was removed; `/health` adds `role` and
  `trust_notice`; `/settings` shows `state_dir` `~`-relative to non-admins; the playground sends
  `Accept-Encoding: identity`; validate calls are capped by a semaphore of 2; the SSE slot is taken inside the generator;
  `NEMO_CONSOLE_URLS__X` merges over defaults; the repo-root default walks up to the compose files and a missing root is
  a reported problem; frontend stream errors parse the error envelope; the audit params blob is valid JSON by
  construction (oversize -> `{"_truncated":true,...}`); redaction and secret scanning are bounded and linear; static
  serving rejects `..`, `//`, backslash, NUL, `.` segments and encoded forms before resolving; viewers receive config
  content with literal secrets redacted; playground credential names set via the Settings API must be `LITELLM_*`,
  `NEMO_CONSOLE_PLAYGROUND_*` or `*_API_KEY`; URL userinfo is redacted in audit; one semaphore(2) covers validate, apply
  and rollback; Switchyard routing events keep only identifier-shaped route/algorithm/model/tier/session_id; the Dynamo
  override allowlist also rejects duplicate flags and restricts `--speculative-config`; an absent override reads as empty
  and the first apply creates it. See `docs/PUBLISHING-CHECKLIST.md` for what remains before a public release.

### Verification at release

- Offline gates (re-run 2026-09-25 for the final code): backend pytest 340 passed / 3 skipped in the repo checkout
  (a fresh-directory copy without `.git`: 338 passed / 5 skipped; not a `git clone`), ruff clean, mypy --strict clean (40 files), frontend node --test 94 passed.
- Verified live (maintainer-reported, one workstation): Switchyard config validate, stale-sha 409, apply with container
  recreate and health check; Dynamo preflight, `down` (400 without confirm), `up`, then serving again; playground chat 200
  through a Switchyard route; Relay `doctor_offline`, `gateway_status`, `agents_list`, `plugins_validate`,
  `gateway_restart` (PID changed, three consecutive healthy checks); the viewer role (reads 200, audit/validate/playground/actions
  403); auth behaviour
  (public 401, wrong key 401, bootstrap key -> admin); the capable-tier-dark problem; unauthenticated
  `POST /v1/stats/reset` -> 200; status/metrics/events read paths for all three; Host/Origin/CSRF/X-Forwarded-For and
  security headers; frontend rendered against the real backend (headless Firefox).
- Not verified live (fakes only): config auto-rollback, Dynamo parameter apply,
  `stop_sharing_engine`, Dynamo `restart`, assistive technology, browsers other than headless Firefox.

### Known limitations

- Capable-tier-dark detection has no request-level probe: it is a stopped-container check for containers named in
  `engine_probes` plus an error-count heuristic; a running-but-broken engine is not detected.
- Playground served-by headers are guessed and were absent from real Switchyard responses.
- Topology edges are approximations, and the edge label "agent gateway" is partly clipped behind a node.
- Relay `plugins.toml` / `config.toml` are read-only by design. The Dynamo tab has no config-file tab.
- Dynamo events are redacted log lines, not an allowlist of metadata fields (ADR-0010 deviation).
- The adapters embed container, project and unit names; the default `engine_probes` entry and one topology container name
  come from the author's deployment. Set `NEMO_CONSOLE_REPO_ROOT` for a standalone checkout.
- `NEMO_CONSOLE_HOST` can be set to a non-loopback address; nothing forbids it (loopback trust is then forced off).
- Route validators accept single-label hostnames as service names; a hostile resolver search domain could send them
  off-box. Redaction is chunked (4000 chars): a secret straddling a chunk boundary could slip through. The Dynamo
  override allowlist does not check enum-valued flags against catalog choices or type-check numbers. The systemd unit
  template is untested (namespace-based options can fail with 226/NAMESPACE on restricted distributions).
- Only the alias-to-container mapping is configurable (`ENGINE_PROBES`); other adapter names are constants.
- The Relay playground has no default credential mapping.
