# Changelog

Format: [Keep a Changelog](https://keepachangelog.com/). Versions follow SemVer. Claims use the labels
implemented / verified-live / unverified defined in the README.

## [Unreleased] - 2026-10-29

### Added

- **NVIDIA Model Optimizer as the eleventh managed system** (addendum 10.9). Status (the wrapper's version, the
  recipe file, the last job), the `mo.*` metrics, the editable `recipes.yaml`, a `run` action that starts a
  compression job in the background and a safe `report` action. The console starts the lab's `modelopt-local`
  wrapper and reads each job's `run.json`; it never runs the compression code, never writes a job's files and
  never stops a running job ([ADR-0017](docs/adr/0017-model-optimizer-is-a-background-job.md)). Implemented with
  fakes only (`tests/modelopt_adapter.rs`); nothing about it has been seen live yet.

## [0.4.0] - 2026-10-28

### Changed

- **The backend is rewritten in Rust** (one binary, `backend/target/release/nemo-console`; axum + hyper on tokio,
  rusqlite, reqwest). Same API, same settings (`NEMO_CONSOLE_*` and the env file), same state directory and database
  (a 0.3 `console.db` is read as is and the other way round), same CLI (`open`, `stop`, `mint-key`), same log lines and
  exit codes. The Python backend is gone from the checkout; its last revision (main at `aa2f810`) stays the behavioural
  reference of the parity harness (`make parity`, `parity/ref.mjs`). Developer tasks: `make setup` builds with cargo,
  `make all` runs rustfmt, clippy, `cargo test`, cargo-deny, cargo-audit, a 14-day crate cooldown, the frontend tests and
  the browser e2e. Implemented and checked against the Python reference with fakes (1,393 cases, 1,392 equal, the one
  difference documented as PORTING D-6); every "verified live"
  in the docs was observed with the Python backend and has NOT been repeated with the Rust one (FEATURE-MATRIX).
- **Deviations from 0.3** (each with its reason in `backend/PORTING.md` section 13): parser limits are fixed instead of
  stack-dependent (JSON bodies nested past 512 levels get the 400 Python gives past ~9,998; TOML/YAML past Python's
  recursion depth fail with Python's RecursionError text); offset-less timestamps are read as UTC, not local time;
  ints beyond i128 in JSON become floats; a NaN in Dynamo params is refused by the core check (422 `invalid_param`)
  rather than by the adapter (422 `params_invalid`); YAML sets keep document order; Data Designer copies do not copy
  extended attributes; hyper answers a few malformed requests uvicorn answered itself; multi-range static requests
  get the whole file. Everything else matched the Python backend case by case.
- **Hardening found by a pre-release audit** (`backend/PORTING.md` A-9, deviations D-26 to D-29): every route's role
  is enforced by the request pipeline before its handler (handlers keep their own check); a request head must arrive
  within 5 s, a body within 30 s (408), and at most 256 connections are open at once (uvicorn had no such limits);
  panics in background work are caught and logged redacted, and a poisoned lock no longer locks everyone out;
  redaction and the secret scan fail closed when the regex engine gives up; upstream URLs are also parsed with the
  HTTP client's own parser and refused when the two disagree; Data Designer and AIPerf run-file reads refuse symlinks
  and are size-capped, the top-level static files must resolve inside the frontend directory, and private files are
  written through an exclusive 0600 temp file; header values are decoded as latin-1, as Starlette does, so a
  non-ASCII `Origin` still meets the Origin check. Run ids take their seconds and milliseconds from one clock
  reading (Python's could fall a second behind at a second boundary and sort out of order). The systemd template
  sets `MemoryMax` and `TasksMax`.
- **Upgrade:** `git pull`, `make setup` (needs `rustup`; the toolchain is pinned), restart the console, and point any
  launcher or systemd unit at `backend/target/release/nemo-console` (`make install-desktop` again; edit
  `deploy/nemo-console.service`'s ExecStart). Old venvs under `backend/.venv` can be deleted.

### Fixed (Python failures the Rust backend does not reproduce)

- A Switchyard, Relay or OpenShell config with absurdly deep nesting, a key of more than 1,000 parts or a 4,301-digit
  int no longer answers 500 from validate (Switchyard/Relay: a diagnostic; OpenShell keeps Python's exception class).
- A malformed entry in `docker inspect` output no longer fails the Dynamo status call (it is skipped).
- Recursive YAML aliases and alias bombs: the loader shares objects like PyYAML (no blow-up); the adapters that walk
  plain values refuse a cycle or more than 1,000,000 values instead of recursing.

### Known Python bugs kept on purpose (fix both together later)

- Data Designer `_export_batches`: once `records.jsonl` holds 2,000 lines, later batches are silently missing from the
  user's csv / jsonl / json copy.
- Data Designer does not catch KeyError / TypeError from YAML constructors; the evaluator does not catch
  UnicodeDecodeError on `suites.yaml`; SkillEvaluator reports raise format errors on odd `run.json` values; an
  impossible YAML date (`2026-02-30`) answers 500 in Guardrails, AIPerf and OpenShell validate.

### Also in 0.4.0: the last Python changes after 0.3.1 (ported with the rest)

#### Fixed

- **A signed-out tab no longer polls forever.** Every view refreshes on a timer, and after a 401 the UI showed the sign-in
  dialog but kept sending the refused requests: one `/overview` call every 5 s per tab (12 a minute; 24 a minute were seen
  in the console's denied-request log with two such tabs open). After a 401 the API client now holds every request made
  with the same key (or with none) and answers it locally with a 401, until the user signs in again (even with the same
  key) or a request succeeds; `/health` stays open. Verified live with headless Chrome against a scratch console: a
  signed-out page sent one `/health` and one `/overview` in 30 s (the 0.3.1 frontend: six refused `/overview` calls).

#### Added

- **Deployment names and the repo layout are settings** (ARCHITECTURE addendum 9.11). New `NEMO_CONSOLE_*` names:
  `SWITCHYARD_CONTAINER`, `SWITCHYARD_COMPOSE_PROJECT`, `SWITCHYARD_COMPOSE_SERVICE`, `SWITCHYARD_IMAGE`,
  `SWITCHYARD_LOG_PATH`, `RELAY_UNIT`, `DYNAMO_COMPOSE_PROJECT`, `DYNAMO_SERVICES`, `DYNAMO_CONTAINERS`,
  `GUARDRAILS_COMPOSE_PROJECT`, `DCGM_COMPOSE_PROJECT` and `LAYOUT` (each system's directory under the repo root;
  `resolved_paths()`, the repo-root check and the lab helper hints follow it). Defaults are the former constants, so an
  existing deployment behaves as before. Every value is checked with a full match at start and none is writable through
  the API. Still constants on purpose: Dynamo's one stoppable container, OpenShell's sandbox network and router address.
  Implemented; covered by offline tests; the defaults were exercised live through a second console's status reads.
- **Opt-in active capable-tier probe** (`NEMO_CONSOLE_CAPABLE_PROBE=true`, off by default). For each capable-tier alias
  whose container is running, one 1-token chat completion through the LiteLLM router (response cache bypassed), in the
  background, one at a time, at most every 5 minutes per alias, retried once before it counts. A running engine that
  does not answer is the new problem `switchyard.tier.capable_broken` (Switchyard `degraded`, "capable tier not
  answering"); a missing or refused key is the info problem `switchyard.tier.probe_unconfigured` /
  `switchyard.tier.probe_denied`. The key comes from the env var named by `CAPABLE_PROBE_KEY_ENV` (default
  `LITELLM_API_KEY`); only the verdict and an HTTP status are kept or logged. Verified live 2026-10-26 against the real
  router (ok, denied and unconfigured); the broken path with fakes only. Each probe is a real request billed to that key
  (THREAT-MODEL E-12, R-24).

#### Changed

- The mock server's default port is 18796 (was 8796, which another local tool on the author's machine uses).

## [0.3.1] - 2026-10-24

### Fixed

- **The five 0.3.0 pages showed "Something went wrong" in a browser that had used 0.2.x.** Frontend files were served
  without `Cache-Control`, so browsers kept the old ES modules heuristically (for days, going by their Last-Modified
  date) and paired them with the new views; the old `format.js` has no `fmtUnit`, so GPU telemetry, AIPerf,
  Guardrails, OpenShell and Evaluator failed their import while every older page worked. Every frontend file is now
  served with `Cache-Control: no-cache` and answers a matching `If-None-Match` with 304. Reproduced in headless
  Chrome with the 0.2 `format.js` and the 0.3.0 views (exact error: "The requested module '../format.js' does not
  provide an export named 'fmtUnit'"); the fix is covered by a backend test and the new browser suite. A browser
  that still holds files from before 0.3.1 needs one reload with Ctrl+Shift+R.
- A page whose code fails to load now says how to recover (reload with Ctrl+Shift+R) next to the error.
- Found by the new browser suite on a fresh clone: the Dynamo Params preview answered 500 when the repo compose was
  missing (the adapter's `ParamError` was not caught); a refusal by the adapter is now a 422 with its reason.
- Config tabs of a system that is not deployed showed "Something went wrong: config file does not exist"; a 404
  now reads "This file is not on this machine" with the reason and a Retry button.

### Added

- **Browser end-to-end suite** (`frontend/e2e/run.mjs`, `make e2e`): the real backend (throwaway HOME and state,
  loopback not trusted, signed in with the bootstrap key the way a person does) and a real headless Chrome driven
  over the DevTools protocol with Node 22's built-in WebSocket (no npm, ADR 0013). The backend gets an empty repo
  root, so every run sees what CI sees (no system deployed). It opens every route and every tab and fails on an
  error or not-found state, an uncaught exception, a console error, an API 5xx, or a frontend file that does not
  load or lacks `Cache-Control: no-cache`. Part of `make all` and a new CI job `e2e`.
- **Ship gate**: a versioned git `pre-push` hook (`.githooks/pre-push`, enabled by `make hooks`, which `make setup`
  now runs) refuses to push a commit or tag unless the e2e suite passed on its exact `backend/` and `frontend/`
  trees (`scripts/e2e-gate.sh`, record kept in `.git`); when the pushed commit is the clean HEAD it runs the suite
  first. A Chrome that is not on PATH is named once per checkout with `git config nemo-console.chrome <path>`.

## [0.3.0] - 2026-10-24

### Added

- **Five more systems** (ARCHITECTURE addendum 9.8), each one adapter file plus a registry line, a generic
  descriptor-driven view and a nav group (Routing · Engines and load · Safety and sandboxing · Evaluation and data):
  - **OpenShell** (`openshell`): the sandbox gateway behind Data Designer. Status (CLI, gateway, forwarder,
    connection, version skew), security checks as problems (`openshell-docker` must be `--internal`, no
    non-loopback published port), `os.*` metrics, sandbox events; trust files read-only (ADR 0007); actions
    `doctor`, sandbox list/get/logs, gateway logs, `prove_policy`, `gateway_up`/`gateway_stop`, `delete_sandbox`.
  - **DCGM** (`dcgm`): this host's dcgm-exporter plus every DCGM host Prometheus scrapes; `dcgm.*` metrics per GPU
    and host; `dcgm.exporter.vulnerable` below 4.8.3 and a live `/debug/pprof` probe (CVE-2026-47483); the
    exporter's `counters.csv` editable with a field-name validator and rollback; per-GPU `dcgmi diag` (levels 1 and
    2, both disruptive) and `discovery` in a hardened throwaway `--network none` container from the full DCGM image.
  - **AIPerf** (`aiperf`): load-test presets (`clients/aiperf/profiles.yaml`, editable), one background `profile`
    run at a time through the lab wrapper `aiperf-local`, `ap.*` metrics (TTFT, ITL, latency, throughput, errors).
    The tokenizer vocabulary is vendored in the lab (hash-pinned); `ap.no_tokenizer` points at restoring it, never
    at a download.
  - **NeMo Guardrails** (`guardrails`): a rails server in the lab (`security/nemo-guardrails`, internal network,
    key held by a broker), rails configs editable (validated in a `--network none` container; restart and
    served-check, rollback; a candidate that removes rails needs an explicit marker, ADR 0015), a `check` action
    with fixed probe prompts and a playground through the rails.
  - **NeMo Evaluator** (`evaluator`): offline benchmark suites (`clients/nemo-evaluator/suites.yaml`, editable;
    rule-scored benchmarks only) run in the background through `evaluator-local`, `ev.score{bench,model}`.
  The runner allowlist gains `openshell`, `aiperf-local`, `guardrails-local`, `evaluator-local`; only
  `aiperf-local profile` (3,600 s) and `evaluator-local run` (4,800 s) get long caps. No env reaches any of them:
  each lab wrapper reads its own 0600 router key file. Verification per feature: `docs/FEATURE-MATRIX.md`.

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
  EXECUTE the config script locally (the tool's design; stated in the UI and the threat model, E-7/R-19).
  Since 2026-10-24 every script-executing invocation runs inside an OpenShell sandbox (Landlock FS,
  default-deny network, router key held by the OpenShell credential store — never in the sandbox or the
  console; the wrapper is key-free). Runs land in
  the state dir
  (newest 100) as `dd.run` metadata-only events feeding `dd.*` metrics; two additive viewer endpoints
  (`GET .../runs/{id}/records`, `GET .../runs/{id}/files`) power the UI's paginated records table. The runner
  allowlist gains `data-designer-local` and `data-designer-export` (a fixed-script conversion wrapper); only
  `data-designer-local create` gets the long timeout (3,600 s), everything else keeps 600 s. The router key lives
  in the OpenShell gateway's credential store (the wrapper reads no key file), so the console never sees a key.
  Verified live on 2026-10-24 against the real CLI: status (0.9.3, 15 aliases), script validate through the API,
  `preview` (2 records through the router on `glm-5.3-flash-next-local`), `create` with a csv copy under the datasets
  root, the records/files endpoints, and the UI page (status, actions, runs list) screenshotted at 1440/884 px.
  Unverified live: run detail's records table in the browser (mock-verified only), console-stop cancel of a create.
- New lab client `clients/data-designer/` in the parent repo: pinned install (`uv venv` 3.13 +
  `data-designer==0.9.3`), the policy wrapper (`data-designer-local`, telemetry forced off), the `data-designer-export`
  converter, a router key minter (key_alias `data-designer`), and the vendored NVIDIA agent skill at
  `.claude/skills/data-designer` (see the parent repo).

### Fixed

- **`nemo-console stop` could not stop a console with a browser open.** uvicorn waits for open connections on
  SIGTERM and an SSE stream never closes on its own, so the process lingered until killed and `stop` gave up after
  10 s. The server's exit hook now ends every open stream at once with a final `event: shutdown` (the UI reconnects
  when the console is back), and uvicorn gets a bounded graceful shutdown (`shutdown_grace_s`, default 5 s) as a
  backstop. Verified live: exit in 0.27 s with two streams open, no error logged.
- **TTL caches that looked fresh right after boot.** Three caches (Guardrails isolation, DCGM diag image, OpenShell
  gateway) were stamped 0.0 and compared with `time.monotonic()`, which counts from boot: on a machine up for less
  than the TTL the empty initial value was served and the check skipped. Now stamped `-inf`; regression tests pin
  the clock (CI on a fresh runner caught it).
- **DCGM per-GPU charts were empty**: the `model` label held spaces, which the history API's series keys do not
  allow; labels are slugs now, and the frontend sorts label keys like the backend.

### Security

- **Threat model E-7/R-19 corrected** (docs only; no console code changed). The R-19 residual published in
  `4448a94` was incomplete. An independent review of the lab's OpenShell runner found two gaps, both fixed in the
  parent repo's runner the same day (verified-live): the sandbox network had open internet egress (now an internal
  Docker network; the wrapper refuses to run otherwise), and the artifact copy-back trusted sandbox-supplied paths,
  so a planted filename could make the host write outside the run dir (now NUL-delimited, symlink-free,
  traversal-refusing and confined to the run dir, with offline tests). R-19 now also states a residual that remains:
  OpenShell 0.0.77 mounts the gateway's client certificate and private key into every sandbox container (root-only;
  verified unreadable by the workload), and that key authenticates as the operator. E-7's "the wrapper injects its
  own credentials from 0600 files" was stale: the runner wrapper holds no key.
- **certifi 2026.7.22 → 2026.10.19** (backend lock, transitive) for PYSEC-2026-4320, which turned CI's dependency
  audit red from `86626c3` on. `pip-audit --strict`: no known vulnerabilities.

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
