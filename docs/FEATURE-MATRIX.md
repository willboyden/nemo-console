# Feature matrix

Two columns are kept apart on purpose.

**Status:** *Implemented* (code exists and was read), *Partial* (exists with a stated gap), *Not planned*.

**Verification:**

- **verified live**: exercised through the console against a real Switchyard 0.3.0, Relay 0.9.2 and Dynamo 1.5.0
  on one workstation. These checks were performed by the project maintainer on 2026-09-24, after the build;
  the documentation author was told the list and did **not** repeat them. Items the documentation author did run
  are marked **(re-run)** with what was run.
- **fakes only**: covered by offline tests with fake runners/HTTP and captured fixtures; never seen working on the
  real system.
- **unverified**: not exercised by tests or by a live run in a way that proves the claim.

Gates re-run for this revision on 2026-10-17 in the deployment checkout: backend `pytest -q` 470 passed /
2 skipped (the skip count depends on the environment: live smoke tests run only where the stack is up), `ruff check`
clean, `mypy --strict` clean (42 files), frontend `node --test` 96 passed. Earlier, on 2026-09-28:
backend 384 passed / 3 skipped in the author's deployment checkout, frontend 94 passed. From a clean copy of the
release tree outside that checkout (`make setup && make all`): 382 passed / 5 skipped, frontend 94 passed. Passing tests
prove behaviour against fakes, not against production. Fixtures under `backend/tests/*/fixtures` are captured output
from the real systems, which anchors the parsers but not the write paths.

**Live-run list (maintainer-reported, not repeated by me):** Switchyard config validate, stale-sha 409, and apply with
container recreate and health check; Dynamo preflight, `down` (and the confirm gate: 400 without confirm), `up`, then
serving again; playground chat returning 200 (Switchyard route); Relay `doctor_offline`, `gateway_status`,
`agents_list`, `plugins_validate` actions; auth behaviour (public 401, wrong key 401, bootstrap key -> admin); the
capable-tier-dark problem shown live; unauthenticated `POST /v1/stats/reset` -> 200 on Switchyard; status/metrics/events
read paths for all three; security headers/Host/Origin/CSRF/X-Forwarded-For; frontend rendered against the real
backend. **Also verified live (2026-09-24, later pass):** Relay `gateway_restart` (PID changed, three consecutive healthy checks, 400 without `confirm`),
the viewer role (reads 200; audit, validate, playground and every action 403; nothing executed), and a playground chat routed
through Switchyard `auto-local`. **Still not live:** config auto-rollback, Dynamo parameter apply, `stop_sharing_engine`,
Dynamo `restart`, assistive technology, browsers other than headless Firefox.

## Observe

| Feature | Status | Verification | Notes |
|---|---|---|---|
| Per-system status (health, components, endpoints, problems with a fix) | Implemented | Switchyard, Relay, Dynamo: verified live (read path) | Failure is data: a down system is HTTP 200 with `health=down`. Dynamo "loading" is `degraded` with a summary starting "loading". An opt-in system stopped on purpose is `health=off` (Dynamo with `dynamo_opt_in`, the default, and no container running or restarting: one info problem `dynamo.off`, fix `dynamo_start`); the overall `/health` ignores `off` (fakes only). |
| Metrics snapshot with canonical keys (`sy.*`, `relay.*`, `dyn.*`) | Implemented | verified live (read path, all three) | Keys absent when unreadable; nothing fabricated. `sy.route.calls` counts the newest 2000 routing records (labelled with the window), not a lifetime counter. |
| Metric history 15m / 1h / 24h | Implemented | fakes only for rollup logic; live polling ran with the server | 15 min ring in memory; 1 h and 24 h from 1 minute rollups (SQLite). Same-key series with different labels are **summed**. |
| Events list and SSE stream | Implemented | verified live (read path, all three); SSE caps fakes only; Relay `.gz` rotation read live read-only (2026-10-17: the real `relay-events.jsonl.2.gz`, 4 MiB tail, 417 events parsed) | Switchyard/Relay: metadata allowlists. Relay reads gzipped rotations with size caps; follow tails plain files only. **Dynamo: redacted log lines, not an allowlist (ADR-0010 deviation)**; a variant's real frontend (`dynamo-sv-frontend`) is a log source. The SSE slot is taken inside the generator. |
| Routing summary (by route, model, tier, tokens, errors) | Implemented | event read path verified live; aggregation fakes only | Derived from the last 5000 routing events; windows 15m/1h/24h/7d. |
| Topology | Partial | rendered against the real backend (verified live, before 2026-10-17); edge semantics unverified; the engine probes behind the new defaults were read live (2026-10-17, read-only: lightning running, Bolt / GLM / flashnext / 27B absent) | Edges are approximations, not observed traffic. The edge label "agent gateway" is partly clipped behind a node. Engine nodes come from the setting `topology_engines` (default: lightning, Bolt, GLM, flashnext, the 27B). Opt-in engines and a Dynamo that is `off` are drawn `off` (neutral), not red; an engine whose container does not exist is `off` and labelled "(not deployed)" (fakes only for the rendering); the clients -> Switchyard edge is labelled `default` (fakes only). |
| Switchyard capable-tier-dark detection | Partial | problem shown live (maintainer); logic also fakes (`test_capable_tier.py`); the not-deployed case read live 2026-10-17 (read-only probe against the real `routes.toml`: flashnext absent -> one info problem) | `switchyard.tier.capable_dark`: alias from `engine_probes` is the `capable_target` model of a `stage_router` route in the parsed `routes.toml` and its container exists but is not running per a shared, cached `docker inspect` (fix: the container's `engine_start` command, which says what it stops, else a compose-dir guess); Switchyard becomes `degraded` with summary prefix "capable tier dark". A container that does not exist is `switchyard.tier.capable_not_deployed` (info, health unchanged). Also the older `/v1/stats` error-count heuristic (`switchyard.capable_tier.upstream_errors`), which reads `calls` as SUCCESSES (the sum of calls and errors equalled `total_requests` on the running 0.3.0, read live 2026-10-06): it fires when errors >= successes (errors > 0) or errors rose since the last poll with no new success, so a target that never succeeded is flagged (fakes). No request-level probe; a running-but-broken engine is not detected; the default `engine_probes` and `engine_start` entries are deployment-specific. |
| Relay gateway health | Implemented | verified live (read path) | Any HTTP answer < 500 from the gateway's own `/health` = up (404 expected); upstream from LiteLLM's `/health/liveliness`; unit state from `systemctl --user show`. Never polls `/v1/models` (forwarded to LiteLLM, logged to Langfuse). |
| Relay policy findings (attestation, exporters, PII redaction) | Implemented | part of the Relay status read path (verified live) | Read-only analysis of `plugins.toml`. |
| Console self metrics (`/metrics`) and JSON logs | Implemented | fakes only (`test_console_metrics_endpoint`); JSON log lines seen in a local run | Viewer role. |
| OTLP export of console traces | Partial | unverified | Off by default; needs optional `opentelemetry` packages not in the lock file. |

## Configure

| Feature | Status | Verification | Notes |
|---|---|---|---|
| Switchyard `routes.toml`: validate (TOML + semantic + real `--dry-run` in a throwaway container) | Implemented | verified live | Authoritative. Requires the image locally. |
| Switchyard apply: CAS, backup, atomic write, recreate, health check | Implemented | verified live (apply with container recreate and health check) | `--no-build` recreate. Resets Switchyard's in-memory stats. |
| Stale `base_sha256` -> 409 | Implemented | verified live | |
| Auto-rollback when post-apply check fails | Implemented | **fakes only** (`test_auto_rollback_when_unhealthy`, `..._when_post_apply_raises`) | Not live. |
| Manual rollback to a stored revision, history, pruning | Implemented | fakes only (`test_manual_rollback_to_a_revision`, `test_rollback_validates_and_scans_the_target_revision`) | Rollback re-runs the validator and secret scan on the target revision; 422 before any write if it fails. |
| Relay `routes.toml` edit (validate + apply + gateway restart) | Implemented | fakes only | Validator is non-authoritative (mirror of another schema); the gateway restart is the real check. |
| Relay `plugins.toml` / `config.toml` | Implemented as **read-only by design** | fakes only (`test_post_apply_readonly_file_refused`, `test_read_only_file_is_403`) | Apply refused with 403 `read_only`. Not planned to become writable in v1. |
| Dynamo parameter catalog (14 params), preview of the rendered override | Implemented | preview rendering checked against the real compose (live smoke) | The preview response includes the adapter's `warnings` (the router calls `param_warnings`). |
| Dynamo parameter apply (write override, recreate worker, rollback on unhealthy) | Implemented | **unverified live** (fakes only); the refusal while a variant runs verified live read-only (2026-10-17: Super VL stack, `apply_params` refused, no override written) | Refused, with the files named, while the live stack runs from a repo override file (it would rewrite the base worker command over the variant's). |
| Dynamo compose files in use (from the containers' compose labels) | Implemented | read path verified live read-only (2026-10-17: the Super VL stack resolved to `docker-compose.yml` + `docker-compose.super-vl.yml`, both GPUs from the merged config, version `1.6.0-nemotron-3.5-super-vl-dev.1` instead of the nginx digest, the sv-frontend listed); up/down/restart with those files: fakes only | Label trust rules in ARCHITECTURE 9.6 and THREAT-MODEL T-9; an untrusted label refuses every lifecycle action. |
| Dynamo override / compose through the config API | Partial | fakes only | Backend exposes `compose` (read-only) and `override` (editable). The hand-edited override is a **closed allowlist**: no single-dash or unknown flags; `$` only in tokens byte-identical to the base compose; `--model`/`--served-model-name` equal the base; env keys must exist in the base and carry no `$` or secret-looking value; fail-closed if the base is unreadable. Also rejected: duplicate flags (normalised across `--x=v`, `_`/`-`, `--no-x`), `--speculative-config` other than exactly `{"method":"mtp","num_speculative_tokens":1..8}`, and path/JSON-looking values that differ from the base. **Gap:** enum-valued flags are not checked against catalog choices and numbers are not type-checked (admin-only, argv list). An absent override reads as empty and the first apply creates it (0600). The UI has **no config tab for Dynamo**. |
| Secret-shape refusal in config content | Implemented | fakes only | Applied on apply and on rollback targets. Pattern-based (R-8). Routes validators also error on non-loopback, non-single-label `base_url` hosts and on userinfo (`test_base_url_rejected`, `test_offbox_base_url_is_an_error_diagnostic`). |
| Settings (URLs, poll interval, credential env-var names) | Implemented | (re-run earlier: GET showed `[set, N chars]`; a non-loopback URL was rejected with 422) | URL changes need a restart. `state_dir` is shown `~`-relative to non-admins. |

## Act

| Feature | Status | Verification | Notes |
|---|---|---|---|
| Danger levels; `confirm:true` for disruptive/destructive; per-system busy lock | Implemented | confirm gate live for Dynamo `down` (400 without confirm); rest fakes only | Default action timeout 900 s, per-action overrides via `action_timeouts` (e.g. `dynamo.up`). Validate calls are capped by a semaphore of 2. |
| Switchyard `restart` (recreate), `dry_run_config` | Implemented | recreate verified live via config apply; `dry_run_config` fakes only | |
| Switchyard `reset_stats` | Implemented | unauthenticated `POST /v1/stats/reset` -> 200 live (maintainer); console action itself fakes only | Needs no credential; the former `switchyard.auth` problem code no longer exists. |
| Relay `gateway_status`, `doctor_offline`, `agents_list`, `plugins_validate` | Implemented | verified live (maintainer) | Output carries caveats: passing `plugins validate`/`doctor` does not prove the gateway starts. |
| Relay `gateway_restart` | Implemented | verified live (maintainer, 2026-09-24: PID 2024310 -> 2358531, 3 consecutive healthy checks, ~3 s) | Waits for several consecutive healthy checks and a stable PID. |
| Dynamo `preflight`, `down`, `up` | Implemented | verified live (through the console, then serving again; 2026-09-24, base stack); preflight against the Super VL stack verified live read-only (2026-10-17) | Compose-only, with the compose files in use. Preflight output includes machine-readable `shares_card:` and `router_aliases_dark:` lines, and (2026-10-06) `stoppable:`. Sharers are classified by the `dynamo_*` settings (exclusive / coexisting / ignored / unknown); `up` is refused while an exclusive or unknown container runs on the card, with `dynamo_start` as the fix for the default model (the classification is fakes only; device-request shapes were read live). |
| Dynamo `restart`, `up_mock` | Implemented | fakes only | Not live. |
| Dynamo `up` with `stop_sharing_engine=true` | Implemented | **unverified** (fakes only) | Stops only the opt-in 27B (`STOPPABLE_ENGINE`, name- and label-verified), never the default model: with that on the card `up` stays refused. |
| Dynamo card settings (`dynamo_opt_in`, `dynamo_start`, `default_model_start`, `dynamo_exclusive_engines`, `dynamo_coexisting_engines`, `dynamo_ignored_containers`, `dynamo_dark_while_held`) | Implemented | fakes only (`test_dynamo_policy.py`, `test_adapter.py`) | Environment only (`NEMO_CONSOLE_*`), not editable through `PUT /settings`. The commands are display only. Defaults name the author's containers. |

## Interact

| Feature | Status | Verification | Notes |
|---|---|---|---|
| Playground chat (streamed proxy, `max_tokens` clamp, server-side credential, not persisted) | Implemented | verified live: 200 through a Switchyard route (maintainer) | Admin only. Sends `Accept-Encoding: identity`. Other systems' playgrounds not live. Switchyard's default route is `super-lightning` when it exists, else the first route (fakes only). |
| "Served by" route/tier badges | Partial | unverified | Header names are guessed; **absent from real Switchyard responses**, so badges are normally empty. |
| Relay playground | Partial | unverified live; fakes (`test_relay_playground_sends_the_key_named_for_relay`) | `playground_key_env` maps `relay` to `LITELLM_API_KEY` by default (the gateway's `/v1/*` answers 401 without a bearer); default route `super-lightning` when present. |
| Dynamo playground | Implemented | unverified | Direct to the Dynamo frontend, bypassing the router and any output filtering. |

## Security and access

| Feature | Status | Verification | Notes |
|---|---|---|---|
| Security headers on all responses (CSP, nosniff, referrer, frame, COOP/CORP, no-store on `/api`) | Implemented | verified live; (re-run: headers present on `/api/v1/health`) | |
| `Host` allowlist (421) | Implemented | verified live; (re-run: forged Host -> 421) | |
| Origin check and `X-Console: 1` (403) | Implemented | verified live; (re-run: POST without header -> 403) | |
| `X-Forwarded-For` / `Forwarded` disables loopback trust; loopback trust off by default and forced off for non-loopback host or `allowed_hosts` | Implemented | header behaviour verified live; forced-off rule fakes only | `trust_loopback` defaults to false. |
| Bearer keys (hashed), first-start key file 0600, `mint-key` | Implemented | bootstrap key -> admin, wrong key 401, public 401 verified live (maintainer); (re-run earlier: 0600 files, path-only output) | Default: no key -> role `public`, 401 everywhere except `/health`. |
| Roles: admin | Implemented | verified live via the bootstrap key (maintainer) | |
| Roles: viewer | Implemented | verified live (maintainer, 2026-09-24: viewer key reads 200, audit/validate/playground/actions 403, nothing executed); tests: `test_bearer_keys_roles_and_wrong_key`, `test_viewer_may_only_run_viewer_role_actions` | |
| Every route has exactly one role dependency (enumerated test) | Implemented | fakes-only test of the real app | `test_every_route_carries_exactly_one_role_dependency` |
| Redaction of secrets in all outputs | Implemented | fakes only | |
| Append-only audit | Implemented | fakes only | Records authenticated, authorised mutating calls only. Denials are counted (`nemo_console_denied_requests_total{status}`), kept in a 100-entry ring and logged at most once a minute; never table rows. |
| Audit params blob is valid JSON by construction | Implemented | fakes only (`test_token_key_in_body_does_not_poison_audit`, `test_poisoned_row_does_not_break_list`, `test_audit_write_of_pathological_body_is_fast`) | Oversize becomes `{"_truncated":true,"bytes":n,"keys":[...]}`; unparseable legacy rows are tolerated on read; URL userinfo is redacted (`test_url_userinfo_redacted_in_audit`). |
| Redaction / secret scan bounds | Implemented | fakes only (`test_redact_and_find_secret_are_not_quadratic`) | Prefix `{0,40}`, 4000-character chunks, linear time. **A secret straddling a chunk boundary could slip through.** |
| Static file serving hardening | Implemented | fakes only (`test_static_rejects_dotdot_before_resolving`, `test_nul_in_static_path_is_404_not_500`, `test_static_allowlist_and_traversal`); verified live with `curl --path-as-is` (maintainer) | Rejects `..`, `//`, backslash, NUL, `.` segments, once/twice-decoded forms, before `resolve()`; the file must stay inside its own allowlisted directory. |
| Viewer sees config with literal secrets redacted | Implemented | fakes only (`test_viewer_gets_redacted_config_admin_gets_raw`) | Env-var names are kept; admins get raw content; `sha256` is always of the raw file. |
| Playground credential env-name allowlist | Implemented | fakes only (`test_playground_key_env_must_be_allowlisted`, `test_env_supplied_disallowed_key_name_is_ignored`) | Names set through the Settings API must be `LITELLM_*`, `NEMO_CONSOLE_PLAYGROUND_*` or `*_API_KEY`; a stored disallowed name is ignored. Names supplied by the operator's config file or environment (`NEMO_CONSOLE_PLAYGROUND_KEY_ENV`) are unrestricted. |
| Concurrency cap on validation | Implemented | fakes only (`test_parallel_validations_are_bounded`, `test_rollback_validation_takes_the_semaphore`) | One semaphore(2) covers validate, apply and rollback. |
| Switchyard routing events: identifier-shaped fields only | Implemented | fakes only (`test_routing_line_redacts_non_identifier_strings`, `test_routing_line_keeps_identifiers_and_empty_tier`) | `route`, `algorithm`, `model`, `tier`, `session_id` that are not identifier-shaped become `<redacted>`. |
| `/api/v1/health` `role` and `trust_notice` fields | Implemented | auth behaviour live (maintainer) | `role` is `admin`, `viewer` or `public`; `trust_notice` explains why loopback trust was forced off. |

## Frontend

| Feature | Status | Verification | Notes |
|---|---|---|---|
| Views: Overview, Switchyard, Relay, Dynamo, Audit (admin), Settings | Implemented | frontend rendered against the real backend: verified live (headless Firefox) | Dynamo tabs: Status, Params, Lifecycle, Playground. Switchyard: Routing, Config, Playground, Actions. Relay: Status, Config, Diagnostics, Playground (if offered). |
| Pure-logic unit tests | Implemented | 96 passed (re-run 2026-10-06) | Router, SSE parser, API client, formatting, diff, metrics, share, topology, param form, static guards. |
| Browsers other than headless Firefox | Implemented (untested) | **unverified** | |
| Assistive technology (screen readers, keyboard-only walk-through) | Partial | **unverified** | ARIA tabs, live regions, skip link, focus styles, reduced-motion CSS exist; none was tested with real AT. |
| Dark/light theme, English only | Implemented | unverified beyond the frontend render | Other locales: Not planned. |
| Mock server for UI development | Implemented | frontend test exists | Canned data only; default port 8796 (the real backend is 8793). |

## Desktop launcher (Linux)

| Feature | Status | Verification | Notes |
|---|---|---|---|
| `nemo-console open`: start in the background if needed, wait for health, open the UI | Implemented | verified live (maintainer, 2026-09-28, GNOME: a cold start answered healthy in under 1 s, a second launch reused the running console, the server ran with `-I` and a 0600 pid file); tests in `test_desktop.py` | Loopback only; a non-loopback host is refused. |
| Refuses a port it cannot trust | Implemented | verified live for a same-user non-console HTTP server on the port (refused, no browser); the other-user case is fakes only (`/proc/net/tcp` parsing verified live: `{uid}` for the running console) | THREAT-MODEL S-6. A same-user impersonator that copies the `/health` shape is not detected. |
| `nemo-console stop` and the menu's **Stop Nemo Console** action | Implemented | verified live (stopped the launcher's console, pid file removed; a second `stop` reports nothing recorded); with SSE streams open: verified live 2026-10-24 (server exit in 0.27 s, clients got `event: shutdown`, no error logged); the recycled-pid and other-user refusals are fakes only | Signals only the recorded pid, and only if its owner and command line match. On SIGTERM the server ends every SSE stream first; `shutdown_grace_s` (default 5 s, below `stop`'s 10 s) bounds the rest. |
| `make install-desktop` | Implemented | verified live (entry passes `desktop-file-validate`); unsafe checkout paths refused in a scratch test | Paths limited to `[A-Za-z0-9._/+-]`. |

## SkillEvaluator

| Feature | Status | Verification | Notes |
|---|---|---|---|
| Status: launcher, version, router liveness, skill count, last run | Implemented | verified live (maintainer, 2026-10-06: `skillevaluator-local` 0.5.0 found, router 200, 55 skills); fakes in `tests/skillevaluator/` | `--version` is cached 300 s so status polls do not spawn a process each time. |
| Evaluate one skill (`static`, `tier1`, `tier1+2`) and keep the report | Implemented | verified live (maintainer, 2026-10-06: `tier1+2` on one skill through the API, LLM judge and embeddings through LiteLLM, 10 s, report stored); fakes for argv, verdict-from-JSON, tier2 downgrade, no-report errors | The verdict is the report's `overall_status`; the CLI exits 1 for failed AND incomplete. |
| Static check of every skill | Implemented | fakes only | About 2 s per skill. |
| Stored runs as events, metrics, retention (200) | Implemented | events and metrics verified live (one run, then the page); retention fakes only | `run.json` holds metadata and redacted error text only. |
| SkillEvaluator page (status + Evaluate tabs) | Implemented | headless Firefox screenshots at 1440 and 600 px against the live backend (maintainer, 2026-10-06) | Reports open through the `report` action (Markdown as text). |
| Tier 3 (live agent evaluation, Skill Lift) | Implemented | fakes only (`tests/skillevaluator/test_skilleval_live.py`, result parsing against a captured result.json of 2026-10-17); the console's argv was accepted by the lab launcher's own parser (parse-only, its preflight stubbed, nothing started); status with the wrapper read live (55 skills, router up). Live run verified 2026-10-17 by calling the adapter in-process (`robust-scripts`, opencode on `nemotron-lightning`: succeeded, 2/2 attempts scored, Skill Lift +0.240 in `run.json`, a second request refused as busy). **Through the HTTP API and the UI: unverified** | Action `evaluate_live` (`disruptive`, admin, confirm): runs `skillevaluator-local tier3 ... --quiet` in the background, one at a time, `run.json` from `running` to the verdict with the lift per agent, attempts and feedback; `interrupted` after a console stop or restart. Plain `skillevaluator` is refused (its stock sandboxes run with `network_mode = "public"`). Runner cap 4800 s for this call only. |
| Tier 2 without embeddings | Implemented | fakes only | `skillevaluator.tier2.no_embeddings` with the lab fix `cd <repo>/inference/ollama && docker compose up -d`. |
| Install fix | Implemented | read live (2026-10-17: names the lab's pinned installer) | The lab's `clients/skillevaluator/install.sh` when present, else `uv tool install` pinned to `7304d76` (v0.5.0). |

## Data Designer

> Runner (2026-10-24): every script-executing invocation (validate/preview/create/check-models) runs
> inside an OpenShell sandbox — Landlock FS (run dir writable, baked tool config read-only), default-deny
> network (only the lab router's /v1/**), router key held by the OpenShell credential store and injected
> per-request. Proven live in the sandbox: validate + preview (2 records through the router on
> `glm-5.3-flash-next-local`) + create with a downloaded artifact tree.

| Feature | Status | Verification | Notes |
|---|---|---|---|
| Status: launcher version, router liveness, providers/aliases from the tool's yaml, one live create run | Implemented | verified live (maintainer, 2026-10-24: `data-designer-local` 0.9.3, router 200, 4 providers / 15 aliases parsed from the real `~/.data-designer`); fakes in `tests/datadesigner/` | `api_key` fields (env names) are never surfaced; `~/.data-designer` is never created by the console. |
| Config scripts as config files (edit/validate/diff/apply/history/rollback) | Implemented | script listing, spec, apply `post_check=healthy` and 409 stale-base verified live (2026-10-24); validate with the real CLI verified live (2026-10-24: ok + the temp-file extension fix); revision history/rollback fakes only | Validation EXECUTES the script locally (`load_config_builder()`); the UI states it. Temp copy keeps the script's extension (the CLI rejects anything else). |
| Preview action (sync, sample records + records.jsonl capped 2,000) | Implemented | verified live (2026-10-24: 2 records through the router on `glm-5.3-flash-next-local`, 6.6 s, report + sample_records HTML + records.jsonl kept) | Verdict from the CLI result; export failures carry the exporter's redacted stderr. |
| Create action (background, one at a time, converted copy under the datasets root) | Implemented | verified live (2026-10-24: 2 records, csv copy at `~/Datasets/smoke_reviews_v2/smoke_reviews_v2.csv`, records.jsonl stays jsonl, destination containment fakes) | `data-designer-local create` is the one long-timeout call (3,600 s); cancel on console stop → `interrupted` (fakes only). |
| Records/files endpoints + records table | Implemented | endpoints verified live (pagination, columns, files list); the browser records table verified against the dev mock only (headless Firefox screenshots) | `records.jsonl` is the only dataset artifact the API reads; 2 KiB per-cell cap, `truncated` flag. |
| Report / check_models / show_config actions | Implemented | report and show_config verified live (2026-10-24: report after a real run; show_config lists providers/aliases without key fields); check_models fakes only (the CLI requires a config_source argument — the action takes the script enum) | |
| Runner policy (allowlist, long timeout for create only) | Implemented | fakes + live (real CLI runs through the real runner during the live smoke) | `data-designer-local` joins the SIGINT-first graceful set; `data-designer-export` is the conversion wrapper. |
| Data Designer page (status, scripts, actions, runs tabs) | Implemented | headless Firefox screenshots at 1440 and 884 px against the live backend (2026-10-24: status/actions/runs with a real run); run-detail records table mock only | Deep-linked tabs (`?tab=`); SSE live-run banner. |

## OpenShell (addendum 9.8)

| Feature | Status | Verification | Notes |
|---|---|---|---|
| Status: CLI version, gateway + forwarder containers, connection, `openshell-docker` internal check, exposed-port check, version skew | Implemented | verified live (2026-10-24: CLI 0.0.71, gateway 0.0.77 connected, network internal, one info problem `openshell.version.skew`); stopped/crashed/disconnected/not-internal paths fakes only | A stopped gateway is `off` (started on demand by `data-designer-local`), not `down`. `openshell -g openshell` pins the lab registration (the CLI also has stale `nemoclaw-*` ones). |
| Metrics `os.*`, sandbox events | Implemented | metrics verified live (no sandboxes at the time); events fakes only | Phase names other than `Ready` are from memory; unknown phases land in `Other`. |
| Config files (gateway.toml, policy.yaml, provider profile, compose) | Implemented **read-only by design** (ADR 0007) | read verified live | A web console never loosens a sandbox trust policy. |
| Actions `doctor`, `list_sandboxes`, `sandbox_get`, `sandbox_logs`, `gateway_logs`, `prove_policy` | Implemented | verified live (2026-10-24) except `sandbox_get`/`sandbox_logs` on a live sandbox (refused-name path live; real output captured from a throwaway sandbox) | `prove_policy` derives the credential descriptor from the provider profile (no key read); findings are for human review (the scopes vocabulary is undocumented). |
| Actions `gateway_up`, `gateway_stop` (disruptive), `delete_sandbox` (destructive) | Implemented | **fakes only** | `gateway_up` mirrors `data-designer-local` (refuses unless the network is internal, re-pins the router alias); `gateway_stop` refuses while sandboxes exist unless `force`; never `down`/`-v`. |

## DCGM (addendum 9.8)

| Feature | Status | Verification | Notes |
|---|---|---|---|
| Status: exporter container + version, `/metrics`, vulnerable-version and `/debug/pprof` checks, every DCGM host Prometheus scrapes | Implemented | verified live (2026-10-24: exporter 4.8.4, pprof 404, 6/6 hosts up); vulnerable/pprof-200/host-down paths fakes only | `dcgm.exporter.vulnerable` (< 4.8.3, CVE-2026-47483). Remote exporters' versions are not visible through Prometheus and are not guessed. |
| Metrics per GPU and per host (`dcgm.*`) | Implemented | verified live (50 points) and charted from the real history API | Ratios and bytes, never UUIDs or serials. Label values are slugs (`RTX-PRO-6000-Blackwell-Workstation`): the history API's series keys allow no spaces, which made every per-GPU chart empty until fixed (test `test_every_metric_point_forms_a_history_key_the_api_accepts`). `dcgm.xid_last` is the exporter's LAST XID code, not a count. |
| `counters.csv` edit (validate, apply, rollback) | Implemented | validate verified live (live file ok; a broken candidate rejected); `post_apply(restart=True)` verified live on the unchanged file (healthy, 34 s); a real edit applied without restart: **unverified** | Validator `authoritative=False` (field names from the exporter image's lists; an unlisted name with no close match is a warning). The exporter re-reads the file (directory mount). |
| Actions `discovery` (safe), `diag` (level 1), `diag_level2`, `restart_exporter` (all disruptive) | Implemented | `discovery` and `diag` level 1 on GPU 0 verified live with the hardened flags (2026-10-24: PASS 2.8 s) and through the console API; `restart_exporter` exercised live via `post_apply` before the identity check was added (the check itself: fakes + one live read); level 2 **unverified** | Throwaway container from the full DCGM image: `--network none --pull never --cap-drop ALL --cap-add SYS_ADMIN --read-only --tmpfs /tmp --pids-limit 256 --memory 2g`, no-new-privileges, one GPU index, constant command (no `/run` tmpfs: it hides the persistence daemon's socket and fails level 1). `docker restart` only after the container's compose labels match the observability project. Serials stripped before storage. |

## AIPerf (addendum 9.8)

| Feature | Status | Verification | Notes |
|---|---|---|---|
| Status: wrapper version, router, key file, presets, newest run | Implemented | verified live (2026-10-24: 0.13.0) | Lab install `clients/aiperf/` (pinned to the scanned commit, venv-local; only `aiperf-local` on PATH). |
| `profile` action (background, one at a time) and `report` | Implemented | verified live against `aitop/nemotron-lightning` (smoke preset; TTFT p50 25–34 ms; a second start refused; a bad model reported with the router's 400 text; console cancel → `interrupted`) | Disruptive: loads a live engine. The `aiperf` router key has `max_parallel_requests` 64, `rpm_limit` 2200, `tpm_limit` 600000. `aiperf-local` accepts an exact-token flag allowlist. Tokenizer `builtin`: tiktoken o200k VENDORED in the lab repo (sha256-pinned, installed into the cache by the wrapper before every run — never downloaded); prompt sizes are o200k tokens, so compare runs with each other. |
| Presets `profiles.yaml` edit | Implemented | validate fakes + live read | Own schema, authoritative; nothing to restart. |

## NeMo Guardrails (addendum 9.8)

| Feature | Status | Verification | Notes |
|---|---|---|---|
| Status: server, gateway and router-broker containers, `/v1/health`, served configs, isolation checks | Implemented | verified live (2026-10-24: 0.24.1, 1 config); isolation problems fakes only | Error problem if the server ever joins a non-internal network or publishes a port. |
| Rails config files edit (validate in a `--network none` container, restart, served-check, rollback) | Implemented | validate verified live (good + three broken copies + a weakening candidate refused); `post_apply(restart=True)` healthy live; rollback paths fakes only; apply through the console HTTP API: **unverified** | ADR 0015: a candidate that removes rails is refused unless it carries `# nemo-console: allow-weaken-rails`. Python files are read-only. A load check does not call the LLM: run `check` after an edit. |
| `check` action (fixed benign/jailbreak/harmful probes via `/v1/checks`) | Implemented | verified live (benign allowed, jailbreak and harmful blocked by `self check input`) | Disruptive: spends router tokens. Events carry names and verdicts, never prompt text. |
| Playground (`openai_chat`, model = the rails' main model, the server's default config) | Implemented | the same body replayed by hand live (benign answered, jailbreak refused); through the core's playground: **unverified** | The server has no auth on `127.0.0.1:8806`. |
| `up` / `restart` / `stop` | Implemented | `restart` live via `post_apply`; others fakes only | Compose; never `down`/`-v`. |

## NeMo Evaluator (addendum 9.8)

| Feature | Status | Verification | Notes |
|---|---|---|---|
| Status: wrapper version, router, key file, suites, cached datasets | Implemented | verified live (2026-10-24: 0.3.0, 3 suites, gsm8k + mmlu cached) | Lab install `clients/nemo-evaluator/` (scanned commit, constraints file). |
| `run` action (offline, background, one at a time) and `report` | Implemented | verified live (smoke suite: GSM8K 10/10 on Lightning; MMLU 50 problems 0.48–0.50; second start refused; SIGINT → `interrupted`); strace of a full run (re-run 2026-10-24 after the cache change): 22 connects, all 127.0.0.1 | Offline by library switches, not a kernel block. Only rule-scored benchmarks (no Docker sandbox, no judge). Each request carries LiteLLM's `cache: {no-cache: true}` (a generated per-benchmark config), so repeats are fresh generations. Key limits: 32 parallel, 1200 rpm, 600000 tpm. |
| Suites `suites.yaml` edit | Implemented | validate verified live (shipped file ok; a broken candidate rejected with 4 line-numbered errors) | Own schema plus the wrapper's own `suites --file` as a second opinion. |
| Dataset prefetch | Lab only (`evaluator-local prefetch`) | live (gsm8k, mmlu) | The only step with internet; deliberately not a console action. |

## Not planned for v1

Editing Relay `plugins.toml` / `config.toml`; multi-user or per-user audit identity beyond the key
id; key revocation UI; TLS; remote (non-loopback) deployment; localisation; active probing of the Switchyard
capable tier.
